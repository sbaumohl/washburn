//! All SQL used by the sweeper, judge tooling, and tests (PROJECTS.md 3.4).
//!
//! Runtime-checked `sqlx::query*` only, so builds never need a database.
//! Enum columns are read as text (`status::text`) and written with `::enum`
//! casts. Lease durations are seconds as `f64` and go through `make_interval`.

use chrono::{DateTime, Utc};
use cn_swarms::{
    jev::Screening,
    judge::{Classification, Judgment},
    source,
};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use url::Url;

use crate::{blob, markdown, urls::canonical_url};

const QUEUE_COLUMNS: &str = "id, url, url_hash, host, seed_id, parent_url, depth, status::text AS status, leased_by, lease_until, attempts, last_error, created_at, finished_at";
const HOST_COLUMNS: &str =
    "host, leased_by, lease_until, pending_count, last_request_at, robots_blocked";
const PAGE_COLUMNS: &str = "id, queue_id, url, final_url, seed_id, depth, status, content_type, fetched_at, worker, body_sha256, text, redirects, links, meta";
const SCREENING_COLUMNS: &str = "id, page_id, chunk, requested_model, model, importance, agent_activity, potentially_serious, level_one, threshold, usage, created_at";
const JUDGMENT_COLUMNS: &str = "id, page_id, screening_id, chunk, requested_model, model, activity, seriousness, confidence, classification::text AS classification, confirmed_serious, summary, evidence, uncertainties, usage, verdict, created_at";
const FINDING_COLUMNS: &str = "id, page_id, kind::text AS kind, title, markdown, created_at, updated_at, reviewed_by, reviewed_at, review_note, published, published_at";
const HEARTBEAT_COLUMNS: &str =
    "worker, hostname, pid, version, started_at, last_seen, active_jobs, leased_host, detail";

// ---------------------------------------------------------------- rows

