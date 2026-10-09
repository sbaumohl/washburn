//! Database-backed tests. Set `WASHBURN_TEST_DATABASE_URL` to a disposable
//! Postgres 16 (for example `docker run -d --rm -e POSTGRES_PASSWORD=pw -p 55432:5432 postgres:16`).
//! Each run uses its own schema, so existing data in that database is untouched.
//! Without the variable every test returns early.

use std::time::{SystemTime, UNIX_EPOCH};

use chrono::Utc;
use cn_swarms::{
    jev::Screening,
    judge::{Classification, Judgment, Verdict},
};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use url::Url;
use washburn_common::{
    HeartbeatRow, StorePage, add_seed, delete_heartbeat, fail_job, findings_for_page, get_host,
    get_queue, heartbeat, insert_event, insert_judgment, insert_screening, judgments_for_page,
    lease_host, lease_job, migrate, page_for_queue, queue_depth, release_host, renew_host,
    screenings_for_page, seed_urls_for_host, skip_job, store_page, upsert_level_one_finding,
    upsert_report_finding,
};

/// Returns a pool whose connections all use a fresh schema, after migrating it.
async fn setup() -> Option<(PgPool, String, String)> {
    let url = std::env::var("WASHBURN_TEST_DATABASE_URL").ok()?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let schema = format!("wt_test_{}_{nanos}", std::process::id());
    let admin = PgPool::connect(&url)
        .await
        .expect("connect to test database");
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

fn sample_screening(chunk: usize, importance: f64) -> Screening {
    Screening {
        chunk,
        requested_model: "jev-latest".into(),
        model: "jev-latest".into(),
        usage: json!({"tokens": 10}),
        importance,
        agent_activity: 0.9,
        potentially_serious: 0.85,
    }
}

fn sample_judgment(chunk: usize) -> Judgment {
    Judgment {
        stage: "review".into(),
        chunk,
        requested_model: "provider/strong".into(),
        model: "provider/strong".into(),
        usage: Value::Null,
        verdict: Verdict {
            activity: 0.95,
            seriousness: 0.9,
            confidence: 0.9,
            classification: Classification::SuspectedUnauthorized,
            summary: "Agent read credentials.".into(),
            evidence: vec!["cat ~/.aws/credentials".into()],
            uncertainties: vec!["No identity corroboration.".into()],
            confirmed_serious: true,
        },
    }
}

#[tokio::test]
async fn seed_lease_store_and_depth_flow() {
    let Some((pool, url, schema)) = setup().await else {
        eprintln!("skipping: WASHBURN_TEST_DATABASE_URL not set");
        return;
    };

    let seed_id = add_seed(&pool, "https://Example.test/", Some("note"), Some("tester"))
        .await
        .unwrap();
    let again = add_seed(&pool, "https://example.test/", None, None)
        .await
        .unwrap();
    assert_eq!(seed_id, again, "re-adding a seed returns the same id");
    assert_eq!(
        queue_depth(&pool).await.unwrap(),
        vec![("example.test".to_owned(), 1)]
    );

    let seeds = seed_urls_for_host(&pool, "example.test").await.unwrap();
    assert_eq!(seeds, vec![Url::parse("https://example.test/").unwrap()]);

    let host = lease_host(&pool, "w1", 60.0).await.unwrap();
    assert_eq!(host.as_deref(), Some("example.test"));
    assert!(
        lease_host(&pool, "w2", 60.0).await.unwrap().is_none(),
        "host is leased"
    );
    assert!(renew_host(&pool, "w1", "example.test", 60.0).await.unwrap());
    assert!(!renew_host(&pool, "w2", "example.test", 60.0).await.unwrap());

    let job = lease_job(&pool, "w1", "example.test", 60.0)
        .await
        .unwrap()
        .expect("seed job");
    assert_eq!(job.status, "leased");
    assert_eq!(job.depth, 0);

    let fetched_at = Utc::now();
    let body = b"<html>agent trace</html>".repeat(50);
    let links = vec![
        Url::parse("https://example.test/a").unwrap(),
        Url::parse("https://example.test/b").unwrap(),
    ];
    let page_id = store_page(
        &pool,
        StorePage {
            worker: "w1",
            queue_id: job.id,
            url: &job.url,
            final_url: &job.url,
            seed_id: job.seed_id,
            depth: job.depth,
            status: 200,
            content_type: Some("text/html"),
            fetched_at,
            body: &body,
            text: "agent trace",
            redirects: &[],
            links: &links,
            meta: json!({"title": "Trace"}),
            max_depth: 2,
        },
    )
    .await
    .unwrap();

    let done = get_queue(&pool, job.id).await.unwrap().unwrap();
    assert_eq!(done.status, "done");
    assert!(done.finished_at.is_some());
    assert_eq!(
        queue_depth(&pool).await.unwrap(),
        vec![("example.test".to_owned(), 2)]
    );
    assert_eq!(
        get_host(&pool, "example.test")
            .await
            .unwrap()
            .unwrap()
            .pending_count,
        2
    );

    let page = page_for_queue(&pool, job.id).await.unwrap().unwrap();
    assert_eq!(page.id, page_id);
    assert_eq!(page.meta["title"], "Trace");
    assert_eq!(page.links.as_array().unwrap().len(), 2);

    // Storing the same links again (from the child page) enqueues nothing new.
    let child = lease_job(&pool, "w1", "example.test", 60.0)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(child.depth, 1);
    assert_eq!(child.parent_url.as_deref(), Some("https://example.test/"));
    store_page(
        &pool,
        StorePage {
            worker: "w1",
            queue_id: child.id,
            url: &child.url,
            final_url: &child.url,
            seed_id: child.seed_id,
            depth: child.depth,
            status: 200,
            content_type: None,
            fetched_at,
            body: b"child",
            text: "child",
            redirects: &[],
            links: &links,
            meta: json!({}),
            max_depth: 2,
        },
    )
    .await
    .unwrap();
    // "a" is done; only "b" remains pending. Duplicate links were dropped by url UNIQUE.
    assert_eq!(
        queue_depth(&pool).await.unwrap(),
        vec![("example.test".to_owned(), 1)]
    );
    assert_eq!(
        get_host(&pool, "example.test")
            .await
            .unwrap()
            .unwrap()
            .pending_count,
        1
    );

    // Failures on "b": first attempt returns to pending, second marks failed and decrements.
    let b = lease_job(&pool, "w1", "example.test", 60.0)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(b.url, "https://example.test/b");
    fail_job(&pool, b.id, "timeout", 2).await.unwrap();
    let b = get_queue(&pool, b.id).await.unwrap().unwrap();
    assert_eq!((b.status.as_str(), b.attempts), ("pending", 1));
    let b = lease_job(&pool, "w1", "example.test", 60.0)
        .await
        .unwrap()
        .unwrap();
    fail_job(&pool, b.id, "timeout again", 2).await.unwrap();
    assert_eq!(
        get_queue(&pool, b.id).await.unwrap().unwrap().status,
        "failed"
    );
    assert_eq!(
        get_host(&pool, "example.test")
            .await
            .unwrap()
            .unwrap()
            .pending_count,
        0
    );

    // Nothing left to lease; the host is not handed out again.
    assert!(
        lease_job(&pool, "w1", "example.test", 60.0)
            .await
            .unwrap()
            .is_none()
    );
    release_host(&pool, "w1", "example.test").await.unwrap();
    assert!(queue_depth(&pool).await.unwrap().is_empty());
    assert!(lease_host(&pool, "w1", 60.0).await.unwrap().is_none());

    // Evidence and findings for the seed page.
    let screening_id = insert_screening(&pool, page_id, &sample_screening(1, 0.91), 0.7)
        .await
        .unwrap();
    insert_screening(&pool, page_id, &sample_screening(2, 0.1), 0.7)
        .await
        .unwrap();
    let screenings = screenings_for_page(&pool, page_id).await.unwrap();
    assert_eq!(screenings.len(), 2);
    assert!(screenings[0].level_one && !screenings[1].level_one);

    insert_judgment(&pool, page_id, screening_id, &sample_judgment(1))
        .await
        .unwrap();
    let judgments = judgments_for_page(&pool, page_id).await.unwrap();
    assert_eq!(judgments[0].classification, "suspected_unauthorized");
    assert!(judgments[0].confirmed_serious);

    let section_one = washburn_common::level_one_section(&sample_screening(1, 0.91));
    let section_two = washburn_common::level_one_section(&sample_screening(2, 0.8));
    upsert_level_one_finding(&pool, page_id, "Trace", &section_one)
        .await
        .unwrap();
    upsert_level_one_finding(&pool, page_id, "Trace", &section_two)
        .await
        .unwrap();
    upsert_report_finding(&pool, page_id, "Serious", "# Serious\n")
        .await
        .unwrap();
    upsert_report_finding(&pool, page_id, "Serious", "# Serious v2\n")
        .await
        .unwrap();

    let findings = findings_for_page(&pool, page_id).await.unwrap();
    assert_eq!(findings.len(), 2);
    let level_one = findings.iter().find(|f| f.kind == "level_one").unwrap();
    assert!(
        level_one
            .markdown
            .starts_with("# Level-one agent activity finding")
    );
    assert_eq!(level_one.markdown.matches("## Jev — chunk").count(), 2);
    assert!(!level_one.published);
    let report = findings.iter().find(|f| f.kind == "report").unwrap();
    assert_eq!(report.markdown, "# Serious v2\n");

    insert_event(
        &pool,
        "w1",
        "page_completed",
        &json!({"url": "https://example.test/"}),
    )
    .await
    .unwrap();
    let beat = HeartbeatRow {
        worker: "host:1".into(),
        hostname: "host".into(),
        pid: 1,
        version: "0.1.0".into(),
        started_at: Utc::now(),
        last_seen: Utc::now(),
        active_jobs: 0,
        leased_host: None,
        detail: json!({}),
    };
    heartbeat(&pool, &beat).await.unwrap();
    heartbeat(&pool, &beat).await.unwrap();
    delete_heartbeat(&pool, "host:1").await.unwrap();

    teardown(&url, &schema).await;
}

#[tokio::test]
async fn links_beyond_max_depth_are_not_enqueued_and_skip_releases_host() {
    let Some((pool, url, schema)) = setup().await else {
        eprintln!("skipping: WASHBURN_TEST_DATABASE_URL not set");
        return;
    };
    add_seed(&pool, "https://deep.test/", None, None)
        .await
        .unwrap();
    let job = lease_job(&pool, "w", "deep.test", 30.0)
        .await
        .unwrap()
        .unwrap();
    let links = vec![Url::parse("https://deep.test/next").unwrap()];
    store_page(
        &pool,
        StorePage {
            worker: "w",
            queue_id: job.id,
            url: &job.url,
            final_url: &job.url,
            seed_id: job.seed_id,
            depth: 2,
            status: 200,
            content_type: None,
            fetched_at: Utc::now(),
            body: b"x",
            text: "x",
            redirects: &[],
            links: &links,
            meta: json!({}),
            max_depth: 2,
        },
    )
    .await
    .unwrap();
    assert!(queue_depth(&pool).await.unwrap().is_empty());

    add_seed(&pool, "https://skip.test:8443/", None, None)
        .await
        .unwrap();
    let job = lease_job(&pool, "w", "skip.test:8443", 30.0)
        .await
        .unwrap()
        .unwrap();
    skip_job(&pool, job.id, "robots.txt disallows")
        .await
        .unwrap();
    let skipped = get_queue(&pool, job.id).await.unwrap().unwrap();
    assert_eq!(skipped.status, "skipped");
    assert!(queue_depth(&pool).await.unwrap().is_empty());

    teardown(&url, &schema).await;
}
