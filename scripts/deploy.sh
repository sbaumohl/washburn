#!/usr/bin/env bash
# Deploy one Washburn VM role over Tailscale SSH.
#
# Usage: scripts/deploy.sh <crawler|data|web> <tailscale-hostname> [--workers N]
#
# Needs .rendered/ from scripts/render-config.py. Connects as root@<host>.
# Env files are read as plain KEY=value lines, never sourced or put in argv.
# Secrets reach the VM only through the rsync/ssh stream (GHCR token on stdin).
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
usage: deploy.sh <crawler|data|web> <tailscale-hostname> [--workers N]

  crawler   sweeper replicas (N, default WASHBURN_WORKERS) + judge
  data      Postgres + nightly backup timer
  web       Caddy + public and admin apps

Run scripts/render-config.py first if .rendered/ is missing.
USAGE
  exit 2
}

die() {
  echo "deploy.sh: $*" >&2
  exit 1
}

[[ $# -ge 2 ]] || usage
ROLE=$1
HOST=$2
shift 2

WORKERS=""
while [[ $# -gt 0 ]]; do
  case $1 in
    --workers)
      [[ $# -ge 2 ]] || usage
      WORKERS=$2
      shift 2
      ;;
    -h | --help) usage ;;
    *) die "unknown argument: $1" ;;
  esac
done

case $ROLE in
  crawler | data | web) ;;
  *) usage ;;
esac
# Only Washburn's own Tailscale hostnames are valid targets (optionally with a
# tailnet suffix). This script must never be pointed at any other machine.
[[ $HOST =~ ^(crawler-[0-9]+|data|web)(\.[A-Za-z0-9.-]+)?$ ]] || die "refusing: $HOST is not a washburn host (crawler-N, data, web)"
case $ROLE:$HOST in
  crawler:crawler-* | data:data | data:data.* | web:web | web:web.*) ;;
  *) die "refusing: host $HOST does not match role $ROLE" ;;
esac
[[ -z $WORKERS || $ROLE == crawler ]] || die "--workers only applies to the crawler role"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RENDERED="$ROOT/.rendered/deploy"
ENV_FILE="$RENDERED/compose/$ROLE.env"
COMPOSE_FILE="$ROOT/deploy/compose/$ROLE.yml"
SYSTEMD_DIR="$ROOT/deploy/systemd"

[[ -d "$ROOT/.rendered" ]] || die "missing .rendered/. Run: python3 scripts/render-config.py"
[[ -f "$ENV_FILE" ]] || die "missing $ENV_FILE. Run: python3 scripts/render-config.py"
[[ -f "$COMPOSE_FILE" ]] || die "missing $COMPOSE_FILE"
# Only crawler and web get a config file; each role gets its trimmed copy from
# render-config.py. The data VM needs no config and never receives washburn.toml.
CONFIG_FILE=""
if [[ $ROLE != data ]]; then
  CONFIG_FILE="$ROOT/.rendered/config/$ROLE.toml"
  [[ -f "$CONFIG_FILE" ]] || die "missing $CONFIG_FILE. Run: python3 scripts/render-config.py"
fi

# env_get KEY FILE: value of the last KEY= line, everything after the first '='.
env_get() {
  awk -v k="$1" 'index($0, k "=") == 1 { v = substr($0, length(k) + 2) } END { print v }' "$2"
}

# require_var KEY: die if KEY is empty in ENV_FILE, otherwise print its value.
require_var() {
  local value
  value="$(env_get "$1" "$ENV_FILE")"
  [[ -n $value ]] || die "$1 is empty in $ENV_FILE"
  printf '%s' "$value"
}

case $ROLE in
  crawler)
    require_var WASHBURN_IMAGE_SWEEPER >/dev/null
    require_var WASHBURN_IMAGE_JUDGE >/dev/null
    require_var WASHBURN_TAILNET >/dev/null
    if [[ -z $WORKERS ]]; then
      WORKERS="$(require_var WASHBURN_WORKERS)"
    fi
    [[ $WORKERS =~ ^[1-9][0-9]*$ ]] || die "workers must be a positive integer, got: $WORKERS"
    ;;
  data)
    require_var POSTGRES_PASSWORD >/dev/null
    ;;
  web)
    require_var WASHBURN_IMAGE_WEB >/dev/null
    require_var WASHBURN_TAILNET >/dev/null
    [[ -f "$RENDERED/caddy/Caddyfile" ]] || die "missing $RENDERED/caddy/Caddyfile"
    ;;
esac

SCHEDULE=""
if [[ $ROLE == data ]]; then
  for f in backup/backup.sh backup/restore-drill.sh systemd/washburn-backup.service systemd/washburn-backup.timer; do
    [[ -f "$ROOT/deploy/$f" ]] || die "missing deploy/$f"
  done
  [[ -f "$RENDERED/backup/borg.env" ]] || die "missing $RENDERED/backup/borg.env"
  SCHEDULE="$(require_var WASHBURN_BACKUP_SCHEDULE)"
  [[ $SCHEDULE =~ ^[A-Za-z0-9*:,./\ -]+$ ]] || die "backup schedule has unexpected characters: $SCHEDULE"
