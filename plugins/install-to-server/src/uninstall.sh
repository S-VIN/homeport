# Removes homeport and what install.sh added to the server, as its manifest
# lists it, and the panel file; what the server had before stays. Runs as
# root; the client puts DEST (the installation it knows, also without a
# manifest), STAGE and HOME_DIR (the home folder of the SSH login) in front.
# The client reads "::step::" and "::error::" lines like install.sh's.
#
# Every step may run again: a failed or interrupted run is finished by the next.
set -eu
export LC_ALL=C
IPV6_CONF=/etc/sysctl.d/99-homeport-disable-ipv6.conf
# In the home folder of the login that installed first; any login finds it.
MANIFEST=
for f in "$HOME_DIR/.homeport-install/added" /root/.homeport-install/added /home/*/.homeport-install/added; do
  if [ -f "$f" ]; then MANIFEST=$f; break; fi
done
PROJECT=homeport

step() { echo "::step::$*"; }
die() { echo "::error::$*"; exit 1; }
trap 'exit 1' HUP INT TERM PIPE
# Values of one kind of manifest line.
listed() {
  if [ -f "$MANIFEST" ]; then sed -n "s/^$1 //p" "$MANIFEST"; fi
}
ours() {
  [ -f "$MANIFEST" ] && grep -qxF "$1" "$MANIFEST"
}
packages() {
  if command -v dpkg-query >/dev/null; then dpkg-query -W -f '${Status} ${Package}\n' | awk '$3 == "installed" { print $4 }'
  elif command -v apk >/dev/null; then cat /etc/apk/world
  elif command -v pacman >/dev/null; then pacman -Qq
  elif command -v rpm >/dev/null; then rpm -qa --qf '%{NAME}\n'
  fi | sort -u
}

[ "$(id -u)" = 0 ] || die 'Root or sudo access is required.'
[ -n "$MANIFEST" ] || echo "The server has no list of what the installer added: removing the installation in $DEST only."

if command -v docker >/dev/null && docker info >/dev/null 2>&1; then
  step 'Removing the homeport containers'
  projects=$(docker ps -a --format '{{.Label "com.docker.compose.project"}}' | grep "^$PROJECT" | sort -u || true)
  for p in $projects; do
    docker ps -aq --filter "label=com.docker.compose.project=$p" | xargs -r docker rm -f
    docker network ls -q --filter "label=com.docker.compose.project=$p" | xargs -r docker network rm || true
  done
  # Names first, then the versions an update replaced.
  for image in $(listed image); do
    docker rmi "$image" >/dev/null 2>&1 || true
  done
fi

step 'Removing the installation folder'
# Only the installer's files: the folder may hold other things.
{ listed folder; echo "$DEST -"; } | while read -r folder created; do
  [ -n "$folder" ] && [ "$folder" != / ] || continue
  if [ -e "$folder/compose.yaml" ] || [ -e "$folder/data" ]; then echo "Removing $folder"; fi
  rm -rf -- "$folder/compose.yaml" "$folder/Caddyfile" "$folder/.env" "$folder/data"
  [ "$created" != - ] || continue
  # Empty folders up to the topmost the installer created.
  d=$folder
  while rmdir -- "$d" 2>/dev/null && [ "$d" != "$created" ]; do d=$(dirname "$d"); done
done
rm -f -- "$HOME_DIR/homeport-panel.txt"
for d in /tmp/homeport-install.*; do
  if [ "$d" != "$STAGE" ] && [ -d "$d" ]; then rm -rf -- "$d"; fi
done

if [ -f "$IPV6_CONF" ]; then
  step 'Turning IPv6 back on'
  rm -f "$IPV6_CONF"
  sysctl -w net.ipv6.conf.all.disable_ipv6=0 net.ipv6.conf.default.disable_ipv6=0 || true
fi

if ours docker; then
  step 'Stopping Docker'
  if command -v systemctl >/dev/null; then
    systemctl disable --now docker.socket docker.service containerd.service 2>/dev/null || true
  fi
  if command -v rc-update >/dev/null; then
    rc-service docker stop 2>/dev/null || true
    rc-update del docker default 2>/dev/null || true
  fi
elif ours 'service docker' && command -v systemctl >/dev/null; then
  # Docker was there but off: off again.
  step 'Turning Docker off again'
  systemctl disable --now docker.socket docker.service 2>/dev/null || true
fi

# Only those still installed: removing the rest fails.
listed package | sort -u >"$STAGE/added"
packages | comm -12 "$STAGE/added" - >"$STAGE/remove"
if [ -s "$STAGE/remove" ]; then
  step 'Removing the packages the installer added'
  remove=$(tr '\n' ' ' <"$STAGE/remove")
  echo "$remove"
  # Word splitting: one argument per package.
  if command -v apt-get >/dev/null; then DEBIAN_FRONTEND=noninteractive apt-get purge -y $remove
  elif command -v apk >/dev/null; then apk del $remove
  elif command -v pacman >/dev/null; then pacman -Rns --noconfirm $remove
  elif command -v dnf >/dev/null; then dnf remove -y $remove
  elif command -v yum >/dev/null; then yum remove -y $remove
  elif command -v zypper >/dev/null; then zypper --non-interactive remove $remove
  else die "Remove these packages by hand: $remove"; fi
fi
files=$(listed file)
if [ -n "$files" ]; then
  step 'Removing the package sources the installer added'
  for f in $files; do rm -f -- "$f"; done
  if command -v apt-get >/dev/null; then apt-get update -qq >/dev/null 2>&1 || true; fi
fi

if ours docker; then
  step "Removing Docker's data"
  rm -rf /var/lib/docker /var/lib/containerd /etc/docker /run/docker /run/docker.sock /run/containerd
  if command -v ip >/dev/null; then ip link delete docker0 2>/dev/null || true; fi
  if getent group docker >/dev/null 2>&1; then groupdel docker || true; fi
fi

if [ -n "$MANIFEST" ]; then rm -rf -- "${MANIFEST%/*}"; fi
echo 'homeport is removed from the server'
