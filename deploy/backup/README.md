# Backups: Postgres to a Hetzner Storage Box

The data VM runs `backup.sh` nightly (`washburn-backup.timer`):

    docker exec washburn-postgres pg_dump -Fc -Z0 | zstd -3 | borg create ... -

Each archive is one file, `washburn.dump.zst`, encrypted with borg `repokey-blake2`
and deduplicated on the box. Retention is `KEEP_DAILY` / `KEEP_WEEKLY` /
`KEEP_MONTHLY` (default 7 / 4 / 6). Only `washburn-*` archives are pruned.

The backup does not depend on the sweeper or the admin app. It needs only the
running `washburn-postgres` container, borg, zstd and docker on the data VM.

## Files

| Path (repo) | Installed to (data VM) | Purpose |
|---|---|---|
| `deploy/backup/backup.sh` | `/opt/washburn/backup/backup.sh` | Dump, borg create, prune, compact |
| `deploy/backup/restore-drill.sh` | `/opt/washburn/backup/restore-drill.sh` | Restore latest into a scratch DB and count rows |
| `.rendered/deploy/backup/borg.env` | `/opt/washburn/backup/borg.env` (0600) | All settings, rendered from `washburn.toml` |
| `deploy/systemd/washburn-backup.{service,timer}` | `/etc/systemd/system/` | Timer; `OnCalendar` is substituted by `deploy.sh` |
| `/opt/washburn/backup/id_ed25519` | (generated on the VM by deploy.sh, never leaves it) | SSH key for the Storage Box |

## borg.env contract

Rendered from `washburn.toml`. `KEY=value` lines. Wrap a value in double quotes only when
it contains spaces (`BORG_RSH` always does). Values must not contain `"`, `\`, `$` or
backticks, because both systemd's EnvironmentFile and the shell read the file.

| Variable | Example | Source |
|---|---|---|
| `BORG_REPO` | `ssh://uXXXXXX@uXXXXXX.your-storagebox.de:23/./backups/washburn` | `[hetzner]` |
| `BORG_PASSPHRASE` | (random, alphanumeric) | `[hetzner].borg_passphrase` |
| `BORG_RSH` | `"ssh -i /opt/washburn/backup/id_ed25519 -p 23 -o StrictHostKeyChecking=accept-new"` (quoted) | derived |
| `WASHBURN_PG_CONTAINER` | `washburn-postgres` | fixed in `data.yml` |
| `WASHBURN_PG_DB` | `washburn` | `[postgres].db` |
| `WASHBURN_PG_USER` | `washburn` | `[postgres].user` |
| `KEEP_DAILY` | `7` | `[hetzner].keep_daily` |
| `KEEP_WEEKLY` | `4` | `[hetzner].keep_weekly` |
| `KEEP_MONTHLY` | `6` | `[hetzner].keep_monthly` |

## First-time setup

1. `scripts/deploy.sh data <host>` generates the SSH key on the VM if it is missing:
   `/opt/washburn/backup/id_ed25519` (ed25519, no passphrase, root-owned, mode 0600). The private
   key never leaves the VM. The script prints the public key and the next steps.
   Borg and zstd are installed by cloud-init (`borgbackup`, `zstd`). `deploy.sh` warns if the
   VM's borg is older than 1.2.

2. Authorise the printed public key on the Storage Box. Either:
   - Hetzner Robot / Storage Box panel: add the public key under SSH keys, or
   - from your workstation: `ssh-copy-id -i <key>.pub -p 23 -s uXXXXXX@uXXXXXX.your-storagebox.de`
     (the `-s` flag asks for the subsystem form that Hetzner's docs show; drop it if the box refuses).

   Storage Boxes listen on port **23** for SSH, SFTP and borg. Port 22 is not used.
   Hetzner Storage Boxes run `borg serve` natively, so nothing needs installing on the box.
   Verify from the VM: `ssh -i /opt/washburn/backup/id_ed25519 -p 23 uXXXXXX@uXXXXXX.your-storagebox.de borg --version`.
   The box's borg version is the operator's to confirm; it must be compatible with the VM's
   (1.2 or 1.4 on both sides).

3. Run the first backup once and read the log. `backup.sh` runs `borg info` and, if the repo is
   missing, `borg init --encryption=repokey-blake2`:

       ssh root@data systemctl start washburn-backup.service
       ssh root@data journalctl -u washburn-backup.service -n 50 --no-pager

   If the parent directory (`backups/`) does not exist on the box, create it with
   `sftp -P 23 uXXXXXX@uXXXXXX.your-storagebox.de` and `mkdir backups`.

4. Save the passphrase **outside** the VM as well (password manager). Without it the archives
   cannot be decrypted, and the key file alone is not enough.

5. Run the restore drill after the crawler has started, because the sweeper creates the schema.
   Before that the drill fails, by design, at the `_sqlx_migrations` check.

## Schedule

`deploy.sh` replaces the `OnCalendar=` line in the installed timer with `[hetzner].schedule`.
Check it with:

    systemctl cat washburn-backup.timer
    systemctl list-timers washburn-backup.timer
    journalctl -u washburn-backup.service -n 100 --no-pager

## Restore drill

Run it after the first backup, after any change to the schema or backup scripts, and then
monthly. It is not automatic.

    ssh root@data
    set -a; . /opt/washburn/backup/borg.env; set +a
    /opt/washburn/backup/restore-drill.sh

It restores the newest `washburn-*` archive into database `washburn_drill`, prints the
`pages`, `findings` and `blobs` counts, and asserts that `_sqlx_migrations` has at least one row,
which proves the schema came back. It exits non-zero otherwise. The scratch database and the
temp directory are removed on the way out, even when a step fails.

## Listing archives

    set -a; . /opt/washburn/backup/borg.env; set +a
    borg list "$BORG_REPO"
    borg info "$BORG_REPO::<archive-name>"

## Notes and limits

- A failed run deletes the partial archive it created. A successful run may still produce a
  dump that is wrong at the logical level; the restore drill is the check for that.
- `borg info` failing for any reason (network, auth) makes `backup.sh` attempt `borg init`,
  which then fails with a clear error. It will not overwrite an existing repo.
- Retention is per-day (`--keep-daily`), so two runs on one day keep one of them.
- Dumps are not encrypted by `pg_dump`; borg encrypts them on the way to the box.
