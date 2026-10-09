# Deploy

Three Vultr VMs (see `infra/`): `crawler-N`, `data`, `web`. Each runs one compose stack
from `deploy/compose/<role>.yml`, started by the `washburn-compose@<role>` systemd unit.
Everything is configured from `washburn.toml`. `scripts/render-config.py` renders it into
`.rendered/`: per-role env files, a trimmed config per role, the Caddyfile and `borg.env`.

## Order

1. `cp washburn.example.toml washburn.toml`, fill in every `REPLACE`, then
   `python3 scripts/render-config.py`. This writes `.rendered/`. It refuses placeholders.
2. `tofu apply` in `infra/` (W5). This creates the VMs. cloud-init joins each to Tailscale and
   installs docker, rsync, zstd and borgbackup.
3. DNS: point `[domain].public_host` at the `web` VM's public IP (from the tofu output).
   Nothing else is public.
4. `scripts/deploy.sh data data`. Postgres comes up on the Tailscale IP and the backup timer is
   installed. The script generates the borg SSH key on the VM if missing and prints its public
   key. Then:
   - add that public key to the Storage Box (see `backup/README.md`), and
   - run the first backup once: `ssh root@data systemctl start washburn-backup.service`,
     then check `journalctl -u washburn-backup.service`.
5. `scripts/deploy.sh crawler crawler-1 --workers 2`. The sweepers and judge start, and the
   sweeper creates the schema. Run the restore drill after this (see `backup/README.md`).
6. `scripts/deploy.sh web web`. Caddy obtains the certificate for the public host.
7. Verify:
   - Admin dashboard at `http://web:8001` over Tailscale. It should show a live worker.
   - `ssh root@data systemctl list-timers washburn-backup.timer` shows the next run.
   - `ssh root@crawler-1 docker compose -f /opt/washburn/compose/crawler.yml --env-file /opt/washburn/compose/crawler.env ps`
     shows the sweepers and `judge` healthy.
   - The public site loads over HTTPS with a valid certificate.

`deploy.sh <role> <host> [--workers N]` is idempotent. Re-running it pulls the latest images,
updates the env and config, and runs `up -d`. With `--workers`, the chosen N is written to
`WASHBURN_WORKERS` on the VM, so it survives reboots.

`crawler` and `web` need the data VM's Tailscale IP. `deploy.sh` reads it with
`tailscale ip -4 data` and writes `WASHBURN_DATA_IP`. If that fails, deploy `data` first.

## Config and secrets

- Each VM gets only its own config. `crawler` and `web` receive `.rendered/config/<role>.toml`
  as `/etc/washburn/washburn.toml`, owned `root:10001`, mode 0640. The W8 images run as
  uid/gid 10001. The `data` VM receives no config file.
- Env files are installed mode 0600. `borg.env` is 0600 and root-only.
- The borg SSH private key is generated on the data VM and never leaves it.
- Images are built and pushed by W8. If `[images].ghcr_user` and `ghcr_token` are set,
  `deploy.sh` runs `docker login ghcr.io` on each VM with the token on stdin.

## Images

Images come from the env files: `WASHBURN_IMAGE_SWEEPER` and `WASHBURN_IMAGE_JUDGE` in
`crawler.env`, `WASHBURN_IMAGE_WEB` in `web.env`. Each defaults to
`<registry>/washburn-<name>:<tag>`.

The registry and tag are not yet in `washburn.example.toml`. Proposed addition:

    [images]
    registry = "ghcr.io/OWNER"   # the operator's GHCR owner, lowercase
    tag = "latest"
    # Optional read-only pull token. If both are set, deploy.sh runs docker login on the VMs.
    ghcr_user = ""
    ghcr_token = ""

`render-config.py` builds the image references from these. Pin `tag` to a version for
production deploys.

## Env contract

Each env file is plain `KEY=value`: no quotes, no spaces, no `$` expansion. The authoritative
lists, with comments, are in:

- `deploy/compose/crawler.env.example`
- `deploy/compose/data.env.example`
- `deploy/compose/web.env.example`
- `deploy/backup/README.md` (borg.env)

`deploy.sh` writes `WASHBURN_TAILSCALE_IP` (all roles) and `WASHBURN_DATA_IP` (crawler and
web) on the VM. Render both empty. The `.example` files carry dummy values so
`docker compose config` passes.

## Files

- `compose/{crawler,data,web}.yml` compose stacks. Postgres and admin ports bind only to the
  Tailscale IP. The judge publishes nothing. The sweeper, judge, public and admin services
  pin `data` through `extra_hosts`.
- `caddy/Caddyfile.example` public site only. Admin is not behind Caddy. Rendered verbatim.
- `systemd/` `washburn-compose@.service` (boot-time up, shutdown-time down), and the backup
  service and timer.
- `backup/` backup and restore scripts; see `backup/README.md`.
- `../scripts/deploy.sh` the deploy entry point.