#[derive(Debug, Clone, FromRow)]
pub struct QueueRow {
    pub id: i64,
    pub url: String,
    pub url_hash: Vec<u8>,
    pub host: String,
    pub seed_id: i64,
    pub parent_url: Option<String>,
    pub depth: i32,
    /// One of `pending`, `leased`, `done`, `failed`, `skipped`.
    pub status: String,
    pub leased_by: Option<String>,
    pub lease_until: Option<DateTime<Utc>>,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct HostRow {
    pub host: String,
    pub leased_by: Option<String>,
    pub lease_until: Option<DateTime<Utc>>,
    pub pending_count: i32,
    pub last_request_at: Option<DateTime<Utc>>,
    pub robots_blocked: bool,
}

#[derive(Debug, Clone, FromRow)]
pub struct PageRow {
    pub id: i64,
    pub queue_id: i64,
    pub url: String,
    pub final_url: String,
    pub seed_id: i64,
    pub depth: i32,
    pub status: i32,
    pub content_type: Option<String>,
    pub fetched_at: DateTime<Utc>,
    pub worker: String,
    pub body_sha256: Vec<u8>,
    pub text: String,
    pub redirects: Value,
    pub links: Value,
    pub meta: Value,
}

#[derive(Debug, Clone, FromRow)]
pub struct ScreeningRow {
    pub id: i64,
    pub page_id: i64,
    pub chunk: i32,
    pub requested_model: String,
    pub model: String,
    pub importance: f64,
    pub agent_activity: f64,
    pub potentially_serious: f64,
    pub level_one: bool,
    pub threshold: f64,
    pub usage: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct JudgmentRow {
    pub id: i64,
    pub page_id: i64,
    pub screening_id: i64,
    pub chunk: i32,
    pub requested_model: String,
    pub model: String,
    pub activity: f64,
    pub seriousness: f64,
    pub confidence: f64,
    /// One of `no_signal`, `authorized`, `unclear`, `suspected_unauthorized`.
    pub classification: String,
    pub confirmed_serious: bool,
    pub summary: String,
    pub evidence: Value,
    pub uncertainties: Value,
    pub usage: Value,
    pub verdict: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
pub struct FindingRow {
    pub id: i64,
    pub page_id: i64,
    /// `level_one` or `report`.
    pub kind: String,
    pub title: String,
    pub markdown: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub review_note: Option<String>,
    pub published: bool,
    pub published_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
pub struct HeartbeatRow {
    pub worker: String,
    pub hostname: String,
    pub pid: i32,
    pub version: String,
    pub started_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub active_jobs: i32,
    pub leased_host: Option<String>,
    pub detail: Value,
}

// ---------------------------------------------------------------- helpers

/// The `hosts.host` key for a URL: lowercase `host` plus `:port` when explicit.
/// Matches the crawler's scope rule (`host_str` + `port()`).
pub fn host_key(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    }
}

/// `sha256(url)` as raw bytes, the `queue.url_hash` value.
pub fn url_hash(url: &str) -> Vec<u8> {
    blob::sha256(url.as_bytes()).to_vec()
}

fn invalid(message: impl Into<String>) -> sqlx::Error {
    sqlx::Error::Protocol(message.into())
}

fn int32(value: usize) -> sqlx::Result<i32> {
    i32::try_from(value).map_err(|error| invalid(error.to_string()))
}

fn classification_label(class: &Classification) -> &'static str {
    match class {
        Classification::NoSignal => "no_signal",
        Classification::Authorized => "authorized",
        Classification::Unclear => "unclear",
        Classification::SuspectedUnauthorized => "suspected_unauthorized",
    }
}

async fn bump_pending(conn: &mut sqlx::PgConnection, host: &str, delta: i32) -> sqlx::Result<()> {
    sqlx::query("UPDATE hosts SET pending_count = pending_count + $2 WHERE host = $1")
        .bind(host)
        .bind(delta)
        .execute(conn)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------- seeds

/// Inserts (or finds) a seed, its host, and a depth-0 queue row.
/// `pending_count` is bumped only when the queue row is newly inserted.
/// Returns the seed id.
pub async fn add_seed(
    pool: &PgPool,
    url: &str,
    note: Option<&str>,
    added_by: Option<&str>,
) -> sqlx::Result<i64> {
    let url = source::normalize(url).map_err(|error| invalid(format!("{error:#}")))?;
    let url = canonical_url(url.as_str()).map_err(|error| invalid(format!("{error:#}")))?;
    let host = host_key(&url);
    let mut tx = pool.begin().await?;
    let seed_id: i64 = sqlx::query_scalar(
        "INSERT INTO seeds (url, host, note, added_by) VALUES ($1, $2, $3, $4)
         ON CONFLICT (url) DO UPDATE SET url = EXCLUDED.url
         RETURNING id",
    )
    .bind(url.as_str())
    .bind(&host)
    .bind(note)
    .bind(added_by)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO hosts (host) VALUES ($1) ON CONFLICT (host) DO NOTHING")
        .bind(&host)
        .execute(&mut *tx)
        .await?;
    let inserted = sqlx::query(
        "INSERT INTO queue (url, url_hash, host, seed_id, parent_url, depth)
         VALUES ($1, $2, $3, $4, NULL, 0)
         ON CONFLICT (url) DO NOTHING",
    )
    .bind(url.as_str())
    .bind(url_hash(url.as_str()))
    .bind(&host)
    .bind(seed_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if inserted > 0 {
        bump_pending(&mut tx, &host, 1).await?;
    }
    tx.commit().await?;
    Ok(seed_id)
}

/// Enabled seed URLs whose scope is `host` (the crawler's `Crawler::new` input).
pub async fn seed_urls_for_host(pool: &PgPool, host: &str) -> sqlx::Result<Vec<Url>> {
    let rows: Vec<String> =
        sqlx::query_scalar("SELECT url FROM seeds WHERE host = $1 AND enabled ORDER BY id")
            .bind(host)
            .fetch_all(pool)
            .await?;
    rows.iter()
        .map(|raw| Url::parse(raw).map_err(|error| invalid(format!("seed url: {error}"))))
        .collect()
}

// ---------------------------------------------------------------- leasing

/// Leases the least-recently-leased host with pending work. Sets
/// `last_request_at` so hosts rotate. Returns the host, or `None` if none is free.
pub async fn lease_host(
    pool: &PgPool,
    worker: &str,
    lease_secs: f64,
) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar(
        "UPDATE hosts
         SET leased_by = $1,
             lease_until = now() + make_interval(secs => $2),
             last_request_at = now()
         WHERE host = (
             SELECT host FROM hosts
             WHERE pending_count > 0 AND (lease_until IS NULL OR lease_until < now())
             ORDER BY last_request_at NULLS FIRST
             LIMIT 1
             FOR UPDATE SKIP LOCKED)
         RETURNING host",
    )
    .bind(worker)
    .bind(lease_secs)
    .fetch_optional(pool)
    .await
}

/// Extends a host lease this worker holds. Returns `false` if the lease was lost.
pub async fn renew_host(
    pool: &PgPool,
    worker: &str,
    host: &str,
    lease_secs: f64,
) -> sqlx::Result<bool> {
    let result = sqlx::query(
        "UPDATE hosts SET lease_until = now() + make_interval(secs => $3)
         WHERE host = $2 AND leased_by = $1",
    )
    .bind(worker)
    .bind(host)
    .bind(lease_secs)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Releases a host lease this worker holds.
pub async fn release_host(pool: &PgPool, worker: &str, host: &str) -> sqlx::Result<bool> {
    let result = sqlx::query(
        "UPDATE hosts SET leased_by = NULL, lease_until = NULL
         WHERE host = $2 AND leased_by = $1",
    )
    .bind(worker)
    .bind(host)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Leases the oldest pending URL on `host`, or `None` if the host has none.
pub async fn lease_job(
    pool: &PgPool,
    worker: &str,
    host: &str,
    lease_secs: f64,
) -> sqlx::Result<Option<QueueRow>> {
    sqlx::query_as(&format!(
        "UPDATE queue
         SET status = 'leased', leased_by = $1, lease_until = now() + make_interval(secs => $3)
         WHERE id = (
             SELECT id FROM queue
             WHERE host = $2 AND status = 'pending'
               AND seed_id IN (SELECT id FROM seeds WHERE enabled)
             ORDER BY id
             LIMIT 1
             FOR UPDATE SKIP LOCKED)
         RETURNING {QUEUE_COLUMNS}"
    ))
    .bind(worker)
    .bind(host)
    .bind(lease_secs)
    .fetch_optional(pool)
    .await
}

// ---------------------------------------------------------------- job outcomes

/// Records a fetch error. The job returns to `pending` until `max_attempts`
/// is reached, then becomes `failed` and leaves the host's pending count.
pub async fn fail_job(pool: &PgPool, id: i64, error: &str, max_attempts: i32) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    let row: Option<(String, String)> = sqlx::query_as(
        "UPDATE queue
         SET attempts = attempts + 1,
             last_error = $2,
             leased_by = NULL,
             lease_until = NULL,
             status = CASE WHEN attempts + 1 < $3 THEN 'pending'::queue_status ELSE 'failed'::queue_status END,
             finished_at = CASE WHEN attempts + 1 < $3 THEN NULL ELSE now() END
         WHERE id = $1
         RETURNING host, status::text",
    )
    .bind(id)
    .bind(error)
    .bind(max_attempts)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((host, status)) = row
        && status == "failed"
    {
        bump_pending(&mut tx, &host, -1).await?;
    }
    tx.commit().await
}

/// Marks a job `skipped` (for example robots.txt) and drops it from the host's pending count.
pub async fn skip_job(pool: &PgPool, id: i64, reason: &str) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    let host: Option<String> = sqlx::query_scalar(
        "UPDATE queue
         SET status = 'skipped', last_error = $2, finished_at = now(), leased_by = NULL, lease_until = NULL
         WHERE id = $1 AND status IN ('pending', 'leased')
         RETURNING host",
    )
    .bind(id)
    .bind(reason)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(host) = host {
        bump_pending(&mut tx, &host, -1).await?;
    }
    tx.commit().await
}

/// Everything `store_page` writes for one fetched page.
#[derive(Debug, Clone)]
pub struct StorePage<'a> {
    pub worker: &'a str,
    /// The leased `queue.id` this page answers.
    pub queue_id: i64,
    /// The queued URL (`pages.url`).
    pub url: &'a str,
    pub final_url: &'a str,
    pub seed_id: i64,
    pub depth: i32,
    pub status: i32,
    pub content_type: Option<&'a str>,
    pub fetched_at: DateTime<Utc>,
    /// Raw body; hashed and zstd-compressed into `blobs`.
    pub body: &'a [u8],
    pub text: &'a str,
    pub redirects: &'a [Url],
    /// In-scope links discovered on the page. The caller applies the crawler's scope rule.
    pub links: &'a [Url],
    /// Anything else from the crawl (for example `{"title": ...}`), stored in `pages.meta`.
    pub meta: Value,
    /// Links are enqueued only while `depth < max_depth`.
    pub max_depth: i32,
}

/// Persists a fetched page in one transaction: blob, page row, queue `done`,
/// and newly discovered links. Returns the page id.
pub async fn store_page(pool: &PgPool, page: StorePage<'_>) -> sqlx::Result<i64> {
    let mut tx = pool.begin().await?;
    let sha = blob::sha256(page.body);
    sqlx::query(
        "INSERT INTO blobs (sha256, bytes, raw_len) VALUES ($1, $2, $3)
         ON CONFLICT (sha256) DO NOTHING",
    )
    .bind(&sha[..])
    .bind(blob::compress(page.body))
    .bind(int32(page.body.len())?)
    .execute(&mut *tx)
    .await?;

    let page_id: i64 = sqlx::query_scalar(
        "INSERT INTO pages (queue_id, url, final_url, seed_id, depth, status, content_type,
                            fetched_at, worker, body_sha256, text, redirects, links, meta)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
         RETURNING id",
    )
    .bind(page.queue_id)
    .bind(page.url)
    .bind(page.final_url)
    .bind(page.seed_id)
    .bind(page.depth)
    .bind(page.status)
    .bind(page.content_type)
    .bind(page.fetched_at)
    .bind(page.worker)
    .bind(&sha[..])
    .bind(page.text)
    .bind(serde_json::to_value(page.redirects).map_err(|e| invalid(e.to_string()))?)
    .bind(serde_json::to_value(page.links).map_err(|e| invalid(e.to_string()))?)
    .bind(&page.meta)
    .fetch_one(&mut *tx)
    .await?;

