-- Washburn schema, version 1.
-- Single system of record: queue, state, evidence blobs, findings, telemetry.
-- Applied by `sqlx migrate run` (sweeper) or `psql -f` (deploy/bootstrap).

-- ---------------------------------------------------------------- seeds
-- Operator-supplied crawl roots. Scope rule: links stay within the exact
-- host[:port] of a seed (same rule as the CLI). Subdomains must be seeded.
CREATE TABLE seeds (
    id          bigserial PRIMARY KEY,
    url         text NOT NULL UNIQUE,
    host        text NOT NULL,              -- host[:port], lowercase
    note        text,
    added_by    text,
    added_at    timestamptz NOT NULL DEFAULT now(),
    enabled     boolean NOT NULL DEFAULT true
);
CREATE INDEX seeds_host_idx ON seeds (host);

-- ---------------------------------------------------------------- hosts
-- One worker owns a host at a time, so the crawler's in-process per-origin
-- delay and robots cache keep their guarantees across N workers.
CREATE TABLE hosts (
    host            text PRIMARY KEY,
    leased_by       text,                    -- worker id, NULL when free
    lease_until     timestamptz,
    pending_count   integer NOT NULL DEFAULT 0,
    last_request_at timestamptz,
    robots_blocked  boolean NOT NULL DEFAULT false
);
CREATE INDEX hosts_free_idx ON hosts (lease_until) WHERE pending_count > 0;

-- ---------------------------------------------------------------- queue
-- The URL work queue. `url UNIQUE` doubles as the visited set: an insert
-- that conflicts is a URL we have already scheduled, so it is dropped.
CREATE TYPE queue_status AS ENUM ('pending', 'leased', 'done', 'failed', 'skipped');

CREATE TABLE queue (
    id           bigserial PRIMARY KEY,
    url          text NOT NULL UNIQUE,
    url_hash     bytea NOT NULL UNIQUE,      -- sha256(url), matches CLI page id
    host         text NOT NULL REFERENCES hosts (host),
    seed_id      bigint NOT NULL REFERENCES seeds (id),
    parent_url   text,
    depth        integer NOT NULL DEFAULT 0,
    status       queue_status NOT NULL DEFAULT 'pending',
    leased_by    text,
    lease_until  timestamptz,
    attempts     integer NOT NULL DEFAULT 0,
    last_error   text,
    created_at   timestamptz NOT NULL DEFAULT now(),
    finished_at  timestamptz
);
CREATE INDEX queue_pending_host_idx ON queue (host, id) WHERE status = 'pending';
CREATE INDEX queue_status_idx ON queue (status);

-- ---------------------------------------------------------------- blobs
-- Content-addressed raw evidence. `bytes` is zstd-compressed by the writer;
-- EXTERNAL storage stops TOAST from recompressing it.
CREATE TABLE blobs (
    sha256      bytea PRIMARY KEY,           -- sha256 of the UNCOMPRESSED bytes
    bytes       bytea NOT NULL,              -- zstd frame
    raw_len     integer NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);
ALTER TABLE blobs ALTER COLUMN bytes SET STORAGE EXTERNAL;

