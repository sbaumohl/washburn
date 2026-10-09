#!/usr/bin/env bash
# Restore drill: restore the newest washburn-* archive into a scratch database
# in the running Postgres container, sanity-check row counts, drop the scratch
# database, and clean up. Non-zero exit on any failure. Run it after the crawler
# has started once, because the sweeper is what creates the schema.
#
# Usage (on the data VM): set -a; . /opt/washburn/backup/borg.env; set +a
#                         /opt/washburn/backup/restore-drill.sh
set -euo pipefail

log() { printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"; }
die() { log "ERROR: $*" >&2; exit 1; }

: "${BORG_REPO:?BORG_REPO is not set (borg.env)}"
: "${BORG_PASSPHRASE:?BORG_PASSPHRASE is not set (borg.env)}"
: "${WASHBURN_PG_CONTAINER:=washburn-postgres}"
: "${WASHBURN_PG_DB:=washburn}"
: "${WASHBURN_PG_USER:=washburn}"
export BORG_REPO BORG_PASSPHRASE
[[ -n ${BORG_RSH:-} ]] && export BORG_RSH

readonly DRILL_DB=washburn_drill
tmp=""
pg_up=0

psql_in() { docker exec -i "$WASHBURN_PG_CONTAINER" psql -X -v ON_ERROR_STOP=1 -U "$WASHBURN_PG_USER" "$@"; }

cleanup() {
  local rc=$?
  if (( pg_up )); then
    if psql_in -d postgres -q -c "DROP DATABASE IF EXISTS $DRILL_DB" >/dev/null 2>&1; then
      log "dropped scratch database $DRILL_DB"
    else
      log "WARNING: could not drop $DRILL_DB; drop it by hand" >&2
      rc=1
    fi
  fi
  [[ -n $tmp ]] && rm -rf "$tmp"
  exit "$rc"
}
trap cleanup EXIT

command -v borg >/dev/null || die "borg is not installed"
command -v zstd >/dev/null || die "zstd is not installed"
[[ "$(docker inspect -f '{{.State.Running}}' "$WASHBURN_PG_CONTAINER" 2>/dev/null || true)" == "true" ]] \
  || die "container $WASHBURN_PG_CONTAINER is not running"

archive="$(borg list --last 1 --short --glob-archives 'washburn-*' "$BORG_REPO")" \
  || die "could not list archives"
[[ -n $archive ]] || die "no washburn-* archives found"
log "newest archive: $archive"

tmp="$(mktemp -d "${TMPDIR:-/var/tmp}/washburn-drill.XXXXXX")"
(cd "$tmp" && borg extract "$BORG_REPO::$archive" washburn.dump.zst) || die "borg extract failed"
[[ -s "$tmp/washburn.dump.zst" ]] || die "extracted dump is empty"
log "extracted $(stat -c %s "$tmp/washburn.dump.zst") bytes"

log "creating scratch database $DRILL_DB"
psql_in -d postgres -q -c "DROP DATABASE IF EXISTS $DRILL_DB" -c "CREATE DATABASE $DRILL_DB"
pg_up=1

log "restoring into $DRILL_DB"
zstd -dc "$tmp/washburn.dump.zst" \
  | docker exec -i "$WASHBURN_PG_CONTAINER" pg_restore --no-owner -U "$WASHBURN_PG_USER" -d "$DRILL_DB" \
  || die "pg_restore failed"

for table in pages findings blobs; do
  count="$(psql_in -d "$DRILL_DB" -At -c "SELECT count(*) FROM $table")" \
    || die "count query failed for $table"
  log "$table: $count rows"
done

# The sweeper applies migrations through sqlx, which records them in _sqlx_migrations.
# At least one row proves the schema came back, not just the data.
migrations="$(psql_in -d "$DRILL_DB" -At -c "SELECT count(*) FROM _sqlx_migrations")" \
  || die "could not read _sqlx_migrations (schema missing?)"
log "_sqlx_migrations: $migrations rows"
if ! { [[ $migrations =~ ^[0-9]+$ ]] && (( migrations >= 1 )); }; then
  die "schema check failed: _sqlx_migrations has no applied migrations"
fi

log "restore drill OK"
