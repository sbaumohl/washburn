use std::{
    collections::HashMap,
    fs,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Html,
    routing::{get, post},
};
use clap::Parser;
use cn_swarms::{
    config::Config,
    crawl::{Crawler, Fetch, Job},
};
use serde_json::{Value, json};
use tempfile::TempDir;
use url::Url;

#[derive(Clone, Default)]
struct MockState {
    requests: Arc<Mutex<Vec<Value>>>,
    classifications: Arc<Mutex<Vec<Value>>>,
    output_root: Arc<Mutex<Option<std::path::PathBuf>>>,
    jev_retry_hits: Arc<AtomicUsize>,
    blocked_hits: Arc<AtomicUsize>,
    retry_hits: Arc<AtomicUsize>,
}

async fn classification(
    State(state): State<MockState>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> (StatusCode, HeaderMap, Json<Value>) {
    assert_eq!(
        headers.get("authorization").unwrap(),
        "Bearer mock-typesafe-key"
    );
    assert_eq!(request["model"], "test/jev");
    assert_eq!(request["questions"]["importance"]["type"], "noul");
    assert_eq!(request["questions"].as_object().unwrap().len(), 3);
    let text = request["state"]["document"].as_str().unwrap();
    state.classifications.lock().unwrap().push(request.clone());
    let mut response_headers = HeaderMap::new();
    if text.contains("JEV_RETRY_TRACE") && state.jev_retry_hits.fetch_add(1, Ordering::SeqCst) == 0
    {
        response_headers.insert("retry-after", "0".parse().unwrap());
        return (
            StatusCode::TOO_MANY_REQUESTS,
            response_headers,
            Json(json!({"error": "retry"})),
        );
    }
    if text.contains("JEV_ERROR_TRACE") {
        return (
            StatusCode::UNAUTHORIZED,
            response_headers,
            Json(json!({"error": "mock-typesafe-key mock-test-key"})),
        );
    }
    let importance: f64 = if text.contains("AUTHORIZED_TRACE") {
        0.70
    } else if text.contains("BELOW_TRACE") {
        0.6999
    } else if text.contains("BAD_JEV_TRACE") {
        1.2
    } else if text.contains("CONFIRMED_TRACE")
        || text.contains("SUSPECT_TRACE")
        || text.contains("INVALID_TRACE")
        || text.contains("REVIEW_ERROR_TRACE")
        || text.contains("ROUTING_ERROR_TRACE")
    {
        0.95
    } else {
        0.0
    };
    (
        StatusCode::OK,
        response_headers,
        Json(
            json!({"model": "test/jev-actual", "usage": {"input_tokens": 40, "output_tokens": 0},
                "answers": {
                    "importance": {"type": "noul", "noul": importance},
                    "agent_activity": {"type": "noul", "noul": importance.min(1.0)},
                    "potentially_serious": {"type": "noul", "noul": if text.contains("AUTHORIZED_TRACE") { 0.0 } else { importance.min(1.0) }},
                },
                "debug": "provider echoed mock-typesafe-key and mock-test-key",
            }),
        ),
    )
}

async fn completion(
    State(state): State<MockState>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> (StatusCode, Json<Value>) {
    assert_eq!(
        headers.get("authorization").unwrap(),
        "Bearer mock-test-key"
    );
    assert_eq!(request["model"], "test/review");
    assert_eq!(request["response_format"]["type"], "json_schema");
    assert_eq!(request["provider"]["require_parameters"], true);
    let data: Value =
        serde_json::from_str(request["messages"][1]["content"].as_str().unwrap()).unwrap();
    let text = data["document"].as_str().unwrap();
    assert!(data["screening_hypothesis"]["importance"].as_f64().unwrap() >= 0.7);
    // A durable level-one note must exist before the LLM request even starts.
    let output_root = state.output_root.lock().unwrap().clone().unwrap();
    let run = fs::read_dir(output_root)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let url = Url::parse(data["source_url"].as_str().unwrap()).unwrap();
    let finding =
        fs::read_to_string(run.join(format!("findings/{}.md", cn_swarms::sink::id(&url)))).unwrap();
    assert!(finding.contains("review pending"));
    state.requests.lock().unwrap().push(request.clone());
    if text.contains("REVIEW_ERROR_TRACE") {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "mock-typesafe-key mock-test-key"})),
        );
    }
    if text.contains("ROUTING_ERROR_TRACE") {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": {
                "message": "No endpoints match account routing policies. ZDR violation (account settings). mock-typesafe-key mock-test-key",
                "code": 404,
                "metadata": {
                    "failed_routing_step": "Filter by Guardrails",
                    "ineligibility_reasons": [{"reason": "zdr-violation-by-account", "endpoint_count": 1, "configure_url": "https://openrouter.ai/settings/privacy"}],
                    "raw": "RAW_UPSTREAM_REQUEST mock-typesafe-key mock-test-key"
                }
            }})),
        );
    }
    let (activity, seriousness, classification, quote, confirmed) =
        if text.contains("CONFIRMED_TRACE") {
            (
                0.95,
                0.95,
                "suspected_unauthorized",
                Some("CONFIRMED_TRACE"),
                true,
            )
        } else if text.contains("AUTHORIZED_TRACE") {
            (0.9, 0.0, "authorized", Some("AUTHORIZED_TRACE"), false)
        } else if text.contains("SUSPECT_TRACE") {
            (0.9, 0.2, "unclear", Some("SUSPECT_TRACE"), false)
        } else if text.contains("INVALID_TRACE") {
            (
                0.9,
                0.9,
                "suspected_unauthorized",
                Some("fabricated quote"),
                false,
            )
        } else {
            (0.0, 0.0, "no_signal", None, false)
        };
    let verdict = json!({"activity": activity, "seriousness": seriousness, "confidence": 0.95,
        "classification": classification, "summary": "Independent review assessment", "evidence": quote.into_iter().collect::<Vec<_>>(),
        "uncertainties": ["External corroboration remains necessary"], "confirmed_serious": confirmed});
    (
        StatusCode::OK,
        Json(
            json!({"model": "test/review-actual", "usage": {"prompt_tokens": 20, "completion_tokens": 30},
                "choices": [{"finish_reason": "stop", "message": {"content": verdict.to_string()}}],
                "debug": "provider echoed mock-typesafe-key and mock-test-key",
            }),
        ),
    )
}

