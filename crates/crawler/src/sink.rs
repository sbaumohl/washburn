use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::PathBuf,
};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{
    config::{Config, Services},
    crawl::{Job, Page},
    jev::Screening,
    judge::Judgment,
    source::Targets,
};

#[derive(Debug, Serialize)]
pub struct Record {
    pub job: Job,
    pub page: Option<Box<Page>>,
    pub judgments: Vec<Judgment>,
    pub screenings: Vec<Screening>,
    pub errors: Vec<String>,
    pub disposition: String,
    pub skip_reason: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct Summary {
    pub scheduled: usize,
    pub completed: usize,
    pub fetched: usize,
    pub skipped: usize,
    pub failed: usize,
    pub findings: usize,
    pub reports: usize,
    pub classifier_calls: usize,
    pub important_chunks: usize,
    pub review_calls: usize,
    pub interrupted: bool,
    pub seeds_over_budget: usize,
}

pub struct Sink {
    pub root: PathBuf,
    csv: csv::Writer<File>,
    events: File,
}

pub fn id(url: &url::Url) -> String {
    format!("{:x}", Sha256::digest(url.as_str().as_bytes()))
}

impl Sink {
    pub fn new(config: &Config, targets: &Targets, services: Option<&Services>) -> Result<Self> {
        fs::create_dir_all(&config.output).context("create output directory")?;
        let run = format!(
            "{}-{}",
            chrono::Utc::now().format("%Y%m%dT%H%M%S%.9fZ"),
            std::process::id()
        );
        let root = config.output.join(run);
        fs::create_dir(&root)?;
        for dir in ["pages", "decisions", "findings", "reports"] {
            fs::create_dir(root.join(dir))?;
        }
        let mut csv = csv::Writer::from_path(root.join("results.csv"))?;
        csv.write_record([
            "id",
            "seed",
            "parent",
            "url",
            "final_url",
            "depth",
            "fetched_at",
            "http_status",
            "content_type",
            "title",
            "disposition",
            "skip_reason",
            "importance",
            "activity",
            "seriousness",
            "confidence",
            "classification",
            "summary",
            "classifier_chunks",
            "review_chunks",
            "raw_path",
            "text_path",
            "decision_path",
            "finding_path",
            "report_path",
            "errors",
        ])?;
        csv.flush()?;
        let manifest = json!({
            "started_at": chrono::Utc::now().to_rfc3339(), "version": env!("CARGO_PKG_VERSION"),
            "input": config.input, "column": config.column, "seeds": targets.urls,
            "crawl": {"concurrency": config.concurrency, "max_pages": config.max_pages, "max_depth": config.max_depth,
                "delay_ms": config.delay_ms, "timeout_secs": config.timeout_secs, "retries": config.retries,
                "max_body_bytes": config.max_body_bytes, "max_redirects": config.max_redirects, "user_agent": config.user_agent,
                "ignore_robots": config.ignore_robots, "scope": "seed hosts and explicit ports; HTTP and HTTPS allowed"},
            "triage": {"crawl_only": config.crawl_only,
                "classifier_model": services.map(|s| &s.typesafe.model),
                "review_model": services.map(|s| &s.openrouter.model),
                "classifier_concurrency": config.classifier_concurrency,
                "judge_concurrency": config.judge_concurrency, "timeout_secs": config.judge_timeout_secs,
                "chunk_chars": config.chunk_chars, "importance_threshold": config.importance_threshold,
                "report_threshold": config.report_threshold},
        });
        fs::write(
            root.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest)?,
        )?;
        fs::write(
            root.join("rejected-targets.json"),
            serde_json::to_vec_pretty(&targets.rejected)?,
        )?;
        if let Some(rubric) = &config.rubric {
            fs::copy(rubric, root.join("rubric.txt"))?;
        }
        let events = File::create(root.join("events.jsonl"))?;
        Ok(Self { root, csv, events })
    }

    pub fn event(&mut self, kind: &str, detail: Value) -> Result<()> {
        serde_json::to_writer(
            &mut self.events,
            &json!({"at": chrono::Utc::now().to_rfc3339(), "event": kind, "detail": detail}),
        )?;
        self.events.write_all(b"\n")?;
        self.events.flush()?;
        Ok(())
    }

