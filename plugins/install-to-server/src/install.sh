# Installs or updates the homeport Compose stack. Runs as root; the client puts
# the settings in front: DEST STAGE HOME_DIR PANEL AUTH HTTPS_PORT DISABLE_IPV6
# (HOME_DIR: the home folder of the SSH login).
# The client reads "::step::" (progress), "::error::" (the reason) and
# "::created::" (the installation folder exists) lines; the rest is shown as it is.
# Never enable xtrace: .env holds the panel password.
#
# Every run starts from what is there: an earlier installation or a failed
# attempt is stopped and updated in place, its data folder kept. A failure
# leaves things as they are, so the next run can finish the job.
#
# What the server did not have before is listed in $MANIFEST, so that
# uninstall.sh removes exactly that: packages, repositories, images, folders.
set -eu
umask 077
# One sort order for comm.
export LC_ALL=C
IPV6_CONF=/etc/sysctl.d/99-homeport-disable-ipv6.conf
# In the home folder of the login that installed first; any login finds it.
MANIFEST=
for f in "$HOME_DIR/.homeport-install/added" /root/.homeport-install/added /home/*/.homeport-install/added; do
  if [ -f "$f" ]; then MANIFEST=$f; break; fi
done
MANIFEST=${MANIFEST:-$HOME_DIR/.homeport-install/added}
# Every installation is this one Compose project, whatever its folder.
PROJECT=homeport

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
# What holds a port, for the error message.
holder() {
  if command -v ss >/dev/null; then ss -Hltnp "sport = :$1" 2>/dev/null | grep -o 'users:(("[^"]*"' | head -n 1 | cut -d '"' -f 2; fi
}

# One manifest line, once.
added() {
  grep -qxF "$*" "$MANIFEST" || echo "$*" >>"$MANIFEST"
}
# Installed packages: the user's choices on Alpine (their dependencies go with them).
packages() {
  if command -v dpkg-query >/dev/null; then dpkg-query -W -f '${Status} ${Package}\n' | awk '$3 == "installed" { print $4 }'
  elif command -v apk >/dev/null; then cat /etc/apk/world
  elif command -v pacman >/dev/null; then pacman -Qq
  elif command -v rpm >/dev/null; then rpm -qa --qf '%{NAME}\n'
  fi | sort -u
}
# Package sources and their keys, as the Docker install script adds them.
repositories() {
  for d in /etc/apt/sources.list.d /etc/apt/keyrings /usr/share/keyrings /etc/yum.repos.d /etc/zypp/repos.d; do
    if [ -d "$d" ]; then find "$d" -maxdepth 1 -type f; fi
  done | sort -u
}
# What the package steps added since `before`; once per run. A kernel update
# that happened to land meanwhile is not ours.
remember() {
  [ -f "$STAGE/packages" ] || return 0
  packages | comm -13 "$STAGE/packages" - | grep -Ev '^(linux|kernel)-' | while read -r p; do added package "$p"; done
  repositories | comm -13 "$STAGE/repositories" - | while read -r f; do added file "$f"; done
  rm -f "$STAGE/packages"
}
before() {
  packages >"$STAGE/packages"
  repositories >"$STAGE/repositories"
}

STARTED=
failed() {
  code=$?
  remember || true
  [ "$code" != 0 ] || return 0
  if [ -n "$STARTED" ]; then
    echo 'Last lines of the services:'
    (cd "$DEST" && docker compose logs --no-color --tail=30) 2>&1 || true
  fi
}
trap failed EXIT
trap 'exit 1' HUP INT TERM PIPE

step 'Checking the server'
[ "$(id -u)" = 0 ] || die 'Root or sudo access is required.'
[ "$(uname -s)" = Linux ] || die 'Only Linux servers are supported.'
case "$(uname -m)" in
  x86_64 | aarch64 | arm64) ;;
  *) die 'The server must be amd64 or arm64.' ;;
esac
. /etc/os-release
echo "${PRETTY_NAME:-Linux} on $(uname -m)"
mkdir -p "${MANIFEST%/*}"
touch "$MANIFEST"
before
if [ -f "$DEST/compose.yaml" ]; then echo "Found an earlier installation in $DEST: updating it."; fi

# Earlier runs, also in another folder: stopped so their ports are free.
if command -v docker >/dev/null && docker info >/dev/null 2>&1; then
  projects=$(docker ps -a --format '{{.Label "com.docker.compose.project"}}' | grep "^$PROJECT" | sort -u || true)
  if [ -n "$projects" ]; then
    step 'Stopping the earlier installation'
    for p in $projects; do
      docker ps -aq --filter "label=com.docker.compose.project=$p" | xargs -r docker rm -f
    done
  fi
fi
for port in 80 "$HTTPS_PORT"; do
  if listening "$port"; then
    by=$(holder "$port")
    die "TCP port $port is already in use on the server${by:+ by $by}. Stop that service or choose another panel port."
  fi
done

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
  # Uninstalling also removes Docker's data then: nothing but this used it.
  added docker
  case "$ID" in
    alpine) apk add docker docker-cli-compose && rc-update add docker default && service docker start ;;
    arch | manjaro) pacman -Sy --noconfirm docker docker-compose ;;
    opensuse* | sles) zypper --non-interactive install docker docker-compose ;;
    *) curl -fLsS --connect-timeout 20 --max-time 300 https://get.docker.com -o "$STAGE/get-docker.sh" && sh "$STAGE/get-docker.sh" ;;
  esac
fi
remember
if command -v systemctl >/dev/null; then
  if [ "$(systemctl is-enabled docker 2>/dev/null)" != enabled ]; then added service docker; fi
  systemctl enable --now docker
fi
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
docker --version
docker compose version

if [ -d /proc/sys/net/ipv6 ]; then
  if [ "$DISABLE_IPV6" = 1 ]; then
    step 'Turning IPv6 off'
    mkdir -p /etc/sysctl.d
    printf 'net.ipv6.conf.all.disable_ipv6 = 1\nnet.ipv6.conf.default.disable_ipv6 = 1\n' >"$IPV6_CONF"
    sysctl -w net.ipv6.conf.all.disable_ipv6=1 net.ipv6.conf.default.disable_ipv6=1
  elif [ -f "$IPV6_CONF" ]; then
    step 'Turning IPv6 back on'
    rm -f "$IPV6_CONF"
    sysctl -w net.ipv6.conf.all.disable_ipv6=0 net.ipv6.conf.default.disable_ipv6=0
  fi
fi

step 'Starting homeport'
echo '::created::'
# The folder and the topmost one created for it ("-": it was there).
if ! grep -q "^folder $DEST " "$MANIFEST"; then
  created=- d=$DEST
  while [ ! -e "$d" ]; do created=$d d=$(dirname "$d"); done
  added folder "$DEST" "$created"
fi
mkdir -p "$DEST"
cp "$STAGE/compose.yaml" "$STAGE/Caddyfile" "$STAGE/.env" "$DEST/"
cd "$DEST"
ui_port=$(free_port 20800)
{
  echo "HOMEPORT_UI_PORT=$ui_port"
  echo "TINYAUTH_PORT=$(free_port $((ui_port + 1)))"
  echo "COMPOSE_PROJECT_NAME=$PROJECT"
} >>.env
STARTED=1
# Images the server did not have are removed with the installation, also
# the versions an update replaced.
images=$(docker compose config --images)
for image in $images; do
  if ! docker image inspect "$image" >/dev/null 2>&1; then added image "$image"; fi
done
docker compose pull 2>&1
for image in $images; do
  if grep -qxF "image $image" "$MANIFEST"; then added image "$(docker image inspect -f '{{.Id}}' "$image")"; fi
done
docker compose up -d --wait --wait-timeout 180 2>&1

step 'Waiting for the HTTPS certificate'
ready() {
  # The panel must turn away visitors who are not signed in.
  code=$(curl -sS -o /dev/null -w '%{http_code}' --connect-timeout 10 --max-time 20 "$PANEL") || return 1
  case "$code" in 401 | 302 | 303) ;; *) return 1 ;; esac
  curl -fLsS --connect-timeout 10 --max-time 20 "$AUTH" >/dev/null
}
# Let's Encrypt refuses new certificates for a name for a while: 5 a week for
# the same names (installing again after uninstalling), and a weekly limit
# for the whole domain (sslip.io's is shared by everyone who uses it).
rate_limited() {
  docker compose logs --no-color caddy 2>/dev/null | grep 'rateLimited' | tail -n 1
}
attempt=0
until ready; do
  limit=$(rate_limited)
  if [ -n "$limit" ]; then
    after=$(printf '%s\n' "$limit" | grep -oE 'retry after [0-9][0-9:T -]*(UTC|Z)' | tail -n 1 | sed 's/^retry after //')
    die "Let's Encrypt has issued as many certificates for this domain as it allows for now${after:+. Try again after $after}, or enter another domain."
  fi
  attempt=$((attempt + 1))
  [ "$attempt" -lt 24 ] || die "HTTPS did not start. Open TCP ports 80 and $HTTPS_PORT in the server's firewall and check that the domain points to the server."
  [ $((attempt % 4)) != 0 ] || echo "Still waiting for $PANEL…"
  sleep 5
done
echo "The panel answers at $PANEL"