fi

GHCR_USER="$(env_get WASHBURN_GHCR_USER "$ENV_FILE")"
GHCR_TOKEN="$(env_get WASHBURN_GHCR_TOKEN "$ENV_FILE")"
[[ $GHCR_USER =~ ^[A-Za-z0-9-]*$ ]] || die "WASHBURN_GHCR_USER has unexpected characters"

TARGET="root@$HOST"

# rssh ARGS... < script: run a bash script from stdin on the VM with ARGS as $1...
# Arguments are %q-quoted so ssh's word joining cannot split or inject them.
rssh() {
  local quoted=() a
  for a in "$@"; do
    quoted+=("$(printf '%q' "$a")")
  done
  # SC2029: expanding client-side is intended; every arg was %q-quoted above.
  # shellcheck disable=SC2029
  ssh "$TARGET" "bash -s -- ${quoted[*]}"
}

echo "==> $ROLE on $HOST"

echo "==> preparing directories"
rssh "$ROLE" <<'REMOTE'
set -euo pipefail
ROLE=$1
install -d -m 0755 /opt/washburn/compose /opt/washburn/caddy /opt/washburn/systemd /opt/washburn/backup
install -d -m 0750 /etc/washburn
if [[ $ROLE == data ]]; then
  # The official postgres image runs as uid/gid 999.
  install -d -m 0700 -o 999 -g 999 /opt/washburn/pgdata
fi
REMOTE

echo "==> copying compose file, env, systemd units"
# --chmod sets the mode at creation, so secrets are never briefly world-readable.
rsync -a -e ssh "$COMPOSE_FILE" "$TARGET:/opt/washburn/compose/$ROLE.yml"
rsync -a --chmod=F600 -e ssh "$ENV_FILE" "$TARGET:/opt/washburn/compose/$ROLE.env"
rsync -a --delete -e ssh "$SYSTEMD_DIR/" "$TARGET:/opt/washburn/systemd/"

if [[ $ROLE == web ]]; then
  rsync -a -e ssh "$RENDERED/caddy/Caddyfile" "$TARGET:/opt/washburn/caddy/Caddyfile"
fi

if [[ $ROLE == data ]]; then
  rsync -a -e ssh "$ROOT/deploy/backup/backup.sh" "$ROOT/deploy/backup/restore-drill.sh" \
    "$TARGET:/opt/washburn/backup/"
  rsync -a --chmod=F600 -e ssh "$RENDERED/backup/borg.env" "$TARGET:/opt/washburn/backup/borg.env"
fi

CHANGED=0
if [[ -n $CONFIG_FILE ]]; then
  echo "==> copying $ROLE config to /etc/washburn/washburn.toml"
  # --inplace keeps the file's inode, so running containers see the new contents
  # through their bind mount. --itemize-changes tells us whether anything moved.
  CHANGED="$(rsync -a --inplace --no-owner --no-group --chmod=F640 --itemize-changes -e ssh \
    "$CONFIG_FILE" "$TARGET:/etc/washburn/washburn.toml" | grep -c '^>f' || true)"
  # The W8 images run as uid/gid 10001, so they read the file through group 10001.
  rssh <<'REMOTE'
set -euo pipefail
getent group 10001 >/dev/null || groupadd --gid 10001 washburn
chown root:10001 /etc/washburn/washburn.toml
chmod 0640 /etc/washburn/washburn.toml
REMOTE
fi

if [[ $ROLE == crawler ]]; then
  echo "==> copying rubric to /etc/washburn/rubric.md"
  # The sweeper (Jev stage) and the judge both append this to their prompts.
  rsync -a --inplace --no-owner --no-group --chmod=F644 -e ssh \
    "$ROOT/rubric/rubric.md" "$TARGET:/etc/washburn/rubric.md"
fi

echo "==> finalising env on the VM (Tailscale IP, workers, permissions)"
rssh "$ROLE" "$WORKERS" <<'REMOTE'
set -euo pipefail
ROLE=$1
WORKERS=$2
env_file="/opt/washburn/compose/$ROLE.env"

# set_kv KEY VALUE: replace KEY in the env file (or append it).
set_kv() {
  sed -i "/^${1}=/d" "$env_file"
  printf '%s=%s\n' "$1" "$2" >> "$env_file"
}

ip="$(tailscale ip -4 | head -n1)"
[[ -n $ip ]] || { echo "tailscale returned no IPv4 address" >&2; exit 1; }
set_kv WASHBURN_TAILSCALE_IP "$ip"
if [[ $ROLE == crawler ]]; then
  set_kv WASHBURN_WORKERS "$WORKERS"
