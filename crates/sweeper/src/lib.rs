//! The sweeper daemon (PROJECTS.md 3.4): lease a host, drain its queue with
//! bounded concurrency, screen each page with Jev, review flagged chunks with
//! the judge service, and persist everything through `washburn_common::repo`.
//!
//! Main loop:
//! 1. `lease_host`; none free -> `NoHost` under `--once`, else pause and retry.
//! 2. Build a `Crawler` scoped to the host's enabled seeds; start lease renewal.
//! 3. Lease jobs up to `[crawl].concurrency` in flight. Keep leasing until
//!    `lease_job` returns nothing and no job is in flight, or the hold time
//!    (forever mode) or a shutdown signal ends the host.
//! 4. `release_host`, then back to step 1.
//!
//! On SIGTERM/SIGINT no new jobs are leased; in-flight jobs get up to
//! `2 x max(crawl, judge) timeout_secs`, then the heartbeat is stopped and
//! removed.

pub mod judge;
pub mod process;

use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use cn_swarms::{config::Config as CrawlerConfig, config::Secret, crawl::Crawler, jev::Jev};
use serde_json::json;
use sqlx::PgPool;
use tokio::{
    sync::{Mutex, watch},
    task::{JoinHandle, JoinSet},
    time::{Instant, sleep, timeout_at},
};
use tracing::{error, info, warn};
use url::Url;
use washburn_common::{
    HeartbeatRow, WashburnConfig, delete_heartbeat, heartbeat, lease_host, lease_job, migrate,
    reclaim_stale_jobs, release_host, renew_host, seeds_by_id_for_host, worker_id,
};

use judge::JudgeClient;
use process::{JobContext, process_job};

const IDLE_PAUSE: Duration = Duration::from_secs(5);
const RECLAIM_EVERY: Duration = Duration::from_secs(60);

/// How the daemon runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunMode {
    /// Drain one host and return instead of looping.
    pub once: bool,
    /// Store pages without Jev or the judge.
    pub crawl_only: bool,
    /// Apply migrations at startup (idempotent).
    pub migrate: bool,
}

impl RunMode {
    pub fn forever() -> Self {
        Self {
            once: false,
            crawl_only: false,
            migrate: true,
        }
    }

    pub fn once() -> Self {
        Self {
            once: true,
            ..Self::forever()
        }
    }

    pub fn crawl_only(self, crawl_only: bool) -> Self {
        Self { crawl_only, ..self }
    }
}

/// Why `run` returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    /// `--once`: the leased host was drained (or had nothing to do).
    Drained,
    /// `--once`: no host had pending work that could be leased. CLI exit 3.
    NoHost,
    /// A signal stopped the daemon. `sigint` distinguishes Ctrl-C (CLI exit 130)
    /// from SIGTERM (exit 0).
    Shutdown { sigint: bool },
}

/// Connects with `[postgres]`, then runs. See `run_with_pool`.
pub async fn run(config: WashburnConfig, mode: RunMode) -> Result<RunOutcome> {
    let pool = washburn_common::connect(&config.postgres).await?;
    run_with_pool(config, pool, mode).await
}

