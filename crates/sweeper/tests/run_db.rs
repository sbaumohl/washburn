//! End-to-end sweeper runs against mock TypeSafe, judge, and site servers.
//! Requires `WASHBURN_TEST_DATABASE_URL` (a disposable Postgres 16). Each test
//! uses its own schema. Without the variable every test returns early.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    extract::State,
    response::Html,
    routing::{get, post},
};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use washburn_common::{WashburnConfig, add_seed, migrate};
use washburn_sweeper::{RunMode, RunOutcome, run_with_pool};

const QUOTE: &str = "AGENT ran rm -rf";

#[derive(Clone, Default)]
struct Mock {
    jev_calls: Arc<AtomicUsize>,
    judge_calls: Arc<AtomicUsize>,
}

async fn classify(State(mock): State<Mock>, Json(request): Json<Value>) -> Json<Value> {
    mock.jev_calls.fetch_add(1, Ordering::SeqCst);
    let document = request["state"]["document"].as_str().unwrap_or_default();
    let importance = if document.contains("AGENT") { 0.9 } else { 0.1 };
    Json(json!({
        "model": "test/jev-actual",
        "usage": {"input_tokens": 1},
        "answers": {
            "importance": {"type": "noul", "noul": importance},
            "agent_activity": {"type": "noul", "noul": importance},
            "potentially_serious": {"type": "noul", "noul": 0.2},
        },
    }))
}

async fn judge(State(mock): State<Mock>, Json(request): Json<Value>) -> Json<Value> {
    mock.judge_calls.fetch_add(1, Ordering::SeqCst);
    let text = request["text"].as_str().unwrap_or_default();
    let evidence: Vec<&str> = if text.contains(QUOTE) { vec![QUOTE] } else { vec![] };
    Json(json!({"judgment": {
        "stage": "review",
        "chunk": request["chunk"],
        "requested_model": "test/review",
        "model": "test/review-actual",
        "usage": {},
        "verdict": {
            "activity": 0.9,
            "seriousness": 0.9,
            "confidence": 0.9,
            "classification": "suspected_unauthorized",
            "summary": "The agent ran destructive commands.",
            "evidence": evidence,
            "uncertainties": ["No corroboration of identity."],
            "confirmed_serious": true,
        },
    }}))
}

async fn healthz() -> Json<Value> {
    Json(json!({"ok": true}))
}

async fn page_a() -> Html<&'static str> {
    Html("<title>Trace A</title><p>AGENT ran rm -rf in the trace</p><a href='/b'>next</a>")
}

async fn page_b() -> Html<&'static str> {
    Html("<title>Notes B</title><p>plain notes about gardening</p>")
}