    let done_host: Option<String> = sqlx::query_scalar(
        "UPDATE queue SET status = 'done', finished_at = now(), leased_by = NULL, lease_until = NULL
         WHERE id = $1 AND status IN ('pending', 'leased')
         RETURNING host",
    )
    .bind(page.queue_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(host) = done_host {
        bump_pending(&mut tx, &host, -1).await?;
    }

    let links: Vec<Url> = page
        .links
        .iter()
        .filter_map(|link| canonical_url(link.as_str()).ok())
        .collect();
    if page.depth < page.max_depth && !links.is_empty() {
        let child_depth = page.depth + 1;
        let urls: Vec<String> = links.iter().map(|u| u.as_str().to_owned()).collect();
        let hashes: Vec<Vec<u8>> = urls.iter().map(|u| url_hash(u)).collect();
        let hosts: Vec<String> = links.iter().map(host_key).collect();
        sqlx::query(
            "INSERT INTO hosts (host) SELECT DISTINCT h FROM unnest($1::text[]) AS t(h)
             ON CONFLICT (host) DO NOTHING",
        )
        .bind(&hosts)
        .execute(&mut *tx)
        .await?;
        // One statement: insert links, count inserted rows per host, bump pending_count.
        sqlx::query(
            "WITH ins AS (
                 INSERT INTO queue (url, url_hash, host, seed_id, parent_url, depth)
                 SELECT u.url, u.hash, u.host, $4::bigint, $5::text, $6::int
                 FROM unnest($1::text[], $2::bytea[], $3::text[]) AS u(url, hash, host)
                 ON CONFLICT (url) DO NOTHING
                 RETURNING host
             ), per_host AS (
                 SELECT host, count(*)::int AS n FROM ins GROUP BY host
             )
             UPDATE hosts SET pending_count = hosts.pending_count + per_host.n
             FROM per_host WHERE hosts.host = per_host.host",
        )
        .bind(&urls)
        .bind(&hashes)
        .bind(&hosts)
        .bind(page.seed_id)
        .bind(page.final_url)
        .bind(child_depth)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(page_id)
}

// ---------------------------------------------------------------- judging and findings

/// Records one Jev screening. `level_one` is `importance >= threshold` at write time.
pub async fn insert_screening(
    pool: &PgPool,
    page_id: i64,
    screening: &Screening,
    threshold: f64,
) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "INSERT INTO screenings (page_id, chunk, requested_model, model, importance, agent_activity,
                                 potentially_serious, level_one, threshold, usage)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         RETURNING id",
    )
    .bind(page_id)
    .bind(int32(screening.chunk)?)
    .bind(&screening.requested_model)
    .bind(&screening.model)
    .bind(screening.importance)
    .bind(screening.agent_activity)
    .bind(screening.potentially_serious)
    .bind(screening.important(threshold))
    .bind(threshold)
    .bind(&screening.usage)
    .fetch_one(pool)
    .await
}

