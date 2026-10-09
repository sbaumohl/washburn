use std::{
    collections::{HashSet, VecDeque},
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Context, Result};
use serde_json::json;
use tokio::{sync::Mutex, task::JoinSet};

use crate::{
    api::ApiError,
    config::{Config, Services},
    crawl::{Crawler, Fetch, Job},
    jev::Jev,
    judge::{Judge, RejectedResponse, chunks},
    sink::{Record, Sink, Summary},
    source,
};

#[derive(Clone)]
struct Triage {
    classifier: Jev,
    judge: Judge,
}

pub struct RunResult {
    pub directory: PathBuf,
    pub summary: Summary,
}

pub async fn run(config: Config) -> Result<RunResult> {
    config.validate()?;
    let targets = source::load(&config.input, config.column.as_deref())?;
    let services = config.config.as_deref().map(Services::load).transpose()?;
    let config = Arc::new(config);
    let crawler = Crawler::new(config.clone(), &targets.urls)?;
    let triage = if config.crawl_only {
        None
    } else {
        let settings = services
            .as_ref()
            .context("missing TOML services settings")?;
        let rubric = Arc::new(
            config
                .rubric
                .as_ref()
                .map(std::fs::read_to_string)
                .transpose()
                .context("read research rubric")?
                .unwrap_or_default(),
        );
        Some(Triage {
            classifier: Jev::new(&config, settings, rubric.clone())?,
            judge: Judge::new(&config, settings, rubric)?,
        })
    };
    let mut sink = Sink::new(&config, &targets, services.as_ref())?;
    sink.event(
        "run_started",
        json!({"seeds": targets.urls.len(), "rejected_targets": targets.rejected.len()}),
    )?;
    let directory = sink.root.clone();
    eprintln!(
        "Run: {} ({} seeds; {} rejected)",
        directory.display(),
        targets.urls.len(),
        targets.rejected.len()
    );
    let sink = Arc::new(Mutex::new(sink));
    let mut queue = VecDeque::new();
    let mut seen = HashSet::new();
    let mut summary = Summary {
        seeds_over_budget: targets.urls.len().saturating_sub(config.max_pages),
        ..Summary::default()
    };
    for url in targets.urls.into_iter().take(config.max_pages) {
        seen.insert(url.to_string());
        queue.push_back(Job {
            seed: url.clone(),
            url,
            parent: None,
            depth: 0,
        });
        summary.scheduled += 1;
    }
    let mut tasks = JoinSet::new();
    let interrupted = tokio::signal::ctrl_c();
    tokio::pin!(interrupted);
    loop {
        while tasks.len() < config.concurrency && !summary.interrupted {
            let Some(job) = queue.pop_front() else {
                break;
            };
            let crawler = crawler.clone();
            let triage = triage.clone();
            let config = config.clone();
            let sink = sink.clone();
            tasks.spawn(async move { process(job, crawler, triage, config, sink).await });
        }
        if tasks.is_empty() {
            break;
        }
        tokio::select! {
            signal = &mut interrupted, if !summary.interrupted => {
                signal.context("listen for Ctrl-C")?;
                summary.interrupted = true;
                eprintln!("Stopping scheduling; finishing {} active jobs", tasks.len());
                sink.lock().await.event("interrupt", json!({"active_jobs": tasks.len(), "queued_jobs": queue.len()}))?;
                // Account for queued URLs that were budgeted but never started.
                while let Some(job) = queue.pop_front() {
                    let record = Record { job, page: None, judgments: vec![], screenings: vec![], errors: vec![], disposition: "skipped".into(), skip_reason: Some("interrupted before starting".into()) };
                    sink.lock().await.record(&record, &config, &mut summary)?;
                }
            },
            completed = tasks.join_next() => {
                let record = match completed.context("missing worker result")?.context("worker failed")? {
                    Ok(record) => record,
                    Err(error) => {
                        tasks.abort_all();
                        sink.lock().await.event("run_failed", json!({"error": format!("{error:#}")}))?;
                        sink.lock().await.finish(&summary)?;
                        return Err(error);
                    }
                };
                sink.lock().await.record(&record, &config, &mut summary)?;
                eprintln!("[{}] {}: {}", summary.completed, record.job.url, record.disposition);
                if let Some(page) = record.page {
                    // Suppress separately following a destination already reached by a redirect.
                    seen.insert(page.final_url.to_string());
                    if page.job.depth < config.max_depth && !summary.interrupted {
                        for link in page.links {
                            if summary.scheduled == config.max_pages { break; }
                            if crawler.in_scope(&link) && seen.insert(link.to_string()) {
                                queue.push_back(Job { url: link, seed: page.job.seed.clone(), parent: Some(page.final_url.clone()), depth: page.job.depth + 1 });
                                summary.scheduled += 1;
                            }
                        }
                    }
                }
            },
        }
    }
    sink.lock().await.finish(&summary)?;
    Ok(RunResult { directory, summary })
}

