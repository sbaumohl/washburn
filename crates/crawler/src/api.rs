use std::{sync::Arc, time::Duration};

use anyhow::{Result, anyhow, bail, ensure};
use reqwest::Client;
use serde::Serialize;
use serde_json::Value;
use tokio::{sync::Semaphore, time::sleep};

use crate::config::{Config, Secret};

/// Replaces every configured secret in every string and object key of `value`
/// with `[REDACTED]`. Shared by the CLI transport and the sweeper's event writer.
pub fn redact_secrets(value: &mut Value, secrets: &[Secret]) {
    match value {
        Value::String(text) => {
            for secret in secrets {
                *text = text.replace(secret.expose(), "[REDACTED]");
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_secrets(item, secrets);
            }
        }
        Value::Object(map) => {
            let original = std::mem::take(map);
            for (mut key, mut item) in original {
                for secret in secrets {
                    key = key.replace(secret.expose(), "[REDACTED]");
                }
                redact_secrets(&mut item, secrets);
                map.insert(key, item);
            }
        }
        _ => {}
    }
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    pub service: &'static str,
    pub status: u16,
    pub model: Option<String>,
    pub endpoint: String,
    pub message: String,
    pub code: Option<Value>,
    pub failed_routing_step: Option<String>,
    pub routing_reasons: Vec<RoutingReason>,
}

#[derive(Debug, Serialize)]
pub struct RoutingReason {
    pub reason: String,
    pub endpoint_count: Option<u64>,
    pub configure_url: Option<String>,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} returned HTTP {} (model: {}; endpoint: {}): {}",
            self.service,
            self.status,
            self.model.as_deref().unwrap_or("unknown"),
            self.endpoint,
            self.message
        )
    }
}

impl std::error::Error for ApiError {}

/// Shared bounded transport for both classifier and generative judge.
#[derive(Clone)]
pub(crate) struct ApiClient {
    client: Client,
    api_key: Secret,
    secrets: Arc<Vec<Secret>>,
    endpoint: String,
    service: &'static str,
    retries: usize,
    permits: Arc<Semaphore>,
}

impl ApiClient {
    pub fn new(
        config: &Config,
        key: Secret,
        secrets: Vec<Secret>,
        endpoint: String,
        service: &'static str,
        concurrency: usize,
    ) -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(config.judge_timeout_secs))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            api_key: key,
            secrets: Arc::new(secrets),
            endpoint,
            service,
            retries: config.retries,
            permits: Arc::new(Semaphore::new(concurrency)),
        })
    }

    pub async fn post(&self, body: &Value) -> Result<Value> {
        let _permit = self.permits.acquire().await?;
        let service = self.service;
        for attempt in 0..=self.retries {
            let response = self
                .client
                .post(&self.endpoint)
                .bearer_auth(self.api_key.expose())
                .header("X-Title", "cn-swarms")
                .json(body)
                .send()
                .await;
            match response {
                Ok(mut response) => {
                    let status = response.status();
                    if (status.as_u16() == 429 || status.is_server_error())
                        && attempt < self.retries
                    {
                        let delay = response
                            .headers()
                            .get("retry-after")
                            .and_then(|h| h.to_str().ok())
                            .and_then(|h| h.parse::<u64>().ok())
                            .unwrap_or(1_u64 << attempt.min(6))
                            .min(60);
                        sleep(Duration::from_secs(delay)).await;
                        continue;
                    }
                    let mut bytes = Vec::new();
                    while let Some(chunk) = response
                        .chunk()
                        .await
                        .map_err(|_| anyhow!("{service} response body read failed"))?
                    {
                        ensure!(
                            chunk.len() <= 2_000_000_usize.saturating_sub(bytes.len()),
                            "{service} response exceeds byte limit"
                        );
                        bytes.extend_from_slice(&chunk);
                    }
                    let parsed = serde_json::from_slice(&bytes);
                    let mut value: Value = match parsed {
                        Ok(value) => value,
                        Err(_) if !status.is_success() => {
                            return Err(self.api_error(status.as_u16(), body, None).into());
                        }
                        Err(_) => bail!("{service} returned invalid JSON"),
                    };
                    self.redact(&mut value);
                    if !status.is_success() || value.get("error").is_some() {
                        return Err(self.api_error(status.as_u16(), body, Some(&value)).into());
                    }
                    return Ok(value);
                }
                Err(_) if attempt < self.retries => {
                    sleep(Duration::from_secs(1_u64 << attempt.min(6))).await
                }
                Err(_) => bail!("{service} transport failed after retries"),
            }
        }
        unreachable!()
    }

    fn api_error(&self, status: u16, request: &Value, response: Option<&Value>) -> ApiError {
        // Keep only useful diagnostic fields, after scrubbing every configured key.
        // Never store the raw error body or upstream metadata such as headers/request payloads.
        let error = response.and_then(|r| r.get("error"));
        let message = error.and_then(|e| e.get("message").and_then(Value::as_str).or_else(|| e.as_str()))
            .map(|s| s.chars().take(4096).collect())
            .unwrap_or_else(|| "No structured provider error message was returned; check the endpoint, model, and account routing policies".into());
        let metadata = error.and_then(|e| e.get("metadata"));
        let routing_reasons = metadata
            .and_then(|m| m.get("ineligibility_reasons"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .take(32)
                    .filter_map(|item| {
                        Some(RoutingReason {
                            reason: item.get("reason")?.as_str()?.chars().take(1024).collect(),
                            endpoint_count: item.get("endpoint_count").and_then(Value::as_u64),
                            configure_url: item
                                .get("configure_url")
                                .and_then(Value::as_str)
                                .map(|s| s.chars().take(1024).collect()),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        ApiError {
            service: self.service,
            status,
            model: request
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned),
            endpoint: self.endpoint.clone(),
            message,
            code: error
                .and_then(|e| e.get("code"))
                .filter(|c| c.is_number() || c.is_string())
                .cloned(),
            failed_routing_step: metadata
                .and_then(|m| m.get("failed_routing_step"))
                .and_then(Value::as_str)
                .map(|s| s.chars().take(1024).collect()),
            routing_reasons,
        }
    }

    fn redact(&self, value: &mut Value) {
        redact_secrets(value, &self.secrets);
    }
}