/// Records one strong-judge result. `stage` is not stored: the schema has no column for it.
pub async fn insert_judgment(
    pool: &PgPool,
    page_id: i64,
    screening_id: i64,
    judgment: &Judgment,
) -> sqlx::Result<i64> {
    let verdict = &judgment.verdict;
    sqlx::query_scalar(
        "INSERT INTO judgments (page_id, screening_id, chunk, requested_model, model, activity,
                                seriousness, confidence, classification, confirmed_serious, summary,
                                evidence, uncertainties, usage, verdict)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::classification, $10, $11, $12, $13, $14, $15)
         RETURNING id",
    )
    .bind(page_id)
    .bind(screening_id)
    .bind(int32(judgment.chunk)?)
    .bind(&judgment.requested_model)
    .bind(&judgment.model)
    .bind(verdict.activity)
    .bind(verdict.seriousness)
    .bind(verdict.confidence)
    .bind(classification_label(&verdict.classification))
    .bind(verdict.confirmed_serious)
    .bind(&verdict.summary)
    .bind(serde_json::to_value(&verdict.evidence).map_err(|e| invalid(e.to_string()))?)
    .bind(serde_json::to_value(&verdict.uncertainties).map_err(|e| invalid(e.to_string()))?)
    .bind(&judgment.usage)
    .bind(serde_json::to_value(verdict).map_err(|e| invalid(e.to_string()))?)
    .fetch_one(pool)
    .await
}

