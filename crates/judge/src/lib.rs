//! HTTP wrapper around `cn_swarms::judge::Judge`. One process per crawler VM;
//! sweepers POST level-one chunks to `/v1/judge`.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, State},
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use cn_swarms::judge::Judge;
use serde_json::json;
use tokio::sync::Semaphore;
use tower_http::{timeout::TimeoutLayer, trace::TraceLayer};
use tracing::{Level, info_span};
use washburn_common::{JudgeError, JudgeErrorKind, JudgeRequest, JudgeResponse, WashburnConfig};

/// Rubric location read when `WASHBURN_RUBRIC` is unset.
pub const DEFAULT_RUBRIC_PATH: &str = "/etc/washburn/rubric.md";
pub const RUBRIC_ENV: &str = "WASHBURN_RUBRIC";
/// Headroom above `[judge].timeout_secs` before the whole request is cut off.
const REQUEST_TIMEOUT_SLACK: Duration = Duration::from_secs(10);
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone)]
pub struct AppState {
    judge: Judge,
    permits: Arc<Semaphore>,
    capacity: usize,
    wait: Duration,
    request_timeout: Duration,
    model: String,
}

impl AppState {
    /// Builds the judge and the concurrency gate from the loaded config.
    /// Errors never include config values.
    pub fn new(config: &WashburnConfig, rubric: Arc<String>) -> Result<Self> {
        let services = config.services()?;
        let crawler = config.crawler_config();
        let judge = Judge::new(&crawler, &services, rubric).context("build judge client")?;
        let wait = Duration::from_secs(config.judge.timeout_secs);
        Ok(Self {
            judge,
            permits: Arc::new(Semaphore::new(config.judge.concurrency)),
            capacity: config.judge.concurrency,
            wait,
            request_timeout: wait + REQUEST_TIMEOUT_SLACK,
            model: services.openrouter.model.clone(),
        })
    }
}

/// Router with `/v1/judge` and `/healthz`, plus body, timeout and trace layers.
pub fn build_app(state: AppState) -> Router {
    let request_timeout = state.request_timeout;
    Router::new()
        .route("/v1/judge", post(judge))
        .route("/healthz", get(healthz))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::GATEWAY_TIMEOUT,
            request_timeout,
        ))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &Request<Body>| {
                    info_span!(
                        "request",
                        method = %request.method(),
                        path = %request.uri().path(),
                    )
                })
                .on_response(
                    tower_http::trace::DefaultOnResponse::new()
                        .level(Level::INFO)
                        .latency_unit(tower_http::LatencyUnit::Millis),
                ),
        )
        .with_state(state)
}

async fn judge(State(state): State<AppState>, Json(request): Json<JudgeRequest>) -> Response {
    let permit = match tokio::time::timeout(state.wait, state.permits.clone().acquire_owned()).await
    {
        Ok(Ok(permit)) => permit,
        Ok(Err(_)) => return error_response(JudgeError::busy("judge is shutting down")),
        Err(_) => {
            return error_response(JudgeError::busy("all judge slots are in use; retry later"));
        }
    };
    let result = state
        .judge
        .evaluate_text(
            &request.meta(),
            &request.text,
            request.chunk,
            &request.screening,
        )
        .await;
    drop(permit);
    match result {
        Ok(judgment) => (StatusCode::OK, Json(JudgeResponse { judgment })).into_response(),
        Err(error) => {
            let body = JudgeError::from_judge_error(&error);
            tracing::warn!(kind = ?body.kind, "judge evaluation failed");
            error_response(body)
        }
    }
}

fn error_response(body: JudgeError) -> Response {
    let status = match body.kind {
        JudgeErrorKind::Rejected => StatusCode::UNPROCESSABLE_ENTITY,
        JudgeErrorKind::Provider => StatusCode::BAD_GATEWAY,
        JudgeErrorKind::Busy => StatusCode::SERVICE_UNAVAILABLE,
        JudgeErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, Json(body)).into_response()
}

async fn healthz(State(state): State<AppState>) -> Json<serde_json::Value> {
    let in_flight = state.capacity - state.permits.available_permits().min(state.capacity);
    Json(json!({
        "ok": true,
        "model": state.model,
        "in_flight": in_flight,
        "capacity": state.capacity,
    }))
}

/// Rubric from `WASHBURN_RUBRIC`, else `/etc/washburn/rubric.md` if present, else empty.
pub fn load_rubric() -> Result<Arc<String>> {
    let path = match std::env::var_os(RUBRIC_ENV).filter(|value| !value.is_empty()) {
        Some(path) => Some(PathBuf::from(path)),
        None => {
            let default = Path::new(DEFAULT_RUBRIC_PATH);
            default.exists().then(|| default.to_path_buf())
        }
    };
    let text = match path {
        Some(path) => fs::read_to_string(&path)
            .with_context(|| format!("read rubric at {}", path.display()))?,
        None => String::new(),
    };
    Ok(Arc::new(text))
}

/// Resolves when SIGINT or SIGTERM arrives.
pub async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut terminate = match signal(SignalKind::terminate()) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::error!(%error, "cannot install SIGTERM handler");
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
    tracing::info!("shutdown signal received; finishing in-flight evaluations");
}

/// Installs the global subscriber: JSON when `WASHBURN_LOG_JSON=1`, else pretty.
pub fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let json = std::env::var("WASHBURN_LOG_JSON").is_ok_and(|value| value == "1");
    if json {
        fmt().with_env_filter(filter).json().init();
    } else {
        fmt().with_env_filter(filter).init();
    }
}