/// Runs the daemon on an existing pool. Tests use this to pin a search_path.
pub async fn run_with_pool(config: WashburnConfig, pool: PgPool, mode: RunMode) -> Result<RunOutcome> {
    if mode.migrate {
        migrate(&pool).await?;
    }
    let worker = worker_id();
    let started = Utc::now();
    let crawler_config = Arc::new(config.crawler_config());
    let secrets = vec![
        config.typesafe.api_key.clone(),
        config.openrouter.api_key.clone(),
        config.postgres.password.clone(),
    ];
    let (jev, judge) = if mode.crawl_only {
        (None, None)
    } else {
        let services = config.services()?;
        let jev = Jev::new(&crawler_config, &services, Arc::new(load_rubric()?))?;
        let judge = JudgeClient::new(
            &config.judge.url,
            Duration::from_secs(config.judge.timeout_secs + 15),
        )?;
        if let Err(error) = judge.health().await {
            warn!(error = %error, "judge is not reachable yet; flagged chunks will record judge_error until it is");
        }
        (Some(jev), Some(judge))
    };

    match reclaim_stale_jobs(&pool).await {
        Ok(count) => info!(reclaimed = count, "reclaimed stale job leases at startup"),
        Err(error) => warn!(error = %error, "startup lease reclaim failed"),
    }

    let live = Arc::new(Live::default());
    let (signal_tx, signal_rx) = watch::channel(None::<bool>);
    let signal_task = spawn_signal_listener(signal_tx);
    let (hb_stop_tx, hb_stop_rx) = watch::channel(false);
    let heartbeat_every = Duration::from_secs(config.crawl.heartbeat_secs.max(1));
    let heartbeat_task = spawn_heartbeat(
        pool.clone(),
        worker.clone(),
        started,
        live.clone(),
        heartbeat_every,
        hb_stop_rx,
    );

    info!(
        worker = %worker,
        once = mode.once,
        crawl_only = mode.crawl_only,
        concurrency = crawler_config.concurrency,
        "sweeper started"
    );
    let shared = Shared {
        pool: pool.clone(),
        worker: worker.clone(),
        config: crawler_config,
        jev,
        judge,
        secrets,
        live,
        host_lease_secs: config.crawl.host_lease_secs,
        drain_budget: Duration::from_secs(
            2 * config.judge.timeout_secs.max(config.crawl.timeout_secs),
        ),
        once: mode.once,
        shutdown: Shutdown(signal_rx),
    };
    let outcome = run_loop(&shared).await;

    signal_task.abort();
    let _ = hb_stop_tx.send(true);
    if let Err(error) = heartbeat_task.await {
        warn!(error = %error, "heartbeat task ended abnormally");
    }
    if let Err(error) = delete_heartbeat(&pool, &worker).await {
        warn!(error = %error, "could not delete heartbeat row");
    }
    info!(worker = %worker, "sweeper stopped");
    outcome
}

/// Shared state for every host this process works on.
struct Shared {
    pool: PgPool,
    worker: String,
    config: Arc<CrawlerConfig>,
    jev: Option<Jev>,
    judge: Option<JudgeClient>,
    secrets: Vec<Secret>,
    live: Arc<Live>,
    host_lease_secs: u64,
    drain_budget: Duration,
    once: bool,
    shutdown: Shutdown,
}

/// Values the heartbeat row reports.
#[derive(Default)]
struct Live {
    active_jobs: AtomicUsize,
    leased_host: Mutex<Option<String>>,
}

/// `None` while running; `Some(sigint)` once a signal arrived.
#[derive(Clone)]
struct Shutdown(watch::Receiver<Option<bool>>);

impl Shutdown {
    fn requested(&self) -> Option<bool> {
        *self.0.borrow()
    }