struct Server {
    base: String,
    state: MockState,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn server() -> Server {
    let state = MockState::default();
    let app = Router::new()
        .route("/", get(|| async { Html("<title>Index</title><a href='/authorized'>a</a><a href='/serious'>s</a><a href='/suspect'>u</a><a href='/blocked'>b</a><a href='/below'>low</a><a href='/authorized#duplicate'>again</a><a href='https://outside.invalid/'>external</a>") }))
        .route("/robots.txt", get(|| async { "User-agent: *\nDisallow: /blocked\n" }))
        .route("/authorized", get(|| async { "AUTHORIZED_TRACE: an operator's scheduled agent run" }))
        .route("/serious", get(|| async { "CONFIRMED_TRACE: evidence of consequential misconduct" }))
        .route("/suspect", get(|| async { "SUSPECT_TRACE: uncertain web claims" }))
        .route("/below", get(|| async { "BELOW_TRACE" }))
        .route("/bad-jev", get(|| async { "BAD_JEV_TRACE" }))
        .route("/jev-error", get(|| async { "JEV_ERROR_TRACE" }))
        .route("/jev-retry", get(|| async { "JEV_RETRY_TRACE" }))
        .route("/review-error", get(|| async { "REVIEW_ERROR_TRACE" }))
        .route("/routing-error", get(|| async { "ROUTING_ERROR_TRACE" }))
        .route("/invalid", get(|| async { "INVALID_TRACE" }))
        .route("/chunks", get(|| async { "abcdefghijklmnopqrstuvwxyz你🙂boundary-evidence" }))
        .route("/blocked", get(|State(s): State<MockState>| async move { s.blocked_hits.fetch_add(1, Ordering::SeqCst); "must not fetch" }))
        .route("/large", get(|| async { "x".repeat(2048) }))
        .route("/binary", get(|| async { ([("content-type", "image/png")], vec![0_u8, 1, 2]) }))
        .route("/redirect", get(|| async { (StatusCode::FOUND, [("location", "https://outside.invalid/")]) }))
        .route("/retry", get(|State(s): State<MockState>| async move {
            if s.retry_hits.fetch_add(1, Ordering::SeqCst) == 0 {
                (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "0")], "wait")
            } else { (StatusCode::OK, [("retry-after", "0")], "retried successfully") }
        }))
        .route("/api/v1/systemone", post(classification))
        .route("/api/v1/chat/completions", post(completion))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Server { base, state, task }
}

