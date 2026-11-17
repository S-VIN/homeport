# Installs the rproxy Compose stack. Runs as root; the client puts the settings
# in front: DEST STAGE PANEL AUTH HTTPS_PORT DISABLE_IPV6.
# The client reads "::step::" (progress) and "::error::" (the reason) lines.
# Never enable xtrace: .env holds the panel password.
set -eu
umask 077
IPV6_CONF=/etc/sysctl.d/99-rproxy-disable-ipv6.conf

step() { echo "::step::$*"; }
die() { echo "::error::$*"; exit 1; }
listening() {
  cat /proc/net/tcp /proc/net/tcp6 2>/dev/null |
    awk -v p=":$(printf '%04X' "$1")" '$2 ~ p"$" && $4 == "0A" { found = 1 } END { exit !found }'
}
# The first free port from $1 on, for the panel's loopback services.
free_port() {
  port=$1
  while listening "$port" || [ "$port" = "$HTTPS_PORT" ]; do
    port=$((port + 1))
  done
  echo "$port"
}

# A failed installation leaves nothing behind, so it can simply be repeated.
CREATED=
IPV6_OFF=
cleanup() {
  [ "$?" != 0 ] || return 0
  if [ -n "$CREATED" ]; then
    (cd "$DEST" && docker compose logs --tail=20 && docker compose down --volumes --remove-orphans) 2>&1 || true
    rm -rf "$DEST"
  fi
  if [ -n "$IPV6_OFF" ]; then
    rm -f "$IPV6_CONF"
    sysctl -q -w net.ipv6.conf.all.disable_ipv6=0 net.ipv6.conf.default.disable_ipv6=0 || true
  fi
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM PIPE

step 'Checking the server'
[ "$(id -u)" = 0 ] || die 'Root or sudo access is required.'
[ "$(uname -s)" = Linux ] || die 'Only Linux servers are supported.'
case "$(uname -m)" in
  x86_64 | aarch64 | arm64) ;;
  *) die 'The server must be amd64 or arm64.' ;;
esac
[ ! -e "$DEST" ] || die "The folder $DEST already exists on the server. Remove it or choose another folder."
for port in 80 "$HTTPS_PORT"; do
  if listening "$port"; then die "TCP port $port is already in use on the server."; fi
done

. /etc/os-release
if ! command -v curl >/dev/null; then
  step 'Installing curl'
  if command -v apt-get >/dev/null; then apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y curl ca-certificates
  elif command -v apk >/dev/null; then apk add curl ca-certificates
  elif command -v dnf >/dev/null; then dnf install -y curl ca-certificates
  elif command -v yum >/dev/null; then yum install -y curl ca-certificates
  elif command -v zypper >/dev/null; then zypper --non-interactive install curl ca-certificates
  elif command -v pacman >/dev/null; then pacman -Sy --noconfirm curl ca-certificates
  else die 'Install curl and Docker Compose on the server, then try again.'; fi
fi
if ! command -v docker >/dev/null; then
  step 'Installing Docker'
  case "$ID" in
    alpine) apk add docker docker-cli-compose && rc-update add docker default && service docker start ;;
    arch | manjaro) pacman -Sy --noconfirm docker docker-compose ;;
    opensuse* | sles) zypper --non-interactive install docker docker-compose ;;
    *) curl -fLsS --connect-timeout 20 --max-time 300 https://get.docker.com -o "$STAGE/get-docker.sh" && sh "$STAGE/get-docker.sh" ;;
  esac
fi
if command -v systemctl >/dev/null; then systemctl enable --now docker; fi
docker info >/dev/null || die 'Docker is installed but does not run on the server.'
if ! docker compose version >/dev/null 2>&1; then
  step 'Installing Docker Compose'
  case "$ID" in
    ubuntu | debian | raspbian) apt-get update && { DEBIAN_FRONTEND=noninteractive apt-get install -y docker-compose-plugin || DEBIAN_FRONTEND=noninteractive apt-get install -y docker-compose-v2; } ;;
    alpine) apk add docker-cli-compose ;;
    arch | manjaro) pacman -S --noconfirm docker-compose ;;
    opensuse* | sles) zypper --non-interactive install docker-compose ;;
    *) if command -v dnf >/dev/null; then dnf install -y docker-compose-plugin; fi ;;
  esac
  docker compose version >/dev/null || die 'Docker Compose v2 is missing. Install the docker-compose-plugin package on the server, then try again.'
fi

if [ "$DISABLE_IPV6" = 1 ] && [ -d /proc/sys/net/ipv6 ]; then
  step 'Turning IPv6 off'
  mkdir -p /etc/sysctl.d
  printf 'net.ipv6.conf.all.disable_ipv6 = 1\nnet.ipv6.conf.default.disable_ipv6 = 1\n' >"$IPV6_CONF"
  IPV6_OFF=1
  sysctl -q -w net.ipv6.conf.all.disable_ipv6=1 net.ipv6.conf.default.disable_ipv6=1
fi

step 'Starting rproxy'
mkdir -p "$(dirname "$DEST")"
mkdir "$DEST"
CREATED=1
cp "$STAGE/compose.yaml" "$STAGE/Caddyfile" "$STAGE/.env" "$DEST/"
mkdir -p "$DEST/data/rproxy"
cp "$STAGE/profile.sqlite3" "$DEST/data/rproxy/rproxy-plugins.sqlite3"
cd "$DEST"
ui_port=$(free_port 20800)
{
  echo "RPROXY_UI_PORT=$ui_port"
  echo "TINYAUTH_PORT=$(free_port $((ui_port + 1)))"
  # One project per folder: two installations on a server do not collide.
  echo "COMPOSE_PROJECT_NAME=rproxy-$(printf '%s' "$DEST" | cksum | awk '{print $1}')"
} >>.env
docker compose pull --quiet
docker compose up -d --wait --wait-timeout 180

step 'Waiting for the HTTPS certificate'
ready() {
  # The panel must turn away visitors who are not signed in.
  code=$(curl -sS -o /dev/null -w '%{http_code}' --connect-timeout 10 --max-time 20 "$PANEL") || return 1
  case "$code" in 401 | 302 | 303) ;; *) return 1 ;; esac
  curl -fLsS --connect-timeout 10 --max-time 20 "$AUTH" >/dev/null
}
attempt=0
until ready; do
  attempt=$((attempt + 1))
  [ "$attempt" -lt 24 ] || die "HTTPS did not start. Open TCP ports 80 and $HTTPS_PORT in the server's firewall and check that the domain points to the server."
  sleep 5
done