-- ---------------------------------------------------------------- pages
-- One row per fetched page (the CLI's pages/<sha>.json). Extracted text is
-- stored inline for full-text search; the raw body lives in blobs.
CREATE TABLE pages (
    id            bigserial PRIMARY KEY,
    queue_id      bigint NOT NULL UNIQUE REFERENCES queue (id),
    url           text NOT NULL,
    final_url     text NOT NULL,
    seed_id       bigint NOT NULL REFERENCES seeds (id),
    depth         integer NOT NULL,
    status        integer NOT NULL,          -- HTTP status
    content_type  text,
    fetched_at    timestamptz NOT NULL,
    worker        text NOT NULL,
    body_sha256   bytea NOT NULL REFERENCES blobs (sha256),
    text          text NOT NULL,
    -- Capped well under Postgres's 1 MB tsvector limit: hash-heavy pages
    -- produce many unique lexemes. Search covers the first 200k chars.
    text_tsv      tsvector GENERATED ALWAYS AS (to_tsvector('simple', left(text, 200000))) STORED,
    redirects     jsonb NOT NULL DEFAULT '[]',
    links         jsonb NOT NULL DEFAULT '[]',   -- discovered in-scope links
    meta          jsonb NOT NULL DEFAULT '{}'    -- anything else from crawl::Page
);
CREATE INDEX pages_text_tsv_idx ON pages USING gin (text_tsv);
CREATE INDEX pages_fetched_idx ON pages (fetched_at DESC);
CREATE INDEX pages_seed_idx ON pages (seed_id);

-- ---------------------------------------------------------------- screenings
-- One row per Jev call (per chunk). Mirrors jev::Screening.
CREATE TABLE screenings (
    id                   bigserial PRIMARY KEY,
    page_id              bigint NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
    chunk                integer NOT NULL,
    requested_model      text NOT NULL,
    model                text NOT NULL,
    importance           double precision NOT NULL,
    agent_activity       double precision NOT NULL,
    potentially_serious  double precision NOT NULL,
    level_one            boolean NOT NULL,   -- importance >= threshold at the time
    threshold            double precision NOT NULL,
    usage                jsonb NOT NULL DEFAULT '{}',
    created_at           timestamptz NOT NULL DEFAULT now(),
    UNIQUE (page_id, chunk)
);
CREATE INDEX screenings_level_one_idx ON screenings (created_at DESC) WHERE level_one;

-- ---------------------------------------------------------------- judgments
-- One row per strong-judge call. Mirrors judge::Judgment / judge::Verdict.
CREATE TYPE classification AS ENUM ('no_signal', 'authorized', 'unclear', 'suspected_unauthorized');

CREATE TABLE judgments (
    id                 bigserial PRIMARY KEY,
    page_id            bigint NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
    screening_id       bigint NOT NULL REFERENCES screenings (id) ON DELETE CASCADE,
    chunk              integer NOT NULL,
    requested_model    text NOT NULL,
    model              text NOT NULL,
    activity           double precision NOT NULL,
    seriousness        double precision NOT NULL,
    confidence         double precision NOT NULL,
    classification     classification NOT NULL,
    confirmed_serious  boolean NOT NULL,
    summary            text NOT NULL,
    evidence           jsonb NOT NULL DEFAULT '[]',   -- exact source quotes
    uncertainties      jsonb NOT NULL DEFAULT '[]',
    usage              jsonb NOT NULL DEFAULT '{}',
    verdict            jsonb NOT NULL,                -- full raw verdict
    created_at         timestamptz NOT NULL DEFAULT now(),
    UNIQUE (screening_id)
);
CREATE INDEX judgments_serious_idx ON judgments (created_at DESC) WHERE confirmed_serious;

-- ---------------------------------------------------------------- findings
-- Human-facing markdown. `level_one` is written as soon as Jev flags a
-- chunk (before the judge runs); `report` is written when the judge
-- confirms a serious finding above report_threshold. Nothing reaches the
-- public site unless published = true.
CREATE TYPE finding_kind AS ENUM ('level_one', 'report');

CREATE TABLE findings (
    id            bigserial PRIMARY KEY,
    page_id       bigint NOT NULL REFERENCES pages (id) ON DELETE CASCADE,
    kind          finding_kind NOT NULL,
    title         text NOT NULL,
    markdown      text NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    reviewed_by   text,
    reviewed_at   timestamptz,
    review_note   text,
    published     boolean NOT NULL DEFAULT false,
    published_at  timestamptz,
    UNIQUE (page_id, kind)
);
CREATE INDEX findings_published_idx ON findings (published_at DESC) WHERE published;
CREATE INDEX findings_kind_idx ON findings (kind, created_at DESC);

-- ---------------------------------------------------------------- events
-- Append-only telemetry: what events.jsonl holds in the CLI. Credentials
-- must be scrubbed by the writer before insert.
CREATE TABLE events (
    id      bigserial PRIMARY KEY,
    at      timestamptz NOT NULL DEFAULT now(),
    worker  text NOT NULL,
    kind    text NOT NULL,
    detail  jsonb NOT NULL DEFAULT '{}'
);
CREATE INDEX events_at_idx ON events (at DESC);
CREATE INDEX events_kind_idx ON events (kind, at DESC);

-- ---------------------------------------------------------------- heartbeats
-- One row per live worker process. The admin dashboard treats a worker
-- whose last_seen is older than 3 x heartbeat_secs as dead.
CREATE TABLE heartbeats (
    worker       text PRIMARY KEY,           -- "<hostname>:<pid>"
    hostname     text NOT NULL,
    pid          integer NOT NULL,
    version      text NOT NULL,
    started_at   timestamptz NOT NULL,
    last_seen    timestamptz NOT NULL,
    active_jobs  integer NOT NULL DEFAULT 0,
    leased_host  text,
    detail       jsonb NOT NULL DEFAULT '{}'
);

-- ---------------------------------------------------------------- rate_tokens
-- Reserved for shared provider rate caps once there is more than one
-- crawler VM. Not used by v1 (single VM; caps are in-process).
CREATE TABLE rate_tokens (
    provider     text PRIMARY KEY,
    capacity     integer NOT NULL,
    in_flight    integer NOT NULL DEFAULT 0,
    updated_at   timestamptz NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------- views
CREATE VIEW queue_depth AS
    SELECT host, count(*) AS pending
    FROM queue WHERE status = 'pending'
    GROUP BY host;

CREATE VIEW live_workers AS
    SELECT * FROM heartbeats WHERE last_seen > now() - interval '90 seconds';

CREATE VIEW published_reports AS
    SELECT f.id, f.title, f.markdown, f.published_at, p.final_url, p.fetched_at
    FROM findings f JOIN pages p ON p.id = f.page_id
    WHERE f.published AND f.kind = 'report';
