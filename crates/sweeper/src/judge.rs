//! HTTP client for the judge service (`crates/judge`, PROJECTS.md 3.3).

use std::time::Duration;

use anyhow::{Context, Result, ensure};
use cn_swarms::judge::Judgment;
use reqwest::Client;
use serde_json::Value;
use washburn_common::{JudgeError, JudgeRequest, JudgeResponse};

/// A failed judge call, shaped for the `judge_error` event.
#[derive(Clone, Debug, PartialEq)]
pub struct JudgeFailure {
    /// `rejected`, `provider`, `busy`, `internal`, `transport`, or `unexpected`.
    pub kind: &'static str,
    /// HTTP status, or `None` when no response arrived.
    pub status: Option<u16>,
    pub reason: String,
    pub api_error: Option<Value>,
}

impl JudgeFailure {
    fn transport(reason: String) -> Self {
        Self {
            kind: "transport",
            status: None,
            reason,
            api_error: None,
        }
    }
}

/// Maps a non-200 judge response to a failure. A parseable `JudgeError` body
/// wins; otherwise the status code decides. Unmapped 5xx (including 504 from
/// the judge's timeout layer, which has an empty body) are `provider`;
/// unmapped 4xx are `internal`.
pub fn judge_failure(status: u16, body: &[u8]) -> JudgeFailure {
    if let Ok(parsed) = serde_json::from_slice::<JudgeError>(body) {
        return JudgeFailure {
            kind: kind_label(parsed.kind),
            status: Some(status),
            reason: parsed.reason,
            api_error: parsed.api_error,
        };
    }
    let kind = match status {
        422 => "rejected",
        503 => "busy",
        500 => "internal",
        502 | 504 => "provider",
        501..=599 => "provider",
        400..=499 => "internal",
        _ => "unexpected",
    };
    JudgeFailure {
        kind,
        status: Some(status),
        reason: format!("judge returned HTTP {status}"),
        api_error: None,
    }
}

fn kind_label(kind: washburn_common::JudgeErrorKind) -> &'static str {
    use washburn_common::JudgeErrorKind as K;
    match kind {
        K::Rejected => "rejected",
        K::Provider => "provider",
        K::Busy => "busy",
        K::Internal => "internal",
    }
}

#[derive(Clone)]
pub struct JudgeClient {
    http: Client,
    base: String,
}

impl JudgeClient {
    pub fn new(base_url: &str, timeout: Duration) -> Result<Self> {
        Ok(Self {
            http: Client::builder()
                .timeout(timeout)
                .build()
                .context("build judge HTTP client")?,
            base: base_url.trim_end_matches('/').to_owned(),
        })
    }

    /// `GET /healthz`. Callers log a warning on failure and continue.
    pub async fn health(&self) -> Result<()> {
        let response = self
            .http
            .get(format!("{}/healthz", self.base))
            .send()
            .await
            .context("judge /healthz unreachable")?;
        ensure!(
            response.status().is_success(),
            "judge /healthz returned HTTP {}",
            response.status().as_u16()
        );
        Ok(())
    }

    /// `POST /v1/judge`. Request bodies are never logged.
    pub async fn review(&self, request: &JudgeRequest) -> Result<Judgment, JudgeFailure> {
        let response = self
            .http
            .post(format!("{}/v1/judge", self.base))
            .json(request)
            .send()
            .await
            .map_err(|error| JudgeFailure::transport(format!("judge request failed: {}", error.without_url())))?;
        let status = response.status().as_u16();
        let body = response
            .bytes()
            .await
            .map_err(|error| JudgeFailure::transport(format!("judge response failed: {}", error.without_url())))?;
        if status == 200 {
            return serde_json::from_slice::<JudgeResponse>(&body)
                .map(|response| response.judgment)
                .map_err(|error| JudgeFailure {
                    kind: "unexpected",
                    status: Some(status),
                    reason: format!("judge response did not match the schema: {error}"),
                    api_error: None,
                });
        }
        Err(judge_failure(status, &body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_codes_map_to_kinds() {
        let cases = [
            (422, "rejected"),
            (500, "internal"),
            (502, "provider"),
            (503, "busy"),
            (504, "provider"),
            (529, "provider"),
            (404, "internal"),
            (302, "unexpected"),
        ];
        for (status, kind) in cases {
            let failure = judge_failure(status, b"");
            assert_eq!(failure.kind, kind, "status {status}");
            assert_eq!(failure.status, Some(status));
            assert_eq!(failure.reason, format!("judge returned HTTP {status}"));
            assert!(failure.api_error.is_none());
        }
    }

    #[test]
    fn unparseable_body_is_ignored_and_status_decides() {
        let failure = judge_failure(503, b"<html>upstream</html>");
        assert_eq!(failure.kind, "busy");
        assert_eq!(failure.reason, "judge returned HTTP 503");
    }

    #[test]
    fn judge_error_body_takes_precedence() {
        let body = serde_json::json!({
            "kind": "provider",
            "reason": "upstream failed",
            "api_error": {"status": 429}
        });
        let failure = judge_failure(502, body.to_string().as_bytes());
        assert_eq!(failure.kind, "provider");
        assert_eq!(failure.status, Some(502));
        assert_eq!(failure.reason, "upstream failed");
        assert_eq!(failure.api_error, Some(serde_json::json!({"status": 429})));
    }
}