    async fn wait(&self) -> bool {
        let mut rx = self.0.clone();
        loop {
            if let Some(sigint) = *rx.borrow_and_update() {
                return sigint;
            }
            if rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

async fn run_loop(shared: &Shared) -> Result<RunOutcome> {
    loop {
        if let Some(sigint) = shared.shutdown.requested() {
            return Ok(RunOutcome::Shutdown { sigint });
        }
        let host = match lease_host(&shared.pool, &shared.worker, shared.host_lease_secs as f64).await {
            Ok(Some(host)) => host,
            Ok(None) => {
                if shared.once {
                    return Ok(RunOutcome::NoHost);
                }
                pause(&shared.shutdown, IDLE_PAUSE).await;
                continue;
            }
            Err(error) => {
                if shared.once {
                    return Err(error).context("lease a host");
                }
                warn!(error = %error, "lease_host failed; retrying");
                pause(&shared.shutdown, IDLE_PAUSE).await;
                continue;
            }
        };
        info!(host = %host, "leased host");
        let leased = match drain_host(shared, &host).await {
            Ok(leased) => leased,
            Err(error) => {
                if shared.once {
                    return Err(error);
                }
                error!(host = %host, error = %error, "host drain failed; will retry");
                pause(&shared.shutdown, IDLE_PAUSE).await;
                0
            }
        };
        if let Some(sigint) = shared.shutdown.requested() {
            return Ok(RunOutcome::Shutdown { sigint });
        }
        if shared.once {
            return Ok(RunOutcome::Drained);
        }
        if leased == 0 {
            // Every pending row on this host belongs to a disabled seed, or
            // the host's seeds were removed. Do not spin on it.
            pause(&shared.shutdown, IDLE_PAUSE).await;
        }
    }
}

async fn pause(shutdown: &Shutdown, duration: Duration) {
    tokio::select! {
        _ = sleep(duration) => {}
        _ = shutdown.wait() => {}
    }
}

/// Leases and runs jobs on one host until it is drained or must be given up.
/// Returns how many jobs were leased. The host is always released.
async fn drain_host(shared: &Shared, host: &str) -> Result<usize> {
    let seeds = seeds_by_id_for_host(&shared.pool, host).await?;
    if seeds.is_empty() {
        warn!(host = %host, "host has no enabled seeds; releasing it");
        release(shared, host).await;
        return Ok(0);
    }
    let scope: Vec<Url> = seeds.iter().map(|(_, url)| url.clone()).collect();
    let seed_map: HashMap<i64, Url> = seeds.into_iter().collect();
    let crawler = Crawler::new(shared.config.clone(), &scope)?;
    let ctx = Arc::new(JobContext {
        pool: shared.pool.clone(),
        worker: shared.worker.clone(),
        crawler,
        config: shared.config.clone(),
        jev: shared.jev.clone(),
        judge: shared.judge.clone(),
        secrets: shared.secrets.clone(),
        seeds: seed_map,
    });
    *shared.live.leased_host.lock().await = Some(host.to_owned());

    let lost = Arc::new(AtomicBool::new(false));
    let (renew_stop_tx, renew_stop_rx) = watch::channel(false);
    let renewal = spawn_renewal(
        shared.pool.clone(),
        shared.worker.clone(),
        host.to_owned(),
        shared.host_lease_secs as f64,
        lost.clone(),
        renew_stop_rx,
    );
    let result = run_jobs(shared, &ctx, host, &lost).await;
    let _ = renew_stop_tx.send(true);
    if let Err(error) = renewal.await {
        warn!(error = %error, "lease renewal task ended abnormally");
    }
    release(shared, host).await;
    *shared.live.leased_host.lock().await = None;
    result
}

async fn release(shared: &Shared, host: &str) {
    if let Err(error) = release_host(&shared.pool, &shared.worker, host).await {
        warn!(host = %host, error = %error, "could not release host lease");
    }
}

/// The lease loop for one host. Returns the number of jobs leased.
async fn run_jobs(shared: &Shared, ctx: &Arc<JobContext>, host: &str, lost: &AtomicBool) -> Result<usize> {
    let concurrency = shared.config.concurrency.max(1);
    let lease_secs = shared.host_lease_secs as f64;
    let hold_limit = Duration::from_secs(shared.host_lease_secs);
    let held_since = Instant::now();
    let mut tasks: JoinSet<()> = JoinSet::new();
    let mut leased = 0;
    let mut deadline: Option<Instant> = None;

    loop {
        if deadline.is_none() && shared.shutdown.requested().is_some() {
            deadline = Some(Instant::now() + shared.drain_budget);
            info!(host = %host, in_flight = tasks.len(), "shutdown requested; finishing in-flight jobs");
        }
        let rotate = !shared.once && held_since.elapsed() >= hold_limit;
        let may_lease = deadline.is_none() && !rotate && !lost.load(Ordering::SeqCst);
        while may_lease && tasks.len() < concurrency {
            match lease_job(&shared.pool, &shared.worker, host, lease_secs).await {
                Ok(Some(row)) => {
                    leased += 1;
                    shared.live.active_jobs.fetch_add(1, Ordering::SeqCst);
                    let ctx = ctx.clone();
                    let live = shared.live.clone();
                    tasks.spawn(async move {
                        process_job(&ctx, row).await;
                        live.active_jobs.fetch_sub(1, Ordering::SeqCst);
                    });
                }
                Ok(None) => break,
                Err(error) => {
                    warn!(host = %host, error = %error, "lease_job failed; stopping this host");
                    break;
                }
            }
        }
        if tasks.is_empty() {
            break;
        }
        if rotate && !lost.load(Ordering::SeqCst) {
            info!(host = %host, "host hold time reached; rotating");
        }
        let next = match deadline {
            Some(at) => match timeout_at(at, tasks.join_next()).await {
                Ok(next) => next,
                Err(_) => {
                    warn!(host = %host, in_flight = tasks.len(), "drain budget exceeded; aborting in-flight jobs (leases are reclaimed later)");
                    tasks.abort_all();
                    while tasks.join_next().await.is_some() {}
                    break;
                }
            },
            None => tokio::select! {
                next = tasks.join_next() => next,
                _ = shared.shutdown.wait() => continue,
            },
        };
        if let Some(Err(error)) = next {
            error!(host = %host, error = %error, "job task failed");
        }
    }
    Ok(leased)
}

fn spawn_signal_listener(tx: watch::Sender<Option<bool>>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut terminate = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(signal) => Some(signal),
            Err(error) => {
                warn!(error = %error, "cannot listen for SIGTERM");
                None
            }
        };
        let sigint = tokio::select! {
            _ = tokio::signal::ctrl_c() => true,
            _ = async {
                match terminate.as_mut() {
                    Some(signal) => { signal.recv().await; }
                    None => std::future::pending::<()>().await,
                }
            } => false,
        };
        info!(sigint, "shutdown signal received");
        let _ = tx.send(Some(sigint));
    })
}

