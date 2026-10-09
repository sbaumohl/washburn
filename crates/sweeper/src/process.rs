//! Per-job port of the CLI's `pipeline::process`, writing to Postgres instead
//! of the run directory. All SQL is in `washburn_common::repo`.

use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use cn_swarms::{
    api::{ApiError, redact_secrets},
    config::{Config as CrawlerConfig, Secret},
    crawl::{Crawler, Fetch, Job, Page},
    jev::{Jev, Screening},
    judge::{Judgment, RejectedResponse, chunks},
    sink,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sqlx::PgPool;
use tracing::{error, info, warn};
use url::Url;
use washburn_common::{
    JudgeRequest, QueueRow, ReportInput, StorePage, fail_job, insert_event, insert_judgment,
    insert_screening, level_one_section, serious_report, skip_job, store_page,
    upsert_level_one_finding, upsert_report_finding,
};

use crate::judge::{JudgeClient, JudgeFailure};

/// Attempts before a fetch or store failure becomes `failed` (the CLI's retry rule).
pub const MAX_ATTEMPTS: i32 = 3;

/// Everything one job needs. Built once per leased host and shared by its jobs.
pub struct JobContext {
    pub pool: PgPool,
    pub worker: String,
    pub crawler: Crawler,
    pub config: Arc<CrawlerConfig>,
    /// `None` under `--crawl-only`.
    pub jev: Option<Jev>,
    /// `None` under `--crawl-only`.
    pub judge: Option<JudgeClient>,
    /// Configured API keys and the Postgres password (D10).
    pub secrets: Vec<Secret>,
    /// Enabled seeds of this host, keyed by `queue.seed_id`.
    pub seeds: HashMap<i64, Url>,
}

/// What `process_job` reports in its one log line.
#[derive(Debug, Default)]
struct PageReport {
    disposition: &'static str,
    screenings: usize,
    level_one: usize,
    judgments: usize,
    errors: usize,
}

/// Runs one leased queue row to a terminal state. Never returns an error:
/// every failure is recorded on the row or as an event.
pub async fn process_job(ctx: &JobContext, row: QueueRow) {
    let id = row.id;
    let queued_url = row.url.clone();
    let job = match job_from_row(&row, &ctx.seeds) {
        Ok(job) => job,
        Err(error) => {
            let message = format!("{error:#}");
            fail(ctx, id, &message).await;
            emit(ctx, "job_error", json!({"url": queued_url, "error": message})).await;
            warn!(url = %queued_url, error = %message, "job could not be built");
            return;
        }
    };
    emit(
        ctx,
        "page_started",
        json!({"url": queued_url, "depth": job.depth, "seed": job.seed.as_str()}),
    )
    .await;

    let page = match ctx.crawler.fetch(job.clone()).await {
        Ok(Fetch::Page(page)) => page,
        Ok(Fetch::Skipped(reason)) => {
            if let Err(error) = skip_job(&ctx.pool, id, &scrub(&reason, &ctx.secrets)).await {
                error!(job = id, error = %error, "could not record skip");
            }
            emit(ctx, "page_skipped", json!({"url": queued_url, "reason": reason})).await;
            info!(url = %queued_url, disposition = "skipped", "page processed");
            return;
        }
        Err(error) => {
            let message = format!("fetch: {error:#}");
            fail(ctx, id, &message).await;
            emit(ctx, "fetch_error", json!({"url": queued_url, "error": message})).await;
            info!(url = %queued_url, disposition = "fetch_error", "page processed");
            return;
        }
    };

    let page_id = match store(ctx, &row, &queued_url, &page).await {
        Ok(page_id) => page_id,
        Err(error) if is_queue_unique_violation(&error) => {
            // Another attempt already stored this page; the queue row is done.
            info!(url = %queued_url, disposition = "already_stored", "page processed");
            return;
        }
        Err(error) => {
            let message = format!("store page: {error}");
            fail(ctx, id, &message).await;
            error!(url = %queued_url, error = %message, "page store failed");
            return;
        }
    };
    emit(
        ctx,
        "page_archived",
        json!({
            "url": queued_url,
            "final_url": page.final_url.as_str(),
            "id": sink::id(&job.url),
            "bytes": page.body.len(),
            "status": page.status,
        }),
    )
    .await;

    let report = match (&ctx.jev, &ctx.judge) {
        (Some(jev), Some(judge)) => triage(ctx, jev, judge, page_id, &page, &job).await,
        _ => PageReport {
            disposition: "unjudged",
            ..PageReport::default()
        },
    };
    info!(
        url = %queued_url,
        disposition = report.disposition,
        screenings = report.screenings,
        level_one = report.level_one,
        judgments = report.judgments,
        errors = report.errors,
        "page processed"
    );
}

/// Builds the crawler job for a queue row. The seed must be one of this host's enabled seeds.
pub fn job_from_row(row: &QueueRow, seeds: &HashMap<i64, Url>) -> Result<Job> {
    let url = Url::parse(&row.url).context("queue url")?;
    let seed = seeds
        .get(&row.seed_id)
        .cloned()
        .context("queue row's seed is not enabled for this host")?;
    let parent = row
        .parent_url
        .as_deref()
        .map(Url::parse)
        .transpose()
        .context("parent url")?;
    let depth = usize::try_from(row.depth).context("negative queue depth")?;
    Ok(Job {
        url,
        seed,
        parent,
        depth,
    })
}

/// In-scope links only, as the CLI's enqueue step filters them.
pub fn in_scope_links(crawler: &Crawler, links: &[Url]) -> Vec<Url> {
    links
        .iter()
        .filter(|link| crawler.in_scope(link))
        .cloned()
        .collect()
}

/// `pages` write. Blob, page row, queue `done`, and discovered links commit together.
async fn store(ctx: &JobContext, row: &QueueRow, queued_url: &str, page: &Page) -> sqlx::Result<i64> {
    let links = in_scope_links(&ctx.crawler, &page.links);
    let fetched_at = DateTime::parse_from_rfc3339(&page.fetched_at)
        .map(|time| time.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    let content_type = (!page.content_type.is_empty()).then_some(page.content_type.as_str());
    let max_depth = i32::try_from(ctx.config.max_depth).unwrap_or(i32::MAX);
    store_page(
        &ctx.pool,
        StorePage {
            worker: &ctx.worker,
            queue_id: row.id,
            url: queued_url,
            final_url: page.final_url.as_str(),
            seed_id: row.seed_id,
            depth: row.depth,
            status: i32::from(page.status),
            content_type,
            fetched_at,
            body: &page.body,
            text: &page.text,
            redirects: &page.redirects,
            links: &links,
            meta: json!({"title": page.title}),
            max_depth,
        },
    )
    .await
}

/// Jev screening, level-one findings, judge review, and the serious report.
async fn triage(
    ctx: &JobContext,
    jev: &Jev,
    judge: &JudgeClient,
    page_id: i64,
    page: &Page,
    job: &Job,
) -> PageReport {
    let url = job.url.to_string();
    let threshold = ctx.config.importance_threshold;
    let report_threshold = ctx.config.report_threshold;
    let title = scrub(&finding_title(&page.title, page.final_url.as_str()), &ctx.secrets);
    let mut judgments: Vec<Judgment> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut screenings = 0;
    let mut level_one = 0;

    for (index, text) in chunks(&page.text, ctx.config.chunk_chars).into_iter().enumerate() {
        if text.trim().is_empty() {
            continue;
        }
        let chunk = index + 1;
        let raw = match jev.evaluate(page, text, chunk).await {
            Ok(screening) => screening,
            Err(error) => {
                let message = format!("classifier chunk {chunk}: {error:#}");
                emit(
                    ctx,
                    "classifier_error",
                    json!({
                        "url": url,
                        "chunk": chunk,
                        "error": message,
                        "rejected_response": error.downcast_ref::<RejectedResponse>().map(|e| e.response.clone()),
                        "api_error": error.downcast_ref::<ApiError>().and_then(|e| serde_json::to_value(e).ok()),
                    }),
                )
                .await;
                errors.push(message);
                continue;
            }
        };
        let screening: Screening = match scrubbed(&raw, &ctx.secrets) {
            Ok(screening) => screening,
            Err(error) => {
                errors.push(format!("screening chunk {chunk}: {error:#}"));
                continue;
            }
        };
        let important = screening.important(threshold);
        let screening_id = match insert_screening(&ctx.pool, page_id, &screening, threshold).await {
            Ok(id) => id,
            Err(error) => {
                errors.push(format!("store screening chunk {chunk}: {error}"));
                continue;
            }
        };
        screenings += 1;
        emit(
            ctx,
            "classifier_completed",
            json!({"url": url, "screening": screening, "level_one": important}),
        )
        .await;
        if !important {
            continue;
        }
        level_one += 1;

        // Level-one evidence is persisted before the (slow, fallible) judge call.
        let section = scrub(&level_one_section(&screening), &ctx.secrets);
        if let Err(error) = upsert_level_one_finding(&ctx.pool, page_id, &title, &section).await {
            errors.push(format!("level-one finding chunk {chunk}: {error}"));
        }
        emit(
            ctx,
            "level_one_flagged",
            json!({"url": url, "chunk": chunk, "importance": screening.importance}),
        )
        .await;

        let request = JudgeRequest {
            final_url: page.final_url.clone(),
            fetched_at: page.fetched_at.clone(),
            status: page.status,
            title: page.title.clone(),
            text: text.to_owned(),
            chunk,
            screening: screening.clone(),
        };
        match judge.review(&request).await {
            Ok(judgment) => {
                let judgment: Judgment = match scrubbed(&judgment, &ctx.secrets) {
                    Ok(judgment) => judgment,
                    Err(error) => {
                        errors.push(format!("review chunk {chunk}: {error:#}"));
                        continue;
                    }
                };
                if let Err(error) = insert_judgment(&ctx.pool, page_id, screening_id, &judgment).await {
                    errors.push(format!("store judgment chunk {chunk}: {error}"));
                }
                emit(
                    ctx,
                    "judge_completed",
                    json!({
                        "url": url,
                        "chunk": chunk,
                        "scores": {
                            "activity": judgment.verdict.activity,
                            "seriousness": judgment.verdict.seriousness,
                            "confidence": judgment.verdict.confidence,
                        },
                        "classification": judgment.verdict.classification,
                    }),
                )
                .await;
                judgments.push(judgment);
            }
            Err(failure) => {
                emit_judge_error(ctx, &url, chunk, &failure).await;
                errors.push(format!("review chunk {chunk}: {}", failure.reason));
            }
        }
    }

    let disposition = if !errors.is_empty() {
        "triage_error"
    } else if level_one > 0 {
        "reviewed"
    } else if page.text.trim().is_empty() {
        "empty"
    } else {
        "screened"
    };
    // One report per page, rendered over every judgment so far (the CLI's shape).
    if judgments.iter().any(|j| j.verdict.report(report_threshold)) {
        let markdown = serious_report(&ReportInput {
            source: page.final_url.as_str(),
            seed: job.seed.as_str(),
            disposition,
            fetched: Some((&page.fetched_at, page.status)),
            judgments: &judgments,
            errors: &errors,
            report_threshold,
        });
        let markdown = scrub(&markdown, &ctx.secrets);
        if let Err(error) = upsert_report_finding(&ctx.pool, page_id, &title, &markdown).await {
            error!(url = %url, error = %error, "could not write serious report");
        }
    }

    PageReport {
        disposition,
        screenings,
        level_one,
        judgments: judgments.len(),
        errors: errors.len(),
    }
}

async fn emit_judge_error(ctx: &JobContext, url: &str, chunk: usize, failure: &JudgeFailure) {
    emit(
        ctx,
        "judge_error",
        json!({
            "kind": failure.kind,
            "reason": failure.reason,
            "status": failure.status,
            "chunk": chunk,
            "url": url,
            "api_error": failure.api_error,
        }),
    )
    .await;
}

/// Appends an event after scrubbing every string in its detail.
async fn emit(ctx: &JobContext, kind: &str, mut detail: Value) {
    scrub_detail(&mut detail, &ctx.secrets);
    if let Err(error) = insert_event(&ctx.pool, &ctx.worker, kind, &detail).await {
        warn!(event = kind, error = %error, "could not write event");
    }
}

async fn fail(ctx: &JobContext, id: i64, message: &str) {
    let message = scrub(message, &ctx.secrets);
    if let Err(error) = fail_job(&ctx.pool, id, &message, MAX_ATTEMPTS).await {
        error!(job = id, error = %error, "could not record job failure");
    }
}

/// Title for level-one and report findings: the page title, else the final URL.
pub fn finding_title(page_title: &str, final_url: &str) -> String {
    let trimmed = page_title.trim();
    if trimmed.is_empty() {
        final_url.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// True only for a unique violation on `pages.queue_id` (the page is already stored).
pub fn is_queue_unique_violation(error: &sqlx::Error) -> bool {
    error.as_database_error().is_some_and(|db| {
        unique_violation_on_pages_queue(db.code().as_deref(), db.constraint())
    })
}

pub fn unique_violation_on_pages_queue(code: Option<&str>, constraint: Option<&str>) -> bool {
    code == Some("23505") && constraint == Some("pages_queue_id_key")
}

/// Replaces configured secrets in every string and key of an event detail.
pub fn scrub_detail(detail: &mut Value, secrets: &[Secret]) {
    redact_secrets(detail, secrets);
}

pub fn scrub(text: &str, secrets: &[Secret]) -> String {
    let mut value = Value::String(text.to_owned());
    redact_secrets(&mut value, secrets);
    value.as_str().unwrap_or_default().to_owned()
}

/// Scrubs a serializable value by round-tripping it through JSON.
fn scrubbed<T: Serialize + DeserializeOwned>(value: &T, secrets: &[Secret]) -> Result<T> {
    let mut json = serde_json::to_value(value).context("encode for scrubbing")?;
    redact_secrets(&mut json, secrets);
    serde_json::from_value(json).context("decode after scrubbing")
}

#[cfg(test)]
mod tests {
    use super::*;
    use cn_swarms::config::Config as CrawlerConfig;
    use serde_json::json;

    fn secret(value: &str) -> Secret {
        serde_json::from_value(json!(value)).unwrap()
    }

    fn row(url: &str, parent: Option<&str>, depth: i32, seed_id: i64) -> QueueRow {
        QueueRow {
            id: 7,
            url: url.into(),
            url_hash: vec![0; 32],
            host: "example.test".into(),
            seed_id,
            parent_url: parent.map(str::to_owned),
            depth,
            status: "leased".into(),
            leased_by: Some("w".into()),
            lease_until: None,
            attempts: 0,
            last_error: None,
            created_at: Utc::now(),
            finished_at: None,
        }
    }

    #[test]
    fn job_from_row_uses_seed_parent_and_depth() {
        let mut seeds = HashMap::new();
        seeds.insert(3, Url::parse("https://example.test/").unwrap());
        let job = job_from_row(
            &row("https://example.test/b", Some("https://example.test/a"), 1, 3),
            &seeds,
        )
        .unwrap();
        assert_eq!(job.url.as_str(), "https://example.test/b");
        assert_eq!(job.seed.as_str(), "https://example.test/");
        assert_eq!(job.parent.unwrap().as_str(), "https://example.test/a");
        assert_eq!(job.depth, 1);

        let root = job_from_row(&row("https://example.test/", None, 0, 3), &seeds).unwrap();
        assert!(root.parent.is_none());
    }

    #[test]
    fn job_from_row_rejects_disabled_seed_and_negative_depth() {
        let seeds: HashMap<i64, Url> = HashMap::new();
        assert!(job_from_row(&row("https://example.test/", None, 0, 9), &seeds).is_err());
        let mut known = HashMap::new();
        known.insert(9, Url::parse("https://example.test/").unwrap());
        assert!(job_from_row(&row("https://example.test/", None, -1, 9), &known).is_err());
    }

    #[test]
    fn links_outside_seed_scope_are_dropped() {
        let config = Arc::new(CrawlerConfig::service_defaults());
        let crawler = Crawler::new(config, &[Url::parse("https://example.test/").unwrap()]).unwrap();
        let links = vec![
            Url::parse("https://example.test/a").unwrap(),
            Url::parse("https://example.test:8443/a").unwrap(),
            Url::parse("https://outside.invalid/").unwrap(),
            Url::parse("ftp://example.test/x").unwrap(),
        ];
        let kept = in_scope_links(&crawler, &links);
        assert_eq!(kept, vec![links[0].clone()]);
    }

    #[test]
    fn event_detail_is_scrubbed_in_values_and_keys() {
        let secrets = vec![secret("sk-live-123")];
        let mut detail = json!({
            "error": "auth failed for sk-live-123",
            "nested": {"sk-live-123": ["x sk-live-123 y"]},
            "chunk": 2,
        });
        scrub_detail(&mut detail, &secrets);
        let text = detail.to_string();
        assert!(!text.contains("sk-live-123"), "{text}");
        assert!(text.contains("[REDACTED]"));
        assert_eq!(detail["chunk"], 2);
    }

    #[test]
    fn scrub_text_and_scrubbed_roundtrip_remove_keys() {
        let secrets = vec![secret("sk-live-123")];
        assert_eq!(scrub("key=sk-live-123", &secrets), "key=[REDACTED]");
        let screening = Screening {
            chunk: 1,
            requested_model: "m".into(),
            model: "sk-live-123".into(),
            usage: Value::Null,
            importance: 0.9,
            agent_activity: 0.5,
            potentially_serious: 0.1,
        };
        let clean: Screening = scrubbed(&screening, &secrets).unwrap();
        assert_eq!(clean.model, "[REDACTED]");
        assert_eq!(clean.importance, 0.9);
    }

    #[test]
    fn only_pages_queue_unique_violation_counts_as_stored() {
        assert!(unique_violation_on_pages_queue(Some("23505"), Some("pages_queue_id_key")));
        assert!(!unique_violation_on_pages_queue(Some("23505"), Some("pages_url_key")));
        assert!(!unique_violation_on_pages_queue(Some("23503"), Some("pages_queue_id_key")));
        assert!(!unique_violation_on_pages_queue(None, None));
    }

    #[test]
    fn finding_title_prefers_page_title() {
        assert_eq!(finding_title("  Trace  ", "https://a.test/"), "Trace");
        assert_eq!(finding_title("   ", "https://a.test/"), "https://a.test/");
    }
}