async fn cli(
    server: &Server,
    path: &str,
    extra: &[&str],
    key: bool,
) -> (std::process::Output, TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("targets.txt");
    fs::write(&input, format!("{}{path}\n", server.base)).unwrap();
    let settings = dir.path().join("settings.toml");
    let service_url = format!("{}/api/v1", server.base);
    fs::write(&settings, format!("[typesafe]\napi_key='mock-typesafe-key'\nmodel='test/jev'\nbase_url='{service_url}'\n[openrouter]\napi_key='mock-test-key'\nmodel='test/review'\nbase_url='{service_url}'\n")).unwrap();
    let output_root = dir.path().join("runs");
    *server.state.output_root.lock().unwrap() = Some(output_root.clone());
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_cn-swarms"));
    cmd.arg("--input")
        .arg(input)
        .arg("--output")
        .arg(output_root)
        .args([
            "--delay-ms",
            "0",
            "--timeout-secs",
            "2",
            "--judge-timeout-secs",
            "2",
        ])
        .args(extra)
        // Deliberately wrong environment keys prove TOML is the credential source.
        .env("OPENROUTER_API_KEY", "wrong-env-openrouter")
        .env("TYPESAFE_API_KEY", "wrong-env-typesafe");
    if key {
        cmd.arg("--config").arg(settings);
    }
    let output = tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap();
    let run = fs::read_dir(dir.path().join("runs"))
        .ok()
        .and_then(|mut r| r.next())
        .map(|e| e.unwrap().path())
        .unwrap_or_default();
    (output, dir, run)
}