/// Creates the page's `level_one` finding, or appends `markdown_section` to it.
/// A new finding gets the CLI's header followed by the section.
pub async fn upsert_level_one_finding(
    pool: &PgPool,
    page_id: i64,
    title: &str,
    markdown_section: &str,
) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    let (final_url, fetched_at): (String, DateTime<Utc>) =
        sqlx::query_as("SELECT final_url, fetched_at FROM pages WHERE id = $1")
            .bind(page_id)
            .fetch_one(&mut *tx)
            .await?;
    let header = markdown::level_one_header(&final_url, &fetched_at.to_rfc3339());
    sqlx::query(
        "INSERT INTO findings (page_id, kind, title, markdown)
         VALUES ($1, 'level_one', $2, $3)
         ON CONFLICT (page_id, kind)
         DO UPDATE SET markdown = findings.markdown || $4, updated_at = now()",
    )
    .bind(page_id)
    .bind(title)
    .bind(format!("{header}{markdown_section}"))
    .bind(markdown_section)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

/// Creates or replaces the page's `report` finding. Publication flags are untouched.
pub async fn upsert_report_finding(
    pool: &PgPool,
    page_id: i64,
    title: &str,
    markdown: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO findings (page_id, kind, title, markdown)
         VALUES ($1, 'report', $2, $3)
         ON CONFLICT (page_id, kind)
         DO UPDATE SET title = EXCLUDED.title, markdown = EXCLUDED.markdown, updated_at = now()",
    )
    .bind(page_id)
    .bind(title)
    .bind(markdown)
    .execute(pool)
    .await?;
    Ok(())
}

/// Enabled seeds whose scope is `host`, with their ids. Used to pair each
/// queue row's `seed_id` with the seed URL the crawler was scoped by.
pub async fn seeds_by_id_for_host(pool: &PgPool, host: &str) -> sqlx::Result<Vec<(i64, Url)>> {
    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, url FROM seeds WHERE host = $1 AND enabled ORDER BY id")
            .bind(host)
            .fetch_all(pool)
            .await?;
    rows.into_iter()
        .map(|(id, raw)| {
            Url::parse(&raw)
                .map(|url| (id, url))
                .map_err(|error| invalid(format!("seed url: {error}")))
        })
        .collect()
}