fi
if [[ $ROLE == crawler || $ROLE == web ]]; then
  # extra_hosts pins the name "data" to this address, so containers do not depend on DNS forwarding.
  data_ip="$(tailscale ip -4 data 2>/dev/null | head -n1 || true)"
  [[ -n $data_ip ]] || { echo "tailscale cannot resolve 'data' to an IPv4 address. Deploy the data VM first (deploy.sh data)." >&2; exit 1; }
  set_kv WASHBURN_DATA_IP "$data_ip"
fi
chmod 0600 "$env_file"

if [[ $ROLE == data ]]; then
  chmod 0600 /opt/washburn/backup/borg.env
  chmod 0700 /opt/washburn/backup/backup.sh /opt/washburn/backup/restore-drill.sh
fi
REMOTE

echo "==> installing systemd units"
rssh "$ROLE" "$SCHEDULE" <<'REMOTE'
set -euo pipefail
ROLE=$1
SCHEDULE=$2
install -m 0644 /opt/washburn/systemd/washburn-compose@.service /etc/systemd/system/washburn-compose@.service
if [[ $ROLE == data ]]; then
  install -m 0644 /opt/washburn/systemd/washburn-backup.service /etc/systemd/system/washburn-backup.service
  # systemd cannot read env files, so the OnCalendar placeholder is replaced here.
  systemd-analyze calendar "$SCHEDULE" >/dev/null
  sed "s|^OnCalendar=.*|OnCalendar=$SCHEDULE|" /opt/washburn/systemd/washburn-backup.timer \
    > /etc/systemd/system/washburn-backup.timer
  chmod 0644 /etc/systemd/system/washburn-backup.timer
fi
systemctl daemon-reload
systemctl enable "washburn-compose@$ROLE.service"
if [[ $ROLE == data ]]; then
  systemctl enable --now washburn-backup.timer
fi
REMOTE

if [[ $ROLE == data ]]; then
  echo "==> borg SSH key and borg version"
  rssh <<'REMOTE'
set -euo pipefail
key=/opt/washburn/backup/id_ed25519
if [[ ! -f $key ]]; then
  # Generated on the VM and never copied off it.
  ssh-keygen -q -t ed25519 -N '' -C washburn-backup -f "$key"
  echo "generated $key"
fi
chown root:root "$key" "$key.pub"
chmod 0600 "$key"
chmod 0644 "$key.pub"
echo "public key (add it to the Storage Box):"
cat "$key.pub"
if ! command -v borg >/dev/null; then
  echo "WARNING: borg is not installed on the data VM (cloud-init should install borgbackup)" >&2
else
  ver="$(borg --version | awk '{print $2}')"
  oldest="$(printf '%s\n' 1.2 "$ver" | sort -V | head -n1)"
  if [[ $oldest != 1.2 ]]; then
    echo "WARNING: borg $ver is older than 1.2; backup.sh needs borg compact and --glob-archives" >&2
  fi
fi
REMOTE
fi

if [[ -n $GHCR_USER && -n $GHCR_TOKEN ]]; then
  echo "==> docker login ghcr.io as $GHCR_USER"
  # The token goes through stdin only.
  # SC2029: GHCR_USER is validated against [A-Za-z0-9-]* above.
  # shellcheck disable=SC2029
  printf '%s\n' "$GHCR_TOKEN" | ssh "$TARGET" "docker login ghcr.io --username $GHCR_USER --password-stdin" >/dev/null
fi

echo "==> pulling images and starting the stack"
rssh "$ROLE" "$WORKERS" "$CHANGED" <<'REMOTE'
set -euo pipefail
ROLE=$1
WORKERS=$2
CHANGED=$3
cd /opt/washburn/compose
compose=(docker compose -f "$ROLE.yml" --env-file "$ROLE.env")
"${compose[@]}" pull
if [[ $ROLE == crawler ]]; then
  "${compose[@]}" up -d --scale "sweeper=$WORKERS"
else
  "${compose[@]}" up -d
fi
# Start the unit too, so systemctl status, stop and Restart=on-failure reflect reality.
# Its ExecStart is the same idempotent `up -d`, and the scale comes from WASHBURN_WORKERS.
systemctl start "washburn-compose@$ROLE.service"
if [[ $CHANGED -gt 0 && $ROLE != data ]]; then
  echo "washburn.toml changed: restarting containers so they reload it"
  "${compose[@]}" restart
fi
"${compose[@]}" ps
REMOTE

echo "==> done: $ROLE on $HOST"
if [[ $ROLE == data ]]; then
  cat <<NEXT
Next steps for the backup:
  1. Add the public key printed above to the Storage Box (Hetzner Robot panel, or
     ssh-copy-id -i <key>.pub -p 23 -s <user>@<box-host>).
  2. Run the first backup once and read its log:
       ssh root@$HOST systemctl start washburn-backup.service
       ssh root@$HOST journalctl -u washburn-backup.service -n 50 --no-pager
NEXT
fi