#[tokio::test]
async fn cli_crawls_triages_and_writes_both_sinks() {
    let server = server().await;
    let (output, _dir, run) = cli(
        &server,
        "/",
        &["--max-pages", "10", "--max-depth", "1"],
        true,
    )
    .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let summary: Value =
        serde_json::from_slice(&fs::read(run.join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary["scheduled"], 6);
    assert_eq!(summary["fetched"], 5);
    assert_eq!(summary["skipped"], 1);
    assert_eq!(summary["findings"], 3);
    assert_eq!(summary["reports"], 1);
    assert_eq!(summary["classifier_calls"], 5);
    assert_eq!(summary["important_chunks"], 3);
    assert_eq!(summary["review_calls"], 3);
    assert_eq!(server.state.blocked_hits.load(Ordering::SeqCst), 0);
    let mut reader = csv::Reader::from_path(run.join("results.csv")).unwrap();
    let rows: Vec<HashMap<String, String>> = reader.deserialize().map(Result::unwrap).collect();
    assert_eq!(rows.len(), 6);
    let authorized = rows
        .iter()
        .find(|r| r["url"].ends_with("/authorized"))
        .unwrap();
    assert_eq!(authorized["classification"], "authorized");
    assert_eq!(authorized["importance"], "0.7");
    assert!(!authorized["finding_path"].is_empty());
    assert!(authorized["report_path"].is_empty());
    let serious = rows
        .iter()
        .find(|r| r["url"].ends_with("/serious"))
        .unwrap();
    let report = fs::read_to_string(run.join(&serious["report_path"])).unwrap();
    assert!(report.contains("CONFIRMED_TRACE"));
    assert!(report.contains("test/review-actual"));
    assert_eq!(
        fs::read(run.join(&serious["raw_path"])).unwrap(),
        b"CONFIRMED_TRACE: evidence of consequential misconduct"
    );
    let suspect = rows
        .iter()
        .find(|r| r["url"].ends_with("/suspect"))
        .unwrap();
    assert_eq!(suspect["seriousness"], "0.2");
    assert!(suspect["report_path"].is_empty());
    let log = fs::read_to_string(run.join("events.jsonl")).unwrap();
    assert!(log.contains("page_skipped"));
    assert!(log.contains("level_one_flagged"));
    let below = rows.iter().find(|r| r["url"].ends_with("/below")).unwrap();
    assert_eq!(below["importance"], "0.6999");
    assert!(below["finding_path"].is_empty());
    assert_eq!(below["review_chunks"], "0");
    assert_artifacts_have_no_keys(&run);
    assert!(!run.join("settings.toml").exists());
    assert!(!log.contains("mock-test-key"));
    for line in log.lines() {
        serde_json::from_str::<Value>(line).unwrap();
    }
}

#[tokio::test]
async fn malformed_evidence_is_incomplete_and_raw_page_survives() {
    let server = server().await;
    let (output, _dir, run) = cli(&server, "/invalid", &["--max-pages", "1"], true).await;
    assert_eq!(output.status.code(), Some(2));
    let summary: Value =
        serde_json::from_slice(&fs::read(run.join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary["failed"], 1);
    assert_eq!(summary["reports"], 0);
    assert_eq!(fs::read_dir(run.join("pages")).unwrap().count(), 3);
    let csv = fs::read_to_string(run.join("results.csv")).unwrap();
    assert!(csv.contains("triage_error"));
    assert_eq!(summary["findings"], 1);
    assert!(csv.contains("not an exact source quote"));
    let log = fs::read_to_string(run.join("events.jsonl")).unwrap();
    assert!(log.contains("fabricated quote"));
    assert!(log.contains("rejected_response"));
    assert_artifacts_have_no_keys(&run);
}

#[tokio::test]
async fn every_unicode_text_chunk_is_sent_to_screening() {
    let server = server().await;
    let (output, _dir, _run) = cli(
        &server,
        "/chunks",
        &["--max-pages", "1", "--chunk-chars", "7"],
        true,
    )
    .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(server.state.requests.lock().unwrap().is_empty());
    let requests = server.state.classifications.lock().unwrap();
    assert!(requests.len() > 1);
    let mut text = String::new();
    for request in requests.iter() {
        assert_eq!(request["model"], "test/jev");
        let evidence = &request["state"];
        let chunk = evidence["document"].as_str().unwrap();
        assert!(chunk.chars().count() <= 7);
        text.push_str(chunk);
    }
    assert_eq!(text, "abcdefghijklmnopqrstuvwxyz你🙂boundary-evidence");
}

#[tokio::test]
async fn crawl_only_needs_no_key_and_obeys_budget() {
    let server = server().await;
    let (output, _dir, run) = cli(&server, "/", &["--crawl-only", "--max-pages", "2"], false).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let summary: Value =
        serde_json::from_slice(&fs::read(run.join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary["scheduled"], 2);
    assert_eq!(summary["completed"], 2);
    assert!(server.state.requests.lock().unwrap().is_empty());
    assert!(server.state.classifications.lock().unwrap().is_empty());
    assert!(
        fs::read_to_string(run.join("results.csv"))
            .unwrap()
            .contains("unjudged")
    );
}

#[tokio::test]
async fn crawler_retries_bounds_bodies_and_blocks_out_of_scope_redirects() {
    let server = server().await;
    let config = Config::try_parse_from([
        "cn-swarms",
        "--input",
        "unused.txt",
        "--crawl-only",
        "--delay-ms",
        "0",
        "--max-body-bytes",
        "1024",
        "--retries",
        "1",
    ])
    .unwrap();
    let seed = Url::parse(&server.base).unwrap();
    let crawler = Crawler::new(Arc::new(config), std::slice::from_ref(&seed)).unwrap();
    let job = |path: &str| Job {
        url: seed.join(path).unwrap(),
        seed: seed.clone(),
        parent: None,
        depth: 0,
    };
    let result = crawler.fetch(job("/retry")).await.unwrap();
    assert!(matches!(result, Fetch::Page(p) if p.text == "retried successfully"));
    assert_eq!(server.state.retry_hits.load(Ordering::SeqCst), 2);
    assert!(
        crawler
            .fetch(job("/large"))
            .await
            .unwrap_err()
            .to_string()
            .contains("byte limit")
    );
    assert!(
        crawler
            .fetch(job("/redirect"))
            .await
            .unwrap_err()
            .to_string()
            .contains("outside seed scope")
    );
    assert!(matches!(
        crawler.fetch(job("/binary")).await.unwrap(),
        Fetch::Skipped(_)
    ));
}

fn assert_artifacts_have_no_keys(path: &std::path::Path) {
    for entry in fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            assert_artifacts_have_no_keys(&path);
        } else {
            let bytes = fs::read(&path).unwrap();
            let content = String::from_utf8_lossy(&bytes);
            for key in [
                "mock-test-key",
                "mock-typesafe-key",
                "wrong-env-openrouter",
                "wrong-env-typesafe",
            ] {
                assert!(
                    !content.contains(key),
                    "secret leaked into {}",
                    path.display()
                );
            }
        }
    }
}

#[tokio::test]
async fn classifier_failures_keep_archives_and_never_trigger_review() {
    for path in ["/bad-jev", "/jev-error"] {
        let server = server().await;
        let (output, _dir, run) =
            cli(&server, path, &["--max-pages", "1", "--retries", "0"], true).await;
        assert_eq!(output.status.code(), Some(2));
        let summary: Value =
            serde_json::from_slice(&fs::read(run.join("summary.json")).unwrap()).unwrap();
        assert_eq!(summary["failed"], 1);
        assert_eq!(summary["findings"], 0);
        assert_eq!(summary["review_calls"], 0);
        assert!(server.state.requests.lock().unwrap().is_empty());
        assert_eq!(fs::read_dir(run.join("pages")).unwrap().count(), 3);
        assert_artifacts_have_no_keys(&run);
        assert!(!String::from_utf8_lossy(&output.stderr).contains("mock-typesafe-key"));
    }
}

#[tokio::test]
async fn level_one_survives_review_failure() {
    let server = server().await;
    let (output, _dir, run) = cli(
        &server,
        "/review-error",
        &["--max-pages", "1", "--retries", "0"],
        true,
    )
    .await;
    assert_eq!(output.status.code(), Some(2));
    let summary: Value =
        serde_json::from_slice(&fs::read(run.join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary["findings"], 1);
    assert_eq!(summary["reports"], 0);
    assert_eq!(summary["important_chunks"], 1);
    let finding = fs::read_dir(run.join("findings"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let content = fs::read_to_string(finding).unwrap();
    assert!(content.contains("Importance probability: 95.00%"));
    assert!(content.contains("review unavailable"));
    assert!(content.contains("Incomplete processing"));
    assert_artifacts_have_no_keys(&run);
}

#[tokio::test]
async fn classifier_retries_rate_limits() {
    let server = server().await;
    let (output, _dir, run) = cli(
        &server,
        "/jev-retry",
        &["--max-pages", "1", "--retries", "1"],
        true,
    )
    .await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(server.state.jev_retry_hits.load(Ordering::SeqCst), 2);
    assert_eq!(server.state.classifications.lock().unwrap().len(), 2);
    assert!(server.state.requests.lock().unwrap().is_empty());
    assert_artifacts_have_no_keys(&run);
}

#[tokio::test]
async fn triage_requires_explicit_toml_config_before_crawling() {
    let server = server().await;
    let (output, _dir, run) = cli(&server, "/", &["--max-pages", "1"], false).await;
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--config"));
    assert!(run.as_os_str().is_empty());
    assert!(server.state.classifications.lock().unwrap().is_empty());
    assert!(server.state.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn openrouter_404_retains_redacted_policy_diagnostics() {
    let server = server().await;
    let (output, _dir, run) = cli(
        &server,
        "/routing-error",
        &["--max-pages", "1", "--retries", "0"],
        true,
    )
    .await;
    assert_eq!(output.status.code(), Some(2));
    let log = fs::read_to_string(run.join("events.jsonl")).unwrap();
    let event: Value = log
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|event| event["event"] == "judge_error")
        .unwrap();
    let error = &event["detail"]["api_error"];
    assert_eq!(error["status"], 404);
    assert_eq!(error["code"], 404);
    assert_eq!(error["model"], "test/review");
    assert!(
        error["endpoint"]
            .as_str()
            .unwrap()
            .ends_with("/api/v1/chat/completions")
    );
    assert!(error["message"].as_str().unwrap().contains("ZDR violation"));
    assert_eq!(error["failed_routing_step"], "Filter by Guardrails");
    assert_eq!(
        error["routing_reasons"][0]["reason"],
        "zdr-violation-by-account"
    );
    assert!(!log.contains("RAW_UPSTREAM_REQUEST"));
    assert_artifacts_have_no_keys(&run);
    let csv = fs::read_to_string(run.join("results.csv")).unwrap();
    assert!(csv.contains("ZDR violation"));
    let summary: Value =
        serde_json::from_slice(&fs::read(run.join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary["findings"], 1);
    assert_eq!(summary["reports"], 0);
}
