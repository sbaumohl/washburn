# Washburn — project spec and status

Washburn is an always-on service for crawling web targets, classifying every
text chunk with TypeSafe Jev, escalating flagged chunks to a strong OpenRouter
judge, and letting an operator review, search, and publish the resulting
findings. It productionizes the `cn-swarms` CLI (now `crates/crawler`) onto
three small Vultr VMs with Postgres as the single system of record.

This file is the running spec. Every workstream below carries a status; update
it when state changes. Architecture diagram: `docs/architecture.excalidraw`.

Status legend: `todo` · `in-progress` · `review` · `done` · `blocked` · `deferred`

---

## 1. Decisions (settled)

| # | Decision | Why |
|---|----------|-----|
| D1 | Three Vultr VMs: crawler `vc2-2c-4gb`, data `vhp-4c-8gb-amd`, web `vc2-2c-4gb`. ~$92/mo. | Data VM is the only IO-bound box; crawler is network-bound; web is read-mostly. |
| D2 | Postgres is the single system of record: queue, visited set, pages, blobs, findings, telemetry, full-text search. No Elasticsearch, no Redis, no object storage. | Corpus is tens of GB at most. One store, one backup, transactional link between evidence and findings. |
| D3 | Raw page bodies are content-addressed zstd blobs in Postgres (`blobs` table, `STORAGE EXTERNAL`). | Few GB compressed; object storage is overhead at this scale. Revisit past ~100 GB. |
| D4 | Durability = nightly `pg_dump -Fc | zstd` to a Hetzner Storage Box via borg over SSH. 7 daily / 4 weekly / 6 monthly. | Off-provider, ~€3/mo, deduplicated. Losing the data VM costs at most one day. |
| D5 | The LLM judge is its own small HTTP service on the crawler VM. Sweepers POST level-one findings to it. | It is an API wrapper with near-zero CPU; one per VM is the natural place for the judge concurrency cap. |
| D6 | Per-host leasing: a worker leases a whole host, then crawls its URLs with the CLI's in-process delay and robots cache. | Preserves politeness guarantees across N workers without a central scheduler. |
| D7 | No Kubernetes. OpenTofu creates VMs; cloud-init installs Docker + Tailscale; Docker Compose runs containers; systemd restarts them. | Three VMs, one service type to scale. Scale-out = one more crawler VM. |
| D8 | One config file, `washburn.toml`, is the source of truth. `scripts/render-config.py` renders tfvars, per-VM `.env`, Caddyfile, borg env. Secrets never go in cloud-init user-data (sole exception: the ephemeral, reusable Tailscale pre-auth key); they are pushed over SSH after boot. | Operator asked for a single TOML. User-data is readable from the instance metadata endpoint. |
| D11 | All intra-service traffic goes over Tailscale MagicDNS names known in advance: `crawler-1`, `data`, `web`. Postgres is published on the data VM's Tailscale IP only. The Vultr VPC is optional and off by default. | Private IPs do not exist until `tofu apply`, so nothing rendered before apply may depend on them. |
| D13 | The full `washburn.toml` never leaves the operator's machine. `render-config.py` writes per-role files: crawler gets `[typesafe] [openrouter] [postgres] [crawl] [judge]`, web gets `[postgres] [web] [domain] [crawl]` (crawl holds no secrets; the admin threshold page reads `importance_threshold`), data gets none (env files only). | Blast radius: the public-facing web VM has no reason to hold the Vultr key or the backup passphrase. |
| D14 | Containers reach the data VM through `extra_hosts: data:<tailscale ip>`, written by `deploy.sh` from `tailscale ip -4 data` at deploy time. Tailscale MagicDNS at 100.100.100.100 is configured as a secondary path only. | DNS from a Docker bridge network to the host's tailscaled is not something we have verified; a static host entry is deterministic. |
| D15 | Images run as uid/gid 10001. `deploy.sh` installs the role config as `root:10001` mode 0640. | Non-root containers plus a readable config without making it world-readable. |
| D16 | Pre-existing Vultr resources are off limits. (1) OpenTofu only manages what is in its state, and nothing is ever `tofu import`ed; (2) `deploy.sh` accepts only `crawler-N`, `data`, `web` hostnames matching the role; (3) recommended: run Washburn from a Vultr sub-account with its own API key so the key cannot even see the old machine. | Vultr API keys are account-wide with no per-instance scoping or deletion protection. |
| D17 | Target definition: primary traces of autonomous AI agent swarms on the public web (collusion.wiki-style), not only malicious use. Jev's three questions, the judge system rubric, and `rubric/rubric.md` all distinguish a primary artifact (agent-authored content, relay/signal pages, identities, heartbeats, tunnels, persistence tricks, probing, transcripts) from human commentary about agents, which scores `no_signal`. `seriousness` now means supported evidence of agents acting outside operators' evident intent. Schema and enum names unchanged. | Reporting about swarms was scoring as highly as the swarms themselves. |
| D18 | Public-site kill switch: `settings.public_site_enabled` in Postgres (migration 0002), toggled from the admin dashboard, read by the public app with a 5 s cache. Off or unreadable means 503 on every public route except `/healthz`; the gate fails closed. | Operator must be able to take the public site down instantly without a redeploy. |
| D12 | Inside a container every service binds `0.0.0.0`. Host exposure is decided only by Compose `ports:` with an explicit host IP (Tailscale IP for Postgres and admin; nothing for judge). | Loopback inside one container is unreachable from another. |
| D9 | Data VM has no public interface. Admin is served on the Tailscale interface with HTTP basic auth. Public site is read-only and sees only `findings.published = true`. | Service publishes claims about third-party sites; publication must be an explicit reviewed step. |
| D10 | Credential scrubbing stays in the sweeper process (inherited from the CLI). Nothing containing keys may be written to Postgres. | Inherited security posture. |

