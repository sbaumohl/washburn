//! End-to-end tests: the judge app runs in-process against a mock OpenRouter
//! endpoint whose behaviour is fixed per test.

use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{net::TcpListener, task::JoinHandle};
use washburn_common::WashburnConfig;
use washburn_judge::{AppState, build_app};

const QUOTE: &str = "the agent ran rm -rf on the staging volume";

/// How the mock OpenRouter answers `POST /api/v1/chat/completions`.
#[derive(Clone, Copy)]
enum Upstream {
    /// Valid structured verdict quoting `QUOTE`.
    Verdict,
    /// HTTP 200 whose message content is not JSON.
    Malformed,
    /// HTTP 500 with a structured provider error.
    ServerError,
    /// HTTP 429 with `retry-after: 2`, every time.
    RateLimited,
}

async fn completion(State(mode): State<Upstream>, Json(request): Json<Value>) -> Response {
    assert_eq!(request["response_format"]["type"], "json_schema");
    match mode {
        Upstream::Verdict => {
            let verdict = json!({
                "activity": 0.9, "seriousness": 0.8, "confidence": 0.95,
                "classification": "suspected_unauthorized",
                "summary": "Agent deleted staging data without authorization.",
                "evidence": [QUOTE],
                "uncertainties": ["No corroborating log outside this page."],
                "confirmed_serious": true,
            });
            Json(chat_completion(&verdict.to_string())).into_response()
        }
        Upstream::Malformed => Json(chat_completion("{not json")).into_response(),
        Upstream::ServerError => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "upstream exploded", "code": 500}})),
        )
            .into_response(),
        Upstream::RateLimited => {
            let mut response = (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({"error": {"message": "rate limited", "code": 429}})),
            )
                .into_response();
            response
                .headers_mut()
                .insert("retry-after", HeaderValue::from_static("2"));
            response
        }
    }
}

fn chat_completion(content: &str) -> Value {
    json!({
        "model": "test/review-actual",
        "usage": {"prompt_tokens": 20, "completion_tokens": 30},
        "choices": [{"finish_reason": "stop", "message": {"content": content}}],
    })
}

/// Aborts its background task when dropped.
struct Spawned {
    base: String,
    task: JoinHandle<()>,
}

impl Drop for Spawned {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(app: Router) -> Spawned {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Spawned { base, task }
}

async fn spawn_upstream(mode: Upstream) -> Spawned {
    let app = Router::new()
        .route("/api/v1/chat/completions", post(completion))
        .with_state(mode);
    serve(app).await
}

/// Writes a five-table `washburn.toml`, loads it, and serves the judge app.
async fn spawn_judge(
    upstream: &Spawned,
    concurrency: usize,
    timeout_secs: u64,
    retries: usize,
) -> (Spawned, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("washburn.toml");
    let toml = format!(
        r#"
[typesafe]
api_key = "test-typesafe-key"
base_url = "{up}/api/v1"

[openrouter]
api_key = "test-openrouter-key"
model = "test/review"
base_url = "{up}/api/v1"

[postgres]
host = "127.0.0.1"
db = "washburn"
user = "washburn"
password = "test-pg-password"
port = 5432

[crawl]
user_agent = "washburn-test/0.1"
workers = 1
concurrency = 1
classifier_concurrency = 1
delay_ms = 0
timeout_secs = 5
retries = {retries}
max_body_bytes = 2000000
chunk_chars = 12000
max_redirects = 5
max_depth = 1
importance_threshold = 0.70
report_threshold = 0.80
ignore_robots = true
host_lease_secs = 120
heartbeat_secs = 30

[judge]
bind = "127.0.0.1:0"
url = "http://judge:8081"
concurrency = {concurrency}
timeout_secs = {timeout_secs}
"#,
        up = upstream.base,
    );
    std::fs::write(&path, toml).unwrap();
    let config = WashburnConfig::load(&path).unwrap();
    let state = AppState::new(&config, Arc::new("test rubric".to_owned())).unwrap();
    (serve(build_app(state)).await, dir)
}

fn judge_request() -> Value {
    json!({
        "final_url": "https://example.test/agent-log",
        "fetched_at": "2026-10-07T00:00:00+00:00",
        "status": 200,
        "title": "Agent log",
        "text": format!("Session note: {QUOTE}."),
        "chunk": 1,
        "screening": {
            "chunk": 1, "requested_model": "jev-latest", "model": "jev-latest",
            "usage": null, "importance": 0.9, "agent_activity": 0.8, "potentially_serious": 0.7,
        },
    })
}

async fn post_judge(client: &reqwest::Client, base: &str, body: &Value) -> (StatusCode, Value) {
    let response = client
        .post(format!("{base}/v1/judge"))
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    (status, response.json().await.unwrap())
}

#[tokio::test]
async fn healthz_reports_model_and_capacity() {
    let upstream = spawn_upstream(Upstream::Verdict).await;
    let (judge, _dir) = spawn_judge(&upstream, 3, 30, 0).await;
    let body: Value = reqwest::get(format!("{}/healthz", judge.base))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body,
        json!({"ok": true, "model": "test/review", "in_flight": 0, "capacity": 3})
    );
}

