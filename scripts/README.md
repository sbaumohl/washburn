# scripts/

Operator tooling. Python here is stdlib-only except `seed.py`, which needs psycopg.

## render-config.py

Renders `washburn.toml` (copy of `washburn.example.toml`, never committed) into
`.rendered/`, which `deploy.sh` and OpenTofu consume.

```sh
python3 scripts/render-config.py                       # ./washburn.toml -> ./.rendered
python3 scripts/render-config.py --config path/to.toml --out somewhere
```

Exit codes: `0` ok, `1` validation error (nothing written), `2` missing config.
`argparse` also exits `2` on a bad flag.

Validation: every field in `washburn.example.toml` must be present and non-empty
with the same type. Any string still starting with `REPLACE` is an error. A value
containing a newline, or a secret containing a character that a raw env file cannot
hold (`"`, `'`, `#`, `$`, backtick, backslash, leading or trailing space), is an
error. Errors name `table.field` and never print values.

Output (`--out`, default `.rendered/`):

| File | Variables / contents |
|------|----------------------|
| `infra/terraform.tfvars.json` | `vultr_api_key`, `region`, `os_id`, `ssh_key_ids`, `plans`, `counts`, `tailscale_auth_key`, `tailnet`, `public_host`, `enable_vpc=false`, `project_tag="washburn"`, `enable_reserved_ip=false` |
| `config/crawler.toml` | Tables `[typesafe]`, `[openrouter]`, `[postgres]`, `[crawl]`, `[judge]`, copied verbatim (mounted as the VM's `washburn.toml`) |
| `config/web.toml` | Tables `[postgres]`, `[web]`, `[domain]`, copied verbatim |
| `deploy/compose/crawler.env` | `WASHBURN_TAILNET`, `WASHBURN_IMAGE_SWEEPER`, `WASHBURN_IMAGE_JUDGE`, `WASHBURN_WORKERS`, `WASHBURN_GHCR_USER`, `WASHBURN_GHCR_TOKEN`, `WASHBURN_DATA_IP` (empty; deploy.sh fills it) |
| `deploy/compose/data.env` | `POSTGRES_DB`, `POSTGRES_USER`, `POSTGRES_PASSWORD`, `WASHBURN_BACKUP_SCHEDULE`, `WASHBURN_GHCR_USER`, `WASHBURN_GHCR_TOKEN` |
| `deploy/compose/web.env` | `WASHBURN_IMAGE_WEB`, `WASHBURN_TAILNET`, `WASHBURN_PUBLIC_HOST`, `WASHBURN_ACME_EMAIL`, `WASHBURN_GHCR_USER`, `WASHBURN_GHCR_TOKEN`, `WASHBURN_DATA_IP` (empty; deploy.sh fills it) |
| `deploy/caddy/Caddyfile` | `deploy/caddy/Caddyfile.example` with `{$WASHBURN_PUBLIC_HOST}` and `{$WASHBURN_ACME_EMAIL}` substituted; a minimal fallback if the template is absent |
| `deploy/backup/borg.env` | `BORG_REPO`, `BORG_PASSPHRASE`, `BORG_RSH`, `WASHBURN_PG_CONTAINER`, `WASHBURN_PG_DB`, `WASHBURN_PG_USER`, `KEEP_DAILY`, `KEEP_WEEKLY`, `KEEP_MONTHLY` |

The data VM gets no TOML. Postgres reads `data.env` and backups read `borg.env`.

Notes:

- `WASHBURN_TAILSCALE_IP` is deliberately not rendered. `deploy.sh` appends it on
  the VM at deploy time.
- Files holding secrets are written mode `0600`. The Caddyfile is `0644`.
- Compose env files are written as raw `KEY=value` lines because `deploy.sh` reads
  them without unquoting. `borg.env` follows `deploy/backup/README.md`: a value is
  double-quoted only when it contains spaces (`BORG_RSH` does). `backup.sh` is
  sourced by bash and systemd reads the file as an `EnvironmentFile`.
- Values that a raw env file cannot hold (`"`, `'`, `#`, `$`, backtick, backslash)
  are refused for env-bound secrets. `borg.env` values are checked against the
  narrower set in its contract.
- The per-role TOML files are written by a small emitter (`dump_toml`). Tests check
  that each one re-parses with `tomllib` to exactly its tables, with the values
  unchanged.
- `WASHBURN_PG_CONTAINER` is `washburn-postgres`, the `container_name` pinned in
  `deploy/compose/data.yml`. `backup.sh` and `restore-drill.sh` depend on it.
- `[images]` is read from the TOML. If the table is absent, image references fall back
  to `ghcr.io/OWNER/washburn-<name>:latest` with a warning. A registry still containing
  `OWNER` also warns. `ghcr_user` and `ghcr_token` may be empty, which means no
  `docker login` on the VM.
- `washburn.toml` is never rendered or copied. Each VM gets only its `config/<role>.toml`.
- The summary prints key names and values for non-secrets. Secrets show only their
  length. On a re-render, changed keys are listed by name with no values.

## seed.py

Adds crawl seeds to Postgres: `seeds`, `hosts`, and `queue`, one transaction per URL.
Run it from the operator machine on the tailnet, where `[postgres].host` (`data`)
resolves.

```sh
uv run --with 'psycopg[binary]' scripts/seed.py --url example.com https://news.example.org/
uv run --with 'psycopg[binary]' scripts/seed.py --file targets.txt --note "batch 3"
uv run --with 'psycopg[binary]' scripts/seed.py --csv out.csv --column primary_urls
uv run --with 'psycopg[binary]' scripts/seed.py --file targets.txt --dry-run   # no database
```

Input rules follow `crates/crawler/README.md`. A text file has one target per line,
and blank lines and `#` comments are ignored. Bare domains and IPs get `https://`.
Explicit `http://` is kept. IPv6 literals work, bare or bracketed. Fragments are
dropped, duplicates removed, and rejects are listed with a reason at the end. A CSV cell
can hold one target, a JSON array of strings, or `;`/newline-separated targets. Without
`--column`, all recognised columns are read (`target`, `url`, `domain`, `ip`,
`target_domain`, `primary_urls`).

URLs are normalised as the Rust `url` crate serialises them: lowercase scheme and host,
default port dropped, empty path becomes `/`, fragment removed. This keeps `url` and
`url_hash` (sha256 of the URL) identical to the ones the sweeper writes. Hosts are
stored as `host` or `host:port` when the port is non-default.

Exit codes: `0` ok (rejections are reported, not fatal), `1` no valid targets or a
database error, `2` missing config or bad arguments.

## Tests

From the repository root:

```sh
uv run --with pytest pytest scripts/tests -q
```

The render tests run the script as a subprocess and need no network. The parsing
tests need no database. The integration test (`test_seed_inserts_and_maintains_pending_count`)
runs only when `WASHBURN_TEST_DATABASE_URL` is set and psycopg is installed:

```sh
WASHBURN_TEST_DATABASE_URL=postgresql://washburn:...@localhost:5432/scratch \
  uv run --with pytest --with 'psycopg[binary]' pytest scripts/tests -q
```

It creates a throwaway schema, applies `db/migrations/0001_init.sql` there, and drops
the schema afterwards. It never touches the `public` schema. Use a disposable database.

Lint: `uv tool run ruff check scripts`.

## Known gaps (for the coordinator)

- The SSH key for the Storage Box (`/opt/washburn/backup/id_ed25519`) is generated on
  the VM by `deploy.sh` (see `deploy/backup/README.md`). Nothing here creates it.
- `D8` says secrets never go into cloud-init user-data, but `infra/` puts
  `tailscale_auth_key` there (`PROJECTS.md` section 3.6). This is a spec conflict,
  not a render bug.