## 2. Repository layout

```
washburn/
  PROJECTS.md               this file
  README.md                 quickstart
  washburn.example.toml     the single config file, documented
  Cargo.toml                Rust workspace
  crates/
    crawler/                cn-swarms library + CLI, copied verbatim (keep tests green)
    common/                 washburn.toml loading, Postgres pool, shared row types
    judge/                  axum service wrapping crawler::judge::Judge
    sweeper/                the daemon: lease host -> crawl -> Jev -> judge -> Postgres
  web/                      Python (uv) FastAPI: admin dashboard + public site
  db/migrations/            SQL schema, numbered; 0001_init.sql is the contract
  infra/                    OpenTofu for Vultr + cloud-init templates
  deploy/
    compose/                one compose file per VM role
    systemd/                units + timers (backup)
    backup/                 pg_dump + borg scripts, restore drill
    caddy/                  Caddyfile template for the web VM
  scripts/                  render-config.py, deploy.sh, seed.py, restore-drill.sh
  rubric/rubric.md          investigation rubric appended to both model stages
  docs/                     architecture diagram, prior attribution write-ups
```

## 3. Component specs

### 3.1 `crates/crawler` (existing, keep green)

The `cn-swarms` library. Public surface used by the other crates:

- `config::{Config, Services, Secret}` — `Config` is the clap struct; `Services` deserializes `[typesafe]` + `[openrouter]`.
- `crawl::{Crawler, Fetch, Job, Page}` — `Crawler::new(Arc<Config>, &[Url])`, `fetch(Job) -> Fetch`, `in_scope(&Url)`.
- `jev::{Jev, Screening}` — `Jev::new(&Config, &Services, Arc<String>)`, `evaluate(&Page, &str, chunk) -> Screening`.
- `judge::{Judge, Judgment, Verdict, Classification, chunks}` — `Judge::new(...)`, `evaluate(&Page, &str, chunk, &Screening) -> Judgment`, `chunks(&str, n)`.
- `sink::id(&Url) -> String` — sha256 hex of the URL. Keep using it as the page id.

Allowed changes (additive only, tests must stay green):
- Add `Config::service_defaults() -> Config` (or similar) so daemons can build a `Config` without parsing CLI args.
- Widen `pub(crate)` to `pub` where a sibling crate genuinely needs it (e.g. `Secret::expose`, `api::ApiClient`). Document each in the PR note.

### 3.2 `crates/common`