fn spawn_heartbeat(
    pool: PgPool,
    worker: String,
    started: DateTime<Utc>,
    live: Arc<Live>,
    every: Duration,
    mut stop: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut last_reclaim = Instant::now();
        loop {
            if let Err(error) = write_heartbeat(&pool, &worker, started, &live).await {
                warn!(error = %error, "heartbeat write failed");
            }
            if last_reclaim.elapsed() >= RECLAIM_EVERY {
                last_reclaim = Instant::now();
                match reclaim_stale_jobs(&pool).await {
                    Ok(count) if count > 0 => info!(reclaimed = count, "reclaimed expired job leases"),
                    Ok(_) => {}
                    Err(error) => warn!(error = %error, "lease reclaim failed"),
                }
            }
            tokio::select! {
                _ = sleep(every) => {}
                _ = stop.changed() => break,
            }
        }
    })
}

async fn write_heartbeat(pool: &PgPool, worker: &str, started: DateTime<Utc>, live: &Live) -> Result<()> {
    let row = HeartbeatRow {
        worker: worker.to_owned(),
        hostname: worker.rsplit_once(':').map_or(worker, |(host, _)| host).to_owned(),
        pid: i32::try_from(std::process::id()).unwrap_or(0),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        started_at: started,
        last_seen: Utc::now(),
        active_jobs: i32::try_from(live.active_jobs.load(Ordering::SeqCst)).unwrap_or(i32::MAX),
        leased_host: live.leased_host.lock().await.clone(),
        detail: json!({}),
    };
    heartbeat(pool, &row).await?;
    Ok(())
}

fn spawn_renewal(
    pool: PgPool,
    worker: String,
    host: String,
    lease_secs: f64,
    lost: Arc<AtomicBool>,
    mut stop: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let every = Duration::from_secs_f64((lease_secs / 3.0).max(1.0));
        loop {
            tokio::select! {
                _ = sleep(every) => {}
                _ = stop.changed() => break,
            }
            match renew_host(&pool, &worker, &host, lease_secs).await {
                Ok(true) => {}
                Ok(false) => {
                    warn!(host = %host, "host lease was lost; no new jobs will be leased from it");
                    lost.store(true, Ordering::SeqCst);
                    break;
                }
                Err(error) => warn!(host = %host, error = %error, "lease renewal failed; retrying"),
            }
        }
    })
}

/// `WASHBURN_RUBRIC`, else `/etc/washburn/rubric.md` if present, else empty.
fn load_rubric() -> Result<String> {
    let path = std::env::var_os("WASHBURN_RUBRIC")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let default = PathBuf::from("/etc/washburn/rubric.md");
            default.exists().then_some(default)
        });
    match path {
        Some(path) => fs::read_to_string(&path)
            .with_context(|| format!("read rubric at {}", path.display())),
        None => Ok(String::new()),
    }
}
