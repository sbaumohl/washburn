# Washburn

An always-on service for crawling web targets, classifying every text chunk
with TypeSafe Jev, escalating flagged chunks to a strong OpenRouter judge, and
letting an operator search, review, and publish the resulting findings.

Washburn is the service form of the `cn-swarms` CLI, which lives on unchanged
as `crates/crawler`. The design, decisions, component contracts, and current
status are in [PROJECTS.md](PROJECTS.md). The architecture diagram is
`docs/architecture.excalidraw`.

## Shape

```
crawler VM  (vc2-2c-4gb)      data VM  (vhp-4c-8gb-amd)       web VM  (vc2-2c-4gb)
  sweeper x N  ──────────────►  Postgres 16                ◄──  admin  (Tailscale only)
  judge (OpenRouter wrapper)     queue, pages, blobs,            public (Caddy, TLS)
                                 findings, events, FTS
                                 nightly pg_dump ──► Hetzner Storage Box (borg)
```

## One config file

Copy `washburn.example.toml` to `washburn.toml`, fill it in, and run
`scripts/render-config.py`. Everything else (OpenTofu variables, per-role
trimmed configs, per-VM env files, Caddyfile, backup settings) is rendered
from it into `.rendered/`. `washburn.toml` is gitignored, holds every secret,
and never leaves your machine; each VM receives only the tables it needs.

## Local development

```sh
cargo build --workspace
cargo test  --workspace            # DB-backed tests skip unless the env var below is set
cd web && uv sync && uv run pytest
uv run --with pytest --with 'psycopg[binary]' pytest scripts/tests
```

The database-backed tests in `crates/common`, `crates/sweeper`, `web`, and
`scripts` run when `WASHBURN_TEST_DATABASE_URL` points at a Postgres 16. With
Docker: `docker run -d --rm -e POSTGRES_PASSWORD=pw -p 55432:5432 postgres:16`
and `postgres://postgres:pw@localhost:55432/postgres`. Without Docker, the
`pgserver` wheel gives a user-space server:

```sh
uv run --python 3.12 --with pgserver python -c \
  'import pgserver,pathlib; print(pgserver.get_server(pathlib.Path("/tmp/wb-pg"), cleanup_mode=None).get_uri())'
```

Stop it afterwards with the same call followed by `.cleanup()`.

## Deploying

1. `scripts/render-config.py`
2. `infra/README.md`: `tofu init`, `tofu plan`, `tofu apply`; create the DNS A/AAAA record.
3. `deploy/README.md`: `scripts/deploy.sh data`, then `crawler`, then `web`.
4. `scripts/build-images.sh --push` beforehand to publish the three images.

Commit `Cargo.lock`, `web/uv.lock`, and `infra/.terraform.lock.hcl`; the
Dockerfile builds with `--locked`.