- Owns **every** change to `crates/crawler` (see 3.1) so no two crates edit it.
- Wire types shared by judge and sweeper: `JudgeRequest`, `JudgeResponse`, `JudgeError`.
- `repo` module with all SQL from 3.4: `lease_host`, `renew_host`, `release_host`, `lease_job`, `finish_job`, `fail_job`, `store_page` (blobs + pages + queue + links in one transaction), `insert_screening`, `insert_judgment`, `upsert_finding`, `insert_event`, `heartbeat`, `delete_heartbeat`, `add_seed`.
- `WashburnConfig`: deserializes the whole `washburn.toml` with `deny_unknown_fields`. `Services` is built by re-deserializing only the `[typesafe]` and `[openrouter]` tables, so the crawler's own validation runs unchanged. Secrets use `crawler::config::Secret` (Debug = `[REDACTED]`).
- `db::connect(&PostgresConfig) -> sqlx::PgPool` with sane pool limits (max 8 per process). `db::migrate(&pool)` runs `db/migrations` via `sqlx::migrate!("../../db/migrations")`.
- `worker_id() -> String` = `"<hostname>:<pid>"`.
- Row structs for `queue`, `hosts`, `pages`, `screenings`, `judgments`, `findings`, `heartbeats` (sqlx `FromRow`).
- `blob::{compress, decompress, sha256}`; compression level 3.

### 3.3 `crates/judge` (HTTP service, crawler VM, loopback only)

- `POST /v1/judge` body: `JudgeRequest` from `washburn-common` (W1 decides whether the judge needs the full `Page` or only url/final_url/fetched_at, and shapes the type to that) → `200 { "judgment": <Judgment> }`, `422` on rejected model output (`{ "reason", "response" }`), `502` on provider error (`ApiError` serialized), `503` when the semaphore is full for more than `timeout_secs`.
- `GET /healthz` → `{ "ok": true, "model": "...", "in_flight": n, "capacity": n }`.
- Concurrency = `[judge].concurrency`, enforced with a semaphore in front of `Judge::evaluate`. This is the only judge rate cap in v1.
- Binds `[judge].bind` (`0.0.0.0:8081` inside the container; Compose does not publish it). Sweepers dial `[judge].url` (`http://judge:8081`). No auth: only reachable on the Compose network.
- Reads `washburn.toml` path from `WASHBURN_CONFIG` env or `--config`.
- Structured logs via `tracing`; never log request bodies.

### 3.4 `crates/sweeper` (daemon, crawler VM, N copies)

Main loop:

1. **Heartbeat** task: upsert `heartbeats` every `heartbeat_secs`.
2. **Lease a host**: `UPDATE hosts SET leased_by=$me, lease_until=now()+lease WHERE host = (SELECT host FROM hosts WHERE pending_count > 0 AND (lease_until IS NULL OR lease_until < now()) ORDER BY last_request_at NULLS FIRST LIMIT 1 FOR UPDATE SKIP LOCKED) RETURNING host`. If none: sleep 5 s, retry.
3. **Build a `Crawler`** scoped to the seeds of that host (`Crawler::new(config, seed_urls_for_host)`).
4. **Drain the host**: repeatedly `UPDATE queue SET status='leased', leased_by=$me, lease_until=... WHERE id = (SELECT id FROM queue WHERE host=$h AND status='pending' ORDER BY id LIMIT 1 FOR UPDATE SKIP LOCKED) RETURNING *`. Renew the host lease every 30 s while draining. For each job:
   - `crawler.fetch(job)`. On `Skipped` → `status='skipped'`. On `Err` → `attempts += 1`, `status = attempts < 3 ? 'pending' : 'failed'`, `last_error`.
   - On `Page`: in one transaction insert `blobs` (body, on conflict do nothing), insert `pages`, mark `queue.status='done'`, and insert discovered in-scope links into `queue` (`ON CONFLICT (url) DO NOTHING`) if `depth < max_depth`; bump `hosts.pending_count` for each inserted link; create the `hosts` row if missing.
   - For each chunk from `chunks(&page.text, chunk_chars)`: `Jev::evaluate` → insert `screenings`. If `level_one`: insert/append `findings(kind='level_one')` **before** calling the judge, then `POST` to the judge service; insert `judgments`; if `verdict.report(report_threshold)` write `findings(kind='report')` with the same markdown the CLI's sink produces.
   - Every error path writes an `events` row (`classifier_error`, `judge_error`, `fetch_error`) with the CLI's event detail shape, credentials scrubbed.