/// Returns jobs whose lease expired back to `pending`. Returns the rows changed.
pub async fn reclaim_stale_jobs(pool: &PgPool) -> sqlx::Result<u64> {
    let result = sqlx::query(
        "UPDATE queue SET status = 'pending', leased_by = NULL, lease_until = NULL
         WHERE status = 'leased' AND lease_until < now()",
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

// ---------------------------------------------------------------- telemetry

/// Appends an `events` row. The caller scrubs credentials from `detail`.
pub async fn insert_event(
    pool: &PgPool,
    worker: &str,
    kind: &str,
    detail: &Value,
) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO events (worker, kind, detail) VALUES ($1, $2, $3)")
        .bind(worker)
        .bind(kind)
        .bind(detail)
        .execute(pool)
        .await?;
    Ok(())
}

/// Upserts this worker's heartbeat row. `row.last_seen` is set by the caller.
pub async fn heartbeat(pool: &PgPool, row: &HeartbeatRow) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO heartbeats (worker, hostname, pid, version, started_at, last_seen, active_jobs, leased_host, detail)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (worker) DO UPDATE SET
             version = EXCLUDED.version,
             last_seen = EXCLUDED.last_seen,
             active_jobs = EXCLUDED.active_jobs,
             leased_host = EXCLUDED.leased_host,
             detail = EXCLUDED.detail",
    )
    .bind(&row.worker)
    .bind(&row.hostname)
    .bind(row.pid)
    .bind(&row.version)
    .bind(row.started_at)
    .bind(row.last_seen)
    .bind(row.active_jobs)
    .bind(&row.leased_host)
    .bind(&row.detail)
    .execute(pool)
    .await?;
    Ok(())
}

/// Removes this worker's heartbeat on clean shutdown.
pub async fn delete_heartbeat(pool: &PgPool, worker: &str) -> sqlx::Result<()> {
    sqlx::query("DELETE FROM heartbeats WHERE worker = $1")
        .bind(worker)
        .execute(pool)
        .await?;
    Ok(())
}

/// Pending URLs per host, from the `queue_depth` view.
pub async fn queue_depth(pool: &PgPool) -> sqlx::Result<Vec<(String, i64)>> {
    sqlx::query_as("SELECT host, pending FROM queue_depth ORDER BY host")
        .fetch_all(pool)
        .await
}

// ---------------------------------------------------------------- reads

pub async fn get_queue(pool: &PgPool, id: i64) -> sqlx::Result<Option<QueueRow>> {
    sqlx::query_as(&format!("SELECT {QUEUE_COLUMNS} FROM queue WHERE id = $1"))
        .bind(id)
        .fetch_optional(pool)
        .await
}

pub async fn get_host(pool: &PgPool, host: &str) -> sqlx::Result<Option<HostRow>> {
    sqlx::query_as(&format!("SELECT {HOST_COLUMNS} FROM hosts WHERE host = $1"))
        .bind(host)
        .fetch_optional(pool)
        .await
}

pub async fn page_for_queue(pool: &PgPool, queue_id: i64) -> sqlx::Result<Option<PageRow>> {
    sqlx::query_as(&format!(
        "SELECT {PAGE_COLUMNS} FROM pages WHERE queue_id = $1"
    ))
    .bind(queue_id)
    .fetch_optional(pool)
    .await
}

pub async fn screenings_for_page(pool: &PgPool, page_id: i64) -> sqlx::Result<Vec<ScreeningRow>> {
    sqlx::query_as(&format!(
        "SELECT {SCREENING_COLUMNS} FROM screenings WHERE page_id = $1 ORDER BY chunk"
    ))
    .bind(page_id)
    .fetch_all(pool)
    .await
}

pub async fn judgments_for_page(pool: &PgPool, page_id: i64) -> sqlx::Result<Vec<JudgmentRow>> {
    sqlx::query_as(&format!(
        "SELECT {JUDGMENT_COLUMNS} FROM judgments WHERE page_id = $1 ORDER BY chunk"
    ))
    .bind(page_id)
    .fetch_all(pool)
    .await
}

pub async fn findings_for_page(pool: &PgPool, page_id: i64) -> sqlx::Result<Vec<FindingRow>> {
    sqlx::query_as(&format!(
        "SELECT {FINDING_COLUMNS} FROM findings WHERE page_id = $1 ORDER BY id"
    ))
    .bind(page_id)
    .fetch_all(pool)
    .await
}

pub async fn heartbeats(pool: &PgPool) -> sqlx::Result<Vec<HeartbeatRow>> {
    sqlx::query_as(&format!(
        "SELECT {HEARTBEAT_COLUMNS} FROM heartbeats ORDER BY worker"
    ))
    .fetch_all(pool)
    .await
}
