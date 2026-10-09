//! Wire types for the judge HTTP service (`crates/judge`) and its callers.

use cn_swarms::{
    api::ApiError,
    jev::Screening,
    judge::{Judgment, PageMeta, RejectedResponse},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

/// Body of `POST /v1/judge`. Carries exactly what `Judge::evaluate_text` reads.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JudgeRequest {
    pub final_url: Url,
    pub fetched_at: String,
    pub status: u16,
    pub title: String,
    pub text: String,
    pub chunk: usize,
    pub screening: Screening,
}

impl JudgeRequest {
    /// Page metadata in the form the crawler's judge consumes.
    pub fn meta(&self) -> PageMeta {
        PageMeta {
            final_url: self.final_url.clone(),
            fetched_at: self.fetched_at.clone(),
            status: self.status,
            title: self.title.clone(),
        }
    }
}

/// Body of a successful judge response (`200`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JudgeResponse {
    pub judgment: Judgment,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JudgeErrorKind {
    /// Model output did not match the verdict schema (`422`).
    Rejected,
    /// Upstream provider returned an error (`502`).
    Provider,
    /// Concurrency cap was full for longer than the timeout (`503`).
    Busy,
    /// Anything else (`500`).
    Internal,
}

/// Error body for non-200 judge responses.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct JudgeError {
    pub kind: JudgeErrorKind,
    pub reason: String,
    #[serde(default)]
    pub response: Option<Value>,
    #[serde(default)]
    pub api_error: Option<Value>,
}

impl JudgeError {
    /// Classifies an error returned by `Judge::evaluate` / `evaluate_text`.
    pub fn from_judge_error(error: &anyhow::Error) -> Self {
        let reason = format!("{error:#}");
        if let Some(rejected) = error.downcast_ref::<RejectedResponse>() {
            Self {
                kind: JudgeErrorKind::Rejected,
                reason,
                response: Some(rejected.response.clone()),
                api_error: None,
            }
        } else if let Some(api) = error.downcast_ref::<ApiError>() {
            Self {
                kind: JudgeErrorKind::Provider,
                reason,
                response: None,
                api_error: serde_json::to_value(api).ok(),
            }
        } else {
            Self {
                kind: JudgeErrorKind::Internal,
                reason,
                response: None,
                api_error: None,
            }
        }
    }

    pub fn busy(reason: impl Into<String>) -> Self {
        Self {
            kind: JudgeErrorKind::Busy,
            reason: reason.into(),
            response: None,
            api_error: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cn_swarms::judge::{Classification, Verdict};

    fn sample_request() -> JudgeRequest {
        JudgeRequest {
            final_url: Url::parse("https://example.test/a").unwrap(),
            fetched_at: "2026-10-07T00:00:00+00:00".into(),
            status: 200,
            title: "Trace".into(),
            text: "agent ran rm -rf".into(),
            chunk: 1,
            screening: Screening {
                chunk: 1,
                requested_model: "jev-latest".into(),
                model: "jev-latest".into(),
                usage: Value::Null,
                importance: 0.9,
                agent_activity: 0.8,
                potentially_serious: 0.1,
            },
        }
    }

    #[test]
    fn request_roundtrips_through_json() {
        let request = sample_request();
        let json = serde_json::to_string(&request).unwrap();
        let back: JudgeRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(
            serde_json::to_value(&back).unwrap(),
            serde_json::to_value(&request).unwrap()
        );
        assert_eq!(back.meta().title, "Trace");
    }

    #[test]
    fn response_and_error_roundtrip() {
        let judgment = Judgment {
            stage: "review".into(),
            chunk: 1,
            requested_model: "m".into(),
            model: "m".into(),
            usage: Value::Null,
            verdict: Verdict {
                activity: 0.5,
                seriousness: 0.0,
                confidence: 0.5,
                classification: Classification::Unclear,
                summary: "s".into(),
                evidence: vec![],
                uncertainties: vec!["none".into()],
                confirmed_serious: false,
            },
        };
        let response = JudgeResponse { judgment };
        let json = serde_json::to_string(&response).unwrap();
        let back: JudgeResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.judgment.verdict.classification,
            Classification::Unclear
        );

        let error = JudgeError::busy("queue full");
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(json["kind"], "busy");
        assert!(json["response"].is_null());
        let back: JudgeError = serde_json::from_value(json).unwrap();
        assert_eq!(back, error);

        let internal = JudgeError::from_judge_error(&anyhow::anyhow!("boom"));
        assert_eq!(internal.kind, JudgeErrorKind::Internal);
    }

    #[test]
    fn rejected_responses_keep_model_output() {
        let err: anyhow::Error = RejectedResponse {
            reason: "bad schema".into(),
            response: serde_json::json!({"choices": []}),
        }
        .into();
        let classified = JudgeError::from_judge_error(&err);
        assert_eq!(classified.kind, JudgeErrorKind::Rejected);
        assert_eq!(
            classified.response,
            Some(serde_json::json!({"choices": []}))
        );
    }
}
