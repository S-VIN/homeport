#!/bin/sh
# All substitutions are shell-quoted or validated integers. Never enable xtrace.
set -eu
umask 077
DEST=@DIRECTORY@
STAGE=@STAGE@
PANEL=@PANEL@
[ "$(id -u)" = 0 ] || { echo 'Root or sudo access is required.' >&2; exit 1; }
[ "$(uname -s)" = Linux ] || { echo 'Only Linux servers are supported.' >&2; exit 1; }
case "$(uname -m)" in x86_64|aarch64|arm64) ;; *) echo 'The image requires amd64 or arm64.' >&2; exit 1;; esac
[ ! -e "$DEST" ] || { echo "Installation directory already exists: $DEST. Choose a new directory; existing data will not be overwritten." >&2; exit 1; }
for port in @PORTS@; do
  hex=$(printf '%04X' "$port")
  if awk -v p=":$hex" '$2 ~ p"$" && $4 == "0A" { found=1 } END { exit !found }' /proc/net/tcp /proc/net/tcp6; then
    echo "TCP port $port is already in use. Choose other ports or stop the conflicting service." >&2; exit 1
  fi
done
. /etc/os-release
fetch() { if command -v curl >/dev/null; then curl -fLsS --connect-timeout 20 --max-time 300 "$1" -o "$2"; else wget -qO "$2" "$1"; fi; }
if ! command -v curl >/dev/null; then
  if command -v apt-get >/dev/null; then apt-get update; DEBIAN_FRONTEND=noninteractive apt-get install -y curl ca-certificates
  elif command -v apk >/dev/null; then apk add curl ca-certificates
  elif command -v dnf >/dev/null; then dnf install -y curl ca-certificates
  elif command -v yum >/dev/null; then yum install -y curl ca-certificates
  elif command -v zypper >/dev/null; then zypper --non-interactive install curl ca-certificates
  elif command -v pacman >/dev/null; then pacman -Sy --noconfirm curl ca-certificates
  else echo 'Install curl and Docker Compose for this distribution, then retry.' >&2; exit 1; fi
fi
if ! command -v docker >/dev/null; then
  case "$ID" in
    alpine) apk add docker docker-cli-compose; rc-update add docker default; service docker start ;;
    arch|manjaro) pacman -Sy --noconfirm docker docker-compose ;;
    opensuse*|sles) zypper --non-interactive install docker docker-compose ;;
    *) fetch https://get.docker.com "$STAGE/get-docker.sh"; sh "$STAGE/get-docker.sh" ;;
  esac
fi
if command -v systemctl >/dev/null; then systemctl enable --now docker; fi
docker info >/dev/null
if ! docker compose version >/dev/null 2>&1; then
  case "$ID" in
    ubuntu|debian|raspbian) apt-get update; DEBIAN_FRONTEND=noninteractive apt-get install -y docker-compose-plugin || DEBIAN_FRONTEND=noninteractive apt-get install -y docker-compose-v2 ;;
    alpine) apk add docker-cli-compose ;;
    arch|manjaro) pacman -S --noconfirm docker-compose ;;
    opensuse*|sles) zypper --non-interactive install docker-compose ;;
    *) if command -v dnf >/dev/null; then dnf install -y docker-compose-plugin; fi ;;
  esac
fi
docker compose version >/dev/null || { echo 'Docker exists but Compose v2 is missing. Install the docker-compose-plugin package and retry.' >&2; exit 1; }
# No firewall flush or policy changes: report reachability failures instead.
mkdir -p "$(dirname "$DEST")"
mkdir "$DEST"
cp "$STAGE/compose.yaml" "$STAGE/Caddyfile" "$STAGE/.env" "$DEST/"
mkdir -p "$DEST/data/rproxy"
cp "$STAGE/seed.sqlite3" "$DEST/data/rproxy/rproxy-plugins.sqlite3"
cd "$DEST"
# Fixed project name is scoped to this installation using a stable directory hash.
export COMPOSE_PROJECT_NAME="rproxy-$(printf '%s' "$DEST" | cksum | awk '{print $1}')"
printf '%s\n' "COMPOSE_PROJECT_NAME=$COMPOSE_PROJECT_NAME" >> .env
fail() { echo "Deployment failed. Files are preserved in $DEST. Inspect: cd $DEST && docker compose logs --tail=100" >&2; }
trap fail EXIT
docker compose pull
docker compose up -d --wait --wait-timeout 180
attempt=0
ready() {
  code=$(curl -sS -o /dev/null -w '%{http_code}' --connect-timeout 10 --max-time 20 "$PANEL") || return 1
  # The management endpoint must reject unauthenticated API clients.
  [ "$code" = 401 ] || [ "$code" = 302 ] || [ "$code" = 303 ] || return 1
  curl -fLsS --connect-timeout 10 --max-time 20 "https://auth.${PANEL#https://panel.}" >/dev/null
}
until ready; do
  attempt=$((attempt + 1))
  [ "$attempt" -lt 24 ] || { echo 'HTTPS is not ready. Check DNS and inbound TCP 80 / HTTPS; ACME may be rate-limited.' >&2; exit 1; }
  sleep 5
done
hex=$(printf '%04X' @PROXY_PORT@)
awk -v p=":$hex" '$2 ~ p"$" && $4 == "0A" { found=1 } END { exit !found }' /proc/net/tcp /proc/net/tcp6 || {
  echo 'VLESS listener did not start. Check the rproxy core logs in the panel.' >&2; exit 1;
}
trap - EXIT
echo 'rproxy deployment ready'