struct Server {
    base: String,
    mock: Mock,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn server() -> Server {
    let mock = Mock::default();
    let app = Router::new()
        .route("/a", get(page_a))
        .route("/b", get(page_b))
        .route("/api/v1/systemone", post(classify))
        .route("/v1/judge", post(judge))
        .route("/healthz", get(healthz))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Server { base, mock, task }
}

fn config(base: &str) -> WashburnConfig {
    WashburnConfig::parse(&format!(
        r#"
[typesafe]
api_key = "mock-typesafe-key"
model = "test/jev"
base_url = "{base}/api/v1"

[openrouter]
api_key = "mock-openrouter-key"
model = "test/review"
base_url = "{base}/api/v1"

[postgres]
host = "localhost"
db = "postgres"
user = "postgres"
password = "unused-in-tests"
port = 5432

[crawl]
user_agent = "washburn-test/0.1"
workers = 1
concurrency = 2
classifier_concurrency = 4
delay_ms = 0
timeout_secs = 5
retries = 0
max_body_bytes = 2000000
chunk_chars = 12000
max_redirects = 5
max_depth = 2
importance_threshold = 0.7
report_threshold = 0.8
ignore_robots = false
host_lease_secs = 60
heartbeat_secs = 30

[judge]
bind = "0.0.0.0:8081"
url = "{base}"
concurrency = 2
timeout_secs = 10
"#
    ))
    .expect("test config parses")
}

/// A pool whose connections all use a fresh schema, after migrating it.
async fn setup() -> Option<(PgPool, String, String)> {
    let url = std::env::var("WASHBURN_TEST_DATABASE_URL").ok()?;
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_nanos();
    let schema = format!("wt_sweeper_{}_{nanos}", std::process::id());
    let admin = PgPool::connect(&url).await.expect("connect to test database");
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("create test schema");
    let search_path = schema.clone();
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .after_connect(move |conn, _meta| {
            let search_path = search_path.clone();
            Box::pin(async move {
                sqlx::query(&format!("SET search_path TO {search_path}"))
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect(&url)
        .await
        .expect("connect with test search_path");
    migrate(&pool).await.expect("migrations apply");
    Some((pool, url, schema))
}

async fn teardown(url: &str, schema: &str) {
    let admin = PgPool::connect(url).await.expect("reconnect for teardown");
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop test schema");
}

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.expect("count query")
}

#[tokio::test]
async fn full_run_screens_reviews_and_drains_host() {
    let Some((pool, url, schema)) = setup().await else {
        eprintln!("skipping: WASHBURN_TEST_DATABASE_URL not set");
        return;
    };
    let site = server().await;
    add_seed(&pool, &format!("{}/a", site.base), None, Some("test"))
        .await
        .unwrap();

    let outcome = run_with_pool(config(&site.base), pool.clone(), RunMode::once())
        .await
        .unwrap();
    assert_eq!(outcome, RunOutcome::Drained);

    let statuses: Vec<String> =
        sqlx::query_scalar("SELECT status::text FROM queue ORDER BY url")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(statuses, vec!["done", "done"]);
    assert_eq!(count(&pool, "SELECT count(*) FROM pages").await, 2);
    assert_eq!(count(&pool, "SELECT count(*) FROM blobs").await, 2);
    assert!(count(&pool, "SELECT count(*) FROM screenings").await >= 2);
    assert!(count(&pool, "SELECT count(*) FROM judgments").await >= 1);

    let page_a = format!("{}/a", site.base);
    let level_one: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM findings f JOIN pages p ON p.id = f.page_id
         WHERE p.url = $1 AND f.kind = 'level_one'",
    )
    .bind(&page_a)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(level_one, 1, "level-one finding for the AGENT page");
    let report: String = sqlx::query_scalar(
        "SELECT f.markdown FROM findings f JOIN pages p ON p.id = f.page_id
         WHERE p.url = $1 AND f.kind = 'report'",
    )
    .bind(&page_a)
    .fetch_one(&pool)
    .await
    .expect("report finding for the AGENT page");
    assert!(report.contains("Serious agent activity report"));

    let pending: Vec<i32> = sqlx::query_scalar("SELECT pending_count FROM hosts")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(pending, vec![0]);
    assert_eq!(count(&pool, "SELECT count(*) FROM heartbeats").await, 0);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM events WHERE kind = 'page_archived'").await,
        2
    );
    let event_text: String =
        sqlx::query_scalar("SELECT COALESCE(string_agg(detail::text, ' '), '') FROM events")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!event_text.contains("mock-typesafe-key"));
    assert!(!event_text.contains("mock-openrouter-key"));
    assert_eq!(site.mock.judge_calls.load(Ordering::SeqCst), 1);

    drop(pool);
    teardown(&url, &schema).await;
}

#[tokio::test]
async fn crawl_only_stores_pages_without_screening() {
    let Some((pool, url, schema)) = setup().await else {
        eprintln!("skipping: WASHBURN_TEST_DATABASE_URL not set");
        return;
    };
    let site = server().await;
    add_seed(&pool, &format!("{}/a", site.base), None, Some("test"))
        .await
        .unwrap();

    let outcome = run_with_pool(
        config(&site.base),
        pool.clone(),
        RunMode::once().crawl_only(true),
    )
    .await
    .unwrap();
    assert_eq!(outcome, RunOutcome::Drained);

    let statuses: Vec<String> =
        sqlx::query_scalar("SELECT status::text FROM queue ORDER BY url")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(statuses, vec!["done", "done"]);
    assert_eq!(count(&pool, "SELECT count(*) FROM pages").await, 2);
    assert_eq!(count(&pool, "SELECT count(*) FROM screenings").await, 0);
    assert_eq!(count(&pool, "SELECT count(*) FROM findings").await, 0);
    assert_eq!(site.mock.jev_calls.load(Ordering::SeqCst), 0);
    assert_eq!(site.mock.judge_calls.load(Ordering::SeqCst), 0);

    drop(pool);
    teardown(&url, &schema).await;
}

#[tokio::test]
async fn disabled_seed_is_not_crawled() {
    let Some((pool, url, schema)) = setup().await else {
        eprintln!("skipping: WASHBURN_TEST_DATABASE_URL not set");
        return;
    };
    let site = server().await;
    add_seed(&pool, &format!("{}/a", site.base), None, Some("test"))
        .await
        .unwrap();
    sqlx::query("UPDATE seeds SET enabled = false")
        .execute(&pool)
        .await
        .unwrap();

    let outcome = run_with_pool(
        config(&site.base),
        pool.clone(),
        RunMode::once().crawl_only(true),
    )
    .await
    .unwrap();
    assert_eq!(outcome, RunOutcome::Drained);

    let statuses: Vec<String> = sqlx::query_scalar("SELECT status::text FROM queue")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(statuses, vec!["pending"], "disabled seed's row is left alone");
    assert_eq!(count(&pool, "SELECT count(*) FROM pages").await, 0);

    drop(pool);
    teardown(&url, &schema).await;
}
