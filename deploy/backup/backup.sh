#!/usr/bin/env bash
# Nightly Postgres backup: pg_dump (custom format, uncompressed) -> zstd -> borg.
#
# Runs on the data VM as root, normally from washburn-backup.service. Settings
# come from /opt/washburn/backup/borg.env (see deploy/backup/README.md). To run
# by hand: set -a; . /opt/washburn/backup/borg.env; set +a; ./backup.sh
#
# Never echoes BORG_PASSPHRASE. Exits non-zero on any failure.
set -euo pipefail

log() { printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"; }
die() { log "ERROR: $*" >&2; exit 1; }

: "${BORG_REPO:?BORG_REPO is not set (borg.env)}"
: "${BORG_PASSPHRASE:?BORG_PASSPHRASE is not set (borg.env)}"
: "${WASHBURN_PG_CONTAINER:=washburn-postgres}"
: "${WASHBURN_PG_DB:=washburn}"
: "${WASHBURN_PG_USER:=washburn}"
: "${KEEP_DAILY:=7}"
: "${KEEP_WEEKLY:=4}"
: "${KEEP_MONTHLY:=6}"
export BORG_REPO BORG_PASSPHRASE
[[ -n ${BORG_RSH:-} ]] && export BORG_RSH

command -v borg >/dev/null || die "borg is not installed"
command -v zstd >/dev/null || die "zstd is not installed"
command -v docker >/dev/null || die "docker is not installed"

state="$(docker inspect -f '{{.State.Running}}' "$WASHBURN_PG_CONTAINER" 2>/dev/null || true)"
[[ "$state" == "true" ]] || die "container $WASHBURN_PG_CONTAINER is not running"

# First run: create the repository. An existing repo answers `borg info`.
if borg info >/dev/null 2>&1; then
  log "repository exists: ${BORG_REPO%%::*}"
else
  log "repository not found; initialising with repokey-blake2"
  borg init --encryption=repokey-blake2 "$BORG_REPO" || die "borg init failed"
fi

archive="washburn-$(date -u +%Y-%m-%dT%H:%M)"
log "dumping database $WASHBURN_PG_DB from $WASHBURN_PG_CONTAINER into archive $archive"
start=$(date +%s)

# pg_dump runs inside the container over the local socket (trust auth in the
# official image), so no password is needed here. -Z0 leaves compression to zstd.
# If any stage fails, the partial archive is deleted before exiting.
if ! { docker exec "$WASHBURN_PG_CONTAINER" pg_dump -Fc -Z0 -U "$WASHBURN_PG_USER" "$WASHBURN_PG_DB" \
        | zstd -3 -T2 -q \
        | borg create --stdin-name washburn.dump.zst --compression none --stats "::$archive" -; }; then
  log "dump or borg create failed; removing partial archive $archive if it was created"
  borg delete "::$archive" >/dev/null 2>&1 || true
  die "backup failed"
fi
log "archive $archive written in $(( $(date +%s) - start ))s"

log "pruning (daily=$KEEP_DAILY weekly=$KEEP_WEEKLY monthly=$KEEP_MONTHLY, glob washburn-*)"
borg prune --glob-archives 'washburn-*' \
  --keep-daily "$KEEP_DAILY" --keep-weekly "$KEEP_WEEKLY" --keep-monthly "$KEEP_MONTHLY" \
  --stats || die "borg prune failed"

log "compacting repository"
borg compact || die "borg compact failed"

log "backup finished OK"