async fn process(
    job: Job,
    crawler: Crawler,
    triage: Option<Triage>,
    config: Arc<Config>,
    sink: Arc<Mutex<Sink>>,
) -> Result<Record> {
    sink.lock().await.event(
        "page_started",
        json!({"url": job.url, "depth": job.depth, "seed": job.seed}),
    )?;
    let mut record = Record {
        job: job.clone(),
        page: None,
        judgments: vec![],
        screenings: vec![],
        errors: vec![],
        disposition: String::new(),
        skip_reason: None,
    };
    let page = match crawler.fetch(job).await {
        Ok(Fetch::Page(page)) => page,
        Ok(Fetch::Skipped(reason)) => {
            record.disposition = "skipped".into();
            record.skip_reason = Some(reason.clone());
            sink.lock().await.event(
                "page_skipped",
                json!({"url": record.job.url, "reason": reason}),
            )?;
            return Ok(record);
        }
        Err(error) => {
            record.disposition = "fetch_error".into();
            record.errors.push(format!("fetch: {error:#}"));
            return Ok(record);
        }
    };
    sink.lock().await.archive(&page)?;
    record.disposition = if triage.is_none() {
        "unjudged"
    } else if page.text.trim().is_empty() {
        "empty"
    } else {
        "screened"
    }
    .into();
    if let Some(triage) = triage {
        for (index, text) in chunks(&page.text, config.chunk_chars)
            .into_iter()
            .enumerate()
        {
            if text.trim().is_empty() {
                continue;
            }
            let chunk = index + 1;
            let screening = match triage.classifier.evaluate(&page, text, chunk).await {
                Ok(screening) => screening,
                Err(error) => {
                    let message = format!("classifier chunk {chunk}: {error:#}");
                    sink.lock().await.event("classifier_error", json!({"url": page.job.url, "chunk": chunk, "error": message,
                        "rejected_response": error.downcast_ref::<RejectedResponse>().map(|e| &e.response),
                        "api_error": error.downcast_ref::<ApiError>()}))?;
                    record.errors.push(message);
                    continue;
                }
            };
            let important = screening.important(config.importance_threshold);
            sink.lock().await.event(
                "classifier_completed",
                json!({"url": page.job.url, "screening": screening, "level_one": important}),
            )?;
            record.screenings.push(screening.clone());
            if important {
                // Persist level-one evidence before the potentially slow/failing LLM review.
                sink.lock().await.level_one(&page, &screening)?;
                record.disposition = "reviewed".into();
                match triage.judge.evaluate(&page, text, chunk, &screening).await {
                    Ok(review) => {
                        sink.lock().await.event(
                            "judge_completed",
                            json!({"url": page.job.url, "judgment": review}),
                        )?;
                        record.judgments.push(review);
                    }
                    Err(error) => {
                        let message = format!("review chunk {chunk}: {error:#}");
                        sink.lock().await.event("judge_error", json!({"url": page.job.url, "stage": "review", "chunk": chunk, "error": message,
                            "rejected_response": error.downcast_ref::<RejectedResponse>().map(|e| &e.response),
                            "api_error": error.downcast_ref::<ApiError>()}))?;
                        record.errors.push(message);
                    }
                }
            }
        }
        if !record.errors.is_empty() {
            record.disposition = "triage_error".into();
        }
    }
    record.page = Some(page);
    Ok(record)
}