#[tokio::test]
async fn judge_returns_verdict_for_quoted_evidence() {
    let upstream = spawn_upstream(Upstream::Verdict).await;
    let (judge, _dir) = spawn_judge(&upstream, 2, 30, 0).await;
    let (status, body) = post_judge(&reqwest::Client::new(), &judge.base, &judge_request()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let judgment = &body["judgment"];
    assert_eq!(judgment["stage"], "review");
    assert_eq!(judgment["chunk"], 1);
    assert_eq!(judgment["model"], "test/review-actual");
    let verdict = &judgment["verdict"];
    assert_eq!(verdict["activity"], json!(0.9));
    assert_eq!(verdict["seriousness"], json!(0.8));
    assert_eq!(verdict["confidence"], json!(0.95));
    assert_eq!(verdict["classification"], "suspected_unauthorized");
    assert_eq!(verdict["confirmed_serious"], true);
    assert_eq!(verdict["evidence"], json!([QUOTE]));
}

#[tokio::test]
async fn malformed_model_output_is_422_rejected() {
    let upstream = spawn_upstream(Upstream::Malformed).await;
    let (judge, _dir) = spawn_judge(&upstream, 2, 30, 0).await;
    let (status, body) = post_judge(&reqwest::Client::new(), &judge.base, &judge_request()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["kind"], "rejected");
    assert!(body["response"].is_object(), "{body}");
}

#[tokio::test]
async fn provider_error_is_502_provider() {
    let upstream = spawn_upstream(Upstream::ServerError).await;
    let (judge, _dir) = spawn_judge(&upstream, 2, 30, 0).await;
    let (status, body) = post_judge(&reqwest::Client::new(), &judge.base, &judge_request()).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["kind"], "provider");
    assert_eq!(body["api_error"]["status"], 500);
}

#[tokio::test]
async fn second_request_waits_out_timeout_and_gets_503_busy() {
    // Upstream rate-limits with a 2 s retry-after, so the one permit stays held
    // for about 2 s, longer than the 1 s wait budget of the second request.
    let upstream = spawn_upstream(Upstream::RateLimited).await;
    let (judge, _dir) = spawn_judge(&upstream, 1, 1, 1).await;
    let client = reqwest::Client::new();
    let body = judge_request();
    let first = post_judge(&client, &judge.base, &body);
    let second = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        post_judge(&client, &judge.base, &body).await
    };
    let ((status_a, body_a), (status_b, body_b)) = tokio::join!(first, second);
    let mut statuses = [status_a, status_b];
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::BAD_GATEWAY, StatusCode::SERVICE_UNAVAILABLE]
    );
    let busy = if status_a == StatusCode::SERVICE_UNAVAILABLE {
        body_a
    } else {
        body_b
    };
    assert_eq!(busy["kind"], "busy");
}