5. **Release the host** (`leased_by = NULL`) when no pending URLs remain or after `host_lease_secs`, so other workers get a turn.
6. On SIGTERM: stop leasing, finish in-flight jobs, release host, delete own heartbeat row, exit 0.

Config comes from `washburn.toml` (`[crawl]`, `[postgres]`, `[judge].url`, `[typesafe]`). All SQL lives in `washburn-common::repo`; the sweeper only orchestrates: loop, lease renewal, HTTP to the judge, SIGTERM. CLI flags: `--config`, `--once` (drain one host and exit, for tests), `--crawl-only`.

Seeding is **not** the sweeper's job: `scripts/seed.py` (or the admin UI) inserts into `seeds`, `hosts`, and `queue`.

### 3.5 `web/` (FastAPI, uv-managed, web VM)

Two ASGI apps from one package, run as two processes:

- **public** (`[web].public_bind`, `0.0.0.0:8000` in-container, behind Caddy TLS on `public_host`): `GET /` stats (pages crawled, hosts, findings by kind, published count), `GET /reports`, `GET /reports/{id}` rendering markdown, `GET /about`. Reads only `published_reports` view + aggregate counts. No search over raw pages. No write path.
- **admin** (`[web].admin_bind`, `0.0.0.0:8001` in-container; Compose publishes it on the web VM's Tailscale IP only; HTTP basic auth from `[web]`):
  - `GET /` dashboard: live workers (from `live_workers`), queue depth per host, pages/hour, Jev flag rate, judge confirm/downgrade rate, last 50 events.
  - `GET /findings?kind=&published=` list; `GET /findings/{id}` show markdown + linked page + judgments; `POST /findings/{id}/publish` and `/unpublish` (sets `reviewed_by`, `reviewed_at`, `published`, `published_at`).
  - `GET /pages?q=` full-text search over `pages.text_tsv`; `GET /pages/{id}` evidence view (text, headers, redirects); `GET /pages/{id}/body` streams the decompressed blob with `Content-Disposition: attachment`.
  - `GET /seeds`, `POST /seeds` (adds seed + host + queue row), `POST /seeds/{id}/disable`.
  - `GET /threshold` — Jev importance histogram vs. judge outcomes, to evaluate the routing threshold (`[crawl].importance_threshold`, default 0.50).
- Stack: FastAPI, Jinja2, psycopg (v3) with a small pool, markdown-it-py, zstandard. No JS build step; htmx from CDN is acceptable.
- Tests: pytest against a disposable Postgres (`docker run postgres:16`) when `WASHBURN_TEST_DATABASE_URL` is set; pure-unit tests otherwise.

### 3.6 `infra/` (OpenTofu, Vultr provider)

- Resources: three instances (`crawler` count from `[vultr.counts]`, `data`, `web`) with the configured plans and OS, hostnames `crawler-N`, `data`, `web`, tagged by role; an optional VPC (variable `enable_vpc`, default false); a firewall group for `web` allowing 80/443 from anywhere and 22 from nowhere public (SSH goes over Tailscale); a firewall group for `crawler` and `data` with no public inbound at all. Reserved IP for `web` is optional and off by default.
- cloud-init per role (templates in `infra/cloud-init/`): install Docker, install Tailscale and join with `[tailscale].auth_key` (the only secret in user-data, and it is ephemeral/reusable by design), create `/opt/washburn`, enable `docker.service`. Data VM additionally formats nothing extra: Postgres data lives on the instance NVMe at `/opt/washburn/pgdata`.
- Outputs: public IP of `web` (for the DNS A record), instance IDs, hostnames. Nothing downstream depends on private IPs.
- Variables come from `infra/terraform.tfvars.json`, which `scripts/render-config.py` writes. Never commit it.
- `tofu validate` and `tofu plan` must run without credentials beyond the Vultr key.

### 3.7 `deploy/`

- `compose/crawler.yml`: `sweeper` (replicas = `[crawl].workers`, `restart: unless-stopped`, memory limit 1200m) and `judge` (memory limit 256m). Config mounted read-only at `/etc/washburn/washburn.toml`.
- `compose/data.yml`: `postgres:16` with `shared_buffers=2GB`, `effective_cache_size=5GB`, published as `<tailscale-ip>:5432:5432` (the Tailscale IP is read at deploy time by `deploy.sh` via `tailscale ip -4` and written into the env), data dir on NVMe; `pgweb` **not** included.
- `compose/web.yml`: `caddy` (TLS for `public_host`), `public` and `admin` apps. Admin port published as `<tailscale-ip>:8001:8001` only.
- `systemd/washburn-backup.service` + `.timer` (OnCalendar from `[hetzner].schedule`), `backup/backup.sh` (`pg_dump -Fc | zstd -3 | borg create --stdin`), `backup/prune.sh`, `backup/restore-drill.sh` (restores latest into a scratch DB, runs row-count sanity checks, drops it).
- `scripts/deploy.sh <role> <host>`: rsyncs the compose dir + rendered env + `washburn.toml` to `/opt/washburn` over Tailscale SSH and runs `docker compose up -d --pull always`.
- Images are built on the operator's machine with `docker buildx` and pushed to GHCR (`ghcr.io/<owner>/washburn-{sweeper,judge,web}`); no registry secrets on the VMs beyond a read-only pull token in the rendered env.

### 3.8 `scripts/render-config.py`

Reads `washburn.toml`, validates required fields, writes to `.rendered/`:
`infra/terraform.tfvars.json`, `deploy/compose/{crawler,data,web}.env`, `deploy/caddy/Caddyfile`, `deploy/backup/borg.env`. Prints a redacted summary. Exits non-zero on missing/placeholder values (`REPLACE*`). Python 3.13 stdlib only (`tomllib`), so it runs anywhere.

---

## 4. Workstreams and status

| ID | Workstream | Owner | Status | Verification |
|----|-----------|-------|--------|--------------|
| W0 | Repo scaffold, workspace, `.gitignore`, example TOML, schema v1, this spec | Fable | done | `cargo build -p cn-swarms`, `cargo test -p cn-swarms` |
| W1 | `crates/common`: config loading, pool, migrations, repo (all SQL), wire types, blob helpers, and all additive changes to `crates/crawler` | Haiku | done | 18 unit tests pass; crawler 18 tests pass; 2 DB integration flows pass against a live Postgres 16 (pgserver wheel, `uv run --python 3.12 --with pgserver`). Adds `pending_count` decrement on fail/skip and `last_request_at` rotation (deviations from 3.4, accepted). |
| W2 | `crates/judge`: axum service | Haiku | done | 5 tests pass (mock OpenRouter: ok, 422, 502, 503 busy); binary smoke-tested; 504 on whole-request timeout | `cargo test -p washburn-judge`; mocked OpenRouter round-trip test |
| W3 | `crates/sweeper`: daemon (+ `reclaim_stale_jobs`, `canonical_url`, `seeds.enabled` filter in common) | Haiku | done | 10 unit + 3 DB integration tests pass (full run with mock site/Jev/judge; crawl-only; disabled seed). Exit codes 0/3/130/1. | `cargo test -p washburn-sweeper`; integration test with mock HTTP + Postgres in Docker when `WASHBURN_TEST_DATABASE_URL` set |
| W4 | `web/`: FastAPI admin + public | Haiku | done | ruff clean; 44 unit tests pass; 52 pass with a user-space Postgres 16 (pgserver wheel) incl. migration + smoke of every route. Adds CSRF origin check on POSTs. Dockerfile written, not built. |
| W5 | `infra/`: OpenTofu + cloud-init | Haiku | done | `tofu fmt -check`, `tofu validate`, credential-free `tofu plan` all pass (OpenTofu 1.13.1 at `~/.local/bin/tofu`, provider vultr 2.32.0). Uses `vultr_vpc` (vpc2 deprecated). tfvars needs `public_host`. No live apply yet. |
| W6 | `deploy/`: compose, systemd, backup + restore drill, deploy.sh | Haiku | done | `docker compose config` x3, `bash -n`, shellcheck, `systemd-analyze verify` all pass. Remote paths (ssh, borg, compose up) unexercised. | `docker compose -f ... config` passes for all three; `bash -n` + `shellcheck` on scripts |
| W7 | `scripts/render-config.py` + `scripts/seed.py` | Haiku | done | 50 tests pass incl. the DB seed test against live Postgres; renders per-role TOMLs, tfvars, env files, Caddyfile, borg.env with 0600 on secrets. | Renders from example TOML with placeholders replaced by test values; rejects placeholders |
| W8 | Dockerfiles for sweeper, judge (web's is in W4); build script | Haiku | done (image build unverified) | hadolint + shellcheck clean; `cargo build --release --workspace --locked` passes; no Docker daemon here, so no image was built | Images build locally |
| W9 | End-to-end dry run: Postgres + judge + sweeper against mock servers | Fable | superseded by W10 | In-process e2e passes (sweeper `run_db.rs`: seed drains to `done`, pages/blobs/screenings/judgment/findings rows, heartbeat cleaned). Compose-level run (real images, three containers) still needs a Docker daemon. |
| W10 | First real deploy: tofu apply, deploy.sh x3, Tailscale ACLs, DNS records, backup timer firing once | Sam + Fable | done (2026-10-08) | 13 resources applied; images pushed to `ghcr.io/sbaumohl` (private, read token on VMs); all three roles deployed; 2 sweepers heartbeating; judge healthy; admin on `http://web:8001` over Tailscale; public on `https://washburn.baumohl.dev`. First backup: see change log. |
| W11 | Threshold evaluation page populated from real runs; decide whether 0.50 stays | Sam | deferred | Needs W10 data |
| W12 | Shared `rate_tokens` cap for Jev once there are 2+ crawler VMs | — | deferred | Not needed at 1 VM |
| W13 | Cross-language URL canonicalisation vectors: one JSON vector file consumed by the Rust `canonical_url` test, `scripts/tests`, and `web/tests` | — | todo | All three suites pass the same vectors |
| W14 | Read-only Postgres role for the public app (views for stats; grant SELECT only) | — | todo | Public app runs with the restricted role; admin unchanged |
| W16 | Bulk URL seeding (`POST /seeds/bulk`, `GET /seeds/export`) and public-site kill switch (`POST /settings/public-site`, migration 0002) | Haiku | done | web: 86 unit tests pass, 96 with Postgres |
| W15 | Sample below-threshold chunks to the judge (`[crawl].judge_sample_below = 0.0`) so W11 can evaluate lowering the threshold further | — | deferred | Needs W10 traffic first |

## 5. Conventions for implementing agents

- Do not edit `PROJECTS.md` or the root `Cargo.toml`. Report status and any needed workspace dependency in your final message; the coordinator updates both.
- Own only the directory named in your task. If you need a change in another crate (e.g. `crates/crawler`), make it minimal and additive, and list it explicitly in your report.
- Read `washburn.example.toml` and `db/migrations/0001_init.sql` first. They are the contracts. If the contract is wrong, say so in your report instead of silently diverging.
- Secrets: never log, never serialize, never write to Postgres. Use `Secret` from the crawler crate.
- Rust: workspace dependencies (`anyhow.workspace = true`), `tracing` for logs, `sqlx::query` (runtime-checked) rather than `query!` macros so builds do not need a live database.
- Python: `uv`, `pyproject.toml` with pinned minimum versions, `ruff` clean, type hints.
- Every workstream ends with: the verification command from the table above actually run and its output summarized in your report.
- Do not commit. The operator commits.

## 6. Open questions

- Q1: Vultr plan and OS IDs in `washburn.example.toml` are from memory; verify with `vultr-cli plans list` / `os list` before the first `tofu apply`.
- Q2: Should the admin site be reachable only over Tailscale, or also over the Vultr VPC from a bastion? Current spec: Tailscale only.
- Q3: Storage Box borg vs. plain `rsync` of dump files. Spec says borg for deduplication; fall back to rsync if borg on the Storage Box proves awkward.
- Q5: Tailscale DNS inside Docker bridge containers is unverified; D14 sidesteps it for `data`. If sweepers cannot resolve public hostnames through 100.100.100.100, add a public resolver as a second `dns:` entry.
- Q6: Hetzner Storage Box `borg serve` support and borg version on the box side need confirming on first backup run. Both sides need borg >= 1.2 for `borg compact`.
- Q7: Judgments exist only for level-one chunks, so the threshold page can only show outcomes at or above the threshold in force (0.50 by default since 2026-10-07; the CLI's own default stays 0.70). W15 addresses this.
- Q8: `judgments` has no `stage` column; `Judgment.stage` is dropped on insert (always "review" today). Add a column if a second stage ever appears.
- Q9: The CLI's serious report includes an "Archived evidence: ../pages/<id>.txt" line; the service omits it since evidence is served from the blobs table.
- Q10: Job leases are not renewed while a page is processed; a page that takes longer than `host_lease_secs` can be reclaimed and re-fetched (writes are idempotent via the `pages.queue_id` unique guard). Consider renewing job leases in the sweeper or raising `host_lease_secs`.
- Q11: A redirect's `final_url` is not enqueued/marked visited, so it may be crawled again as its own queue row.
- Q12: `add_seed` accepts bare hosts via the crawler's normaliser; `scripts/seed.py` and the admin UI normalise independently. W13 covers the shared vectors.
- Q4: Chunk text for FTS is capped at 200k chars per page in the generated tsvector; confirm this is enough for the long wiki-style pages seen in the attribution work.

## 7. Change log

- 2026-10-07 — Spec created from the design review of the original three-instance diagram. W0 done.
- 2026-10-07 — Advisor review: added D11 (Tailscale names, no private-IP dependency), D12 (bind 0.0.0.0 in containers), tsvector cap lowered to 200k chars, `[crawl].workers` split from `concurrency`, W1 owns all crawler-crate edits. Wave 1 (W1, W4, W5, W6, W7) started; wave 2 (W2, W3, W8) waits on W1.
- 2026-10-07 — W5 done. W6/W7 first pass done; review surfaced the full-config blast radius and container DNS uncertainty, resolved as D13–D15; follow-ups dispatched. Added `[images]` to the example config; cloud-init now installs rsync, zstd, borgbackup.
- 2026-10-07 — Wave 1 complete (W1, W4, W5, W6, W7). Wave 2 started (W2, W3, W8). New follow-ups W13–W15 from review. Coordinator ran all DB-gated suites (common 2, scripts 50, web 52) against a user-space Postgres 16: all pass.
- 2026-10-07 — Wave 2 complete (W2, W3, W8). Full workspace: build clean, clippy clean, 57 Rust tests pass with live Postgres; web 52; scripts 50. Rubric now shipped to crawler VMs and mounted; sweeper gets a 300 s stop grace period. Remaining: W9 compose-level run (needs Docker daemon), W10 first deploy (operator), W13–W15.
- 2026-10-07 — Sam set the Jev importance threshold default to 0.50 (`[crawl].importance_threshold`). Web role config now includes `[crawl]` so the admin threshold page reads the real value instead of a fallback.
- 2026-10-08 — D17 prompts, W16 bulk seeding, and D18 kill switch deployed and verified live (503 with Retry-After when off, /healthz unaffected, admin toggle writes an event). Found and fixed: `sqlx::migrate!` does not notice a newly added migration file in a cached build, so `crates/common/build.rs` now emits `rerun-if-changed=../../db/migrations`; without it the Docker image shipped without 0002.
- 2026-10-08 — First deploy. Fixes found live: Tailscale ACLs must use tags not hostnames (cloud-init now advertises `tag:washburn,tag:washburn-<role>`; `lifecycle.ignore_changes=[user_data]` so template edits never rebuild VMs); `deploy.sh` looked for role configs under `.rendered/deploy/config/` instead of `.rendered/config/`; GHCR packages are private by default so `[images].ghcr_user/ghcr_token` (read:packages) are required unless packages are made public; deploy.sh needs the VM host keys accepted once (`ssh -o StrictHostKeyChecking=accept-new root@<host> true`). Data VM is the Regular `vc2-4c-8gb` plan as configured.
- 2026-10-07 — D16: existing-machine safety. Hostname allowlist in `deploy.sh`; sub-account recommended. (A protected-instance-id guard was built and then removed as overkill at Sam's request.)