    pub fn archive(&mut self, page: &Page) -> Result<()> {
        let key = id(&page.job.url);
        fs::write(self.root.join(format!("pages/{key}.body")), &page.body)?;
        fs::write(self.root.join(format!("pages/{key}.txt")), &page.text)?;
        fs::write(
            self.root.join(format!("pages/{key}.json")),
            serde_json::to_vec_pretty(page)?,
        )?;
        self.event("page_archived", json!({"url": page.job.url, "final_url": page.final_url, "id": key, "bytes": page.body.len(), "status": page.status}))
    }

    pub fn level_one(&mut self, page: &Page, screening: &Screening) -> Result<()> {
        let path = self.root.join(format!("findings/{}.md", id(&page.job.url)));
        let exists = path.exists();
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        if !exists {
            file.write_all(level_one_header(&page.final_url, &page.fetched_at).as_bytes())?;
        }
        file.write_all(level_one_section(screening).as_bytes())?;
        file.flush()?;
        self.event("level_one_flagged", json!({"url": page.job.url, "chunk": screening.chunk, "importance": screening.importance}))
    }

    pub fn record(
        &mut self,
        record: &Record,
        config: &Config,
        summary: &mut Summary,
    ) -> Result<()> {
        let key = id(&record.job.url);
        let decision_path = format!("decisions/{key}.json");
        fs::write(
            self.root.join(&decision_path),
            serde_json::to_vec_pretty(record)?,
        )?;
        summary.completed += 1;
        summary.fetched += usize::from(record.page.is_some());
        summary.skipped += usize::from(record.disposition == "skipped");
        summary.failed += usize::from(!record.errors.is_empty());
        let screened = record.screenings.len();
        let reviewed = record.judgments.len();
        summary.classifier_calls += screened;
        summary.review_calls += reviewed;
        summary.important_chunks += record
            .screenings
            .iter()
            .filter(|s| s.important(config.importance_threshold))
            .count();
        let finding = record
            .screenings
            .iter()
            .any(|s| s.important(config.importance_threshold));
        let report = record
            .judgments
            .iter()
            .any(|j| j.stage == "review" && j.verdict.report(config.report_threshold));
        let finding_path = if finding {
            summary.findings += 1;
            let path = format!("findings/{key}.md");
            fs::write(
                self.root.join(&path),
                render(record, false, config.report_threshold),
            )?;
            path
        } else {
            String::new()
        };
        let report_path = if report {
            summary.reports += 1;
            let path = format!("reports/{key}.md");
            fs::write(
                self.root.join(&path),
                render(record, true, config.report_threshold),
            )?;
            path
        } else {
            String::new()
        };
        // The CSV holds the strongest review plus the highest Jev importance.
        let importance = record
            .screenings
            .iter()
            .map(|s| s.importance)
            .max_by(f64::total_cmp);
        let best = record.judgments.iter().max_by(|a, b| {
            a.verdict
                .seriousness
                .total_cmp(&b.verdict.seriousness)
                .then(a.verdict.activity.total_cmp(&b.verdict.activity))
        });
        let page = record.page.as_deref();
        let score = |f: fn(&crate::judge::Verdict) -> f64| {
            best.map(|j| f(&j.verdict).to_string()).unwrap_or_default()
        };
        let classification = best
            .map(|j| {
                serde_json::to_value(&j.verdict.classification)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .unwrap_or_default();
        self.csv.write_record([
            key.clone(),
            record.job.seed.to_string(),
            record
                .job
                .parent
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            record.job.url.to_string(),
            page.map(|p| p.final_url.to_string()).unwrap_or_default(),
            record.job.depth.to_string(),
            page.map(|p| p.fetched_at.clone()).unwrap_or_default(),
            page.map(|p| p.status.to_string()).unwrap_or_default(),
            page.map(|p| p.content_type.clone()).unwrap_or_default(),
            page.map(|p| p.title.clone()).unwrap_or_default(),
            record.disposition.clone(),
            record.skip_reason.clone().unwrap_or_default(),
            importance.map(|v| v.to_string()).unwrap_or_default(),
            score(|v| v.activity),
            score(|v| v.seriousness),
            score(|v| v.confidence),
            classification,
            best.map(|j| j.verdict.summary.clone()).unwrap_or_default(),
            screened.to_string(),
            reviewed.to_string(),
            page.map(|_| format!("pages/{key}.body"))
                .unwrap_or_default(),
            page.map(|_| format!("pages/{key}.txt")).unwrap_or_default(),
            decision_path,
            finding_path,
            report_path,
            record.errors.join("; "),
        ])?;
        self.csv.flush()?;
        self.event("page_completed", json!({"url": record.job.url, "disposition": record.disposition, "finding": finding, "report": report, "errors": record.errors}))
    }

    pub fn finish(&mut self, summary: &Summary) -> Result<()> {
        self.csv.flush()?;
        fs::write(
            self.root.join("summary.json"),
            serde_json::to_vec_pretty(summary)?,
        )?;
        fs::write(
            self.root.join("summary.md"),
            format!(
                "# Crawl summary\n\nScheduled: {}\n\nCompleted: {}\n\nFetched: {}\n\nSkipped: {}\n\nFailed or incomplete: {}\n\nFindings: {}\n\nSerious reports: {}\n\nSuccessful classifier/review calls: {}/{}\n\nInterrupted: {}\n\nSeeds excluded by budget: {}\n\nSee `results.csv`, `events.jsonl`, and `decisions/` for all outcomes.\n",
                summary.scheduled,
                summary.completed,
                summary.fetched,
                summary.skipped,
                summary.failed,
                summary.findings,
                summary.reports,
                summary.classifier_calls,
                summary.review_calls,
                summary.interrupted,
                summary.seeds_over_budget
            ),
        )?;
        self.event("run_finished", serde_json::to_value(summary)?)
    }
}

/// Header written once when a level-one finding file is created.
pub fn level_one_header(final_url: &impl std::fmt::Display, fetched_at: &str) -> String {
    format!(
        "# Level-one agent activity finding\n\nSource: {final_url}\n\nFetched: {fetched_at}\n\nStrong-model review pending.\n\n"
    )
}

/// One Jev section appended to a level-one finding file.
pub fn level_one_section(screening: &Screening) -> String {
    format!(
        "## Jev — chunk {}\n\nModel: {}\n\nImportance probability: {:.2}%\n\nAgent activity probability: {:.2}%\n\nPotentially serious probability: {:.2}%\n\n",
        screening.chunk,
        screening.model,
        100.0 * screening.importance,
        100.0 * screening.agent_activity,
        100.0 * screening.potentially_serious
    )
}

pub fn render(record: &Record, serious: bool, threshold: f64) -> String {
    let mut md = format!(
        "# {}\n\nSource: {}\n\nSeed: {}\n\nDisposition: {}\n\n",
        if serious {
            "Serious agent activity report"
        } else {
            "Agent activity finding"
        },
        record
            .page
            .as_ref()
            .map(|p| p.final_url.as_str())
            .unwrap_or(record.job.url.as_str()),
        record.job.seed,
        record.disposition
    );
    if let Some(page) = &record.page {
        md.push_str(&format!("Fetched: {} (HTTP {})\n\nArchived evidence: `../pages/{}.txt` and `../pages/{}.body`\n\n", page.fetched_at, page.status, id(&record.job.url), id(&record.job.url)));
    }
    md.push_str("This is a model assessment of archived web evidence. Claims on a page may require independent corroboration.\n\n");
    if !serious {
        for screening in &record.screenings {
            md.push_str(&format!("## Jev — chunk {}\n\nModel: {}\n\nImportance probability: {:.2}%; agent activity probability: {:.2}%; potentially serious probability: {:.2}%\n\n", screening.chunk, screening.model, screening.importance * 100.0, screening.agent_activity * 100.0, screening.potentially_serious * 100.0));
        }
        if record.judgments.is_empty() {
            md.push_str("Strong-model review unavailable; the Jev flag remains recorded.\n\n");
        }
    }
    for j in &record.judgments {
        if serious && !(j.stage == "review" && j.verdict.report(threshold)) {
            continue;
        }
        md.push_str(&format!("## {} — chunk {}\n\nModel: {}\n\nClassification: {:?}\n\nActivity: {:.2}; seriousness: {:.2}; confidence: {:.2}\n\n{}\n\n### Evidence\n\n", j.stage, j.chunk, j.model, j.verdict.classification, j.verdict.activity, j.verdict.seriousness, j.verdict.confidence, j.verdict.summary));
        for quote in &j.verdict.evidence {
            for line in quote.lines() {
                md.push_str(&format!("> {line}\n"));
            }
            md.push('\n');
        }
        md.push_str("### Uncertainties and alternatives\n\n");
        for item in &j.verdict.uncertainties {
            md.push_str(&format!("- {item}\n"));
        }
        md.push('\n');
    }
    if !record.errors.is_empty() {
        md.push_str("## Incomplete processing\n\n");
        for error in &record.errors {
            md.push_str(&format!("- {error}\n"));
        }
    }
    md
}
