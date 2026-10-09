use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, ensure};
use clap::Parser;
use serde::Deserialize;

#[derive(Clone, Debug, Parser)]
#[command(
    version,
    about = "Crawl web targets and triage evidence of agent activity"
)]
pub struct Config {
    /// TOML file containing TypeSafe/OpenRouter keys and model settings
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// UTF-8 text (one target per line), or CSV with a target column
    #[arg(short, long)]
    pub input: PathBuf,
    /// CSV column to ingest; defaults to target, url, domain, ip, target_domain, primary_urls
    #[arg(long)]
    pub column: Option<String>,
    /// Parent directory; each invocation creates an isolated run directory
    #[arg(short, long, default_value = "runs")]
    pub output: PathBuf,
    #[arg(long, default_value_t = 8)]
    pub concurrency: usize,
    #[arg(long, default_value_t = 2)]
    pub judge_concurrency: usize,
    #[arg(long, default_value_t = 8)]
    pub classifier_concurrency: usize,
    /// Global URL budget, including seeds, failures, and skipped URLs
    #[arg(long, default_value_t = 100)]
    pub max_pages: usize,
    #[arg(long, default_value_t = 2)]
    pub max_depth: usize,
    /// Minimum interval between requests to the same origin
    #[arg(long, default_value_t = 1000)]
    pub delay_ms: u64,
    #[arg(long, default_value_t = 20)]
    pub timeout_secs: u64,
    #[arg(long, default_value_t = 120)]
    pub judge_timeout_secs: u64,
    /// Retries for transport failures, 429, and 5xx responses
    #[arg(long, default_value_t = 2)]
    pub retries: usize,
    #[arg(long, default_value_t = 2_000_000)]
    pub max_body_bytes: usize,
    /// All extracted text is judged in chunks of this many characters
    #[arg(long, default_value_t = 12_000)]
    pub chunk_chars: usize,
    #[arg(long, default_value_t = 5)]
    pub max_redirects: usize,
    #[arg(long, default_value = "cn-swarms/0.1")]
    pub user_agent: String,
    /// Deliberately ignore robots.txt
    #[arg(long)]
    pub ignore_robots: bool,
    /// Fetch and archive without LLM calls; pages are marked unjudged
    #[arg(long)]
    pub crawl_only: bool,
    /// Jev probability of importance required for a level-one finding and review
    #[arg(long, default_value_t = 0.7)]
    pub importance_threshold: f64,
    /// Minimum activity, seriousness, and confidence for a confirmed serious report
    #[arg(long, default_value_t = 0.8)]
    pub report_threshold: f64,
    /// Additional investigation criteria, added to the judges' shared rubric
    #[arg(long)]
    pub rubric: Option<PathBuf>,
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.concurrency > 0 && self.judge_concurrency > 0 && self.classifier_concurrency > 0,
            "concurrency must be positive"
        );
        ensure!(
            self.max_pages > 0 && self.max_body_bytes > 0 && self.chunk_chars > 0,
            "page, body, and chunk limits must be positive"
        );
        ensure!(
            self.timeout_secs > 0 && self.judge_timeout_secs > 0,
            "timeouts must be positive"
        );
        ensure!(
            !self.user_agent.trim().is_empty(),
            "user agent must not be empty"
        );
        for threshold in [self.importance_threshold, self.report_threshold] {
            ensure!(
                (0.0..=1.0).contains(&threshold),
                "thresholds must be between 0 and 1"
            );
        }
        if !self.crawl_only {
            ensure!(
                self.config.is_some(),
                "select a TOML settings file with --config (or use --crawl-only)"
            );
        }
        Ok(())
    }
}

impl Config {
    /// A `Config` with every clap default applied and no argv parsing, for
    /// daemons that take their settings from elsewhere. `input` is a placeholder.
    pub fn service_defaults() -> Self {
        Self {
            config: None,
            input: PathBuf::from("/dev/null"),
            column: None,
            output: PathBuf::from("runs"),
            concurrency: 8,
            judge_concurrency: 2,
            classifier_concurrency: 8,
            max_pages: 100,
            max_depth: 2,
            delay_ms: 1000,
            timeout_secs: 20,
            judge_timeout_secs: 120,
            retries: 2,
            max_body_bytes: 2_000_000,
            chunk_chars: 12_000,
            max_redirects: 5,
            user_agent: "cn-swarms/0.1".into(),
            ignore_robots: false,
            crawl_only: false,
            importance_threshold: 0.7,
            report_threshold: 0.8,
            rubric: None,
        }
    }
}

/// Keys are never serialized, and Debug output is always redacted.
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Services {
    pub typesafe: TypeSafeSettings,
    pub openrouter: OpenRouterSettings,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeSafeSettings {
    pub api_key: Secret,
    #[serde(default = "jev_model")]
    pub model: String,
    #[serde(default = "typesafe_url")]
    pub base_url: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRouterSettings {
    pub api_key: Secret,
    pub model: String,
    #[serde(default = "openrouter_url")]
    pub base_url: String,
}

fn jev_model() -> String {
    "jev-latest".into()
}
fn typesafe_url() -> String {
    "https://api.typesafe.ai/v1".into()
}
fn openrouter_url() -> String {
    "https://openrouter.ai/api/v1".into()
}

impl Services {
    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path).context("read TOML settings file")?;
        Self::from_toml_str(&content)
    }

    pub fn from_toml_str(content: &str) -> Result<Self> {
        // TOML's normal Display error includes a source excerpt, which could contain keys.
        let settings: Self = toml::from_str(content).map_err(|error: toml::de::Error| {
            let line = error.span().map(|s| content[..s.start].bytes().filter(|b| *b == b'\n').count() + 1);
            anyhow!("invalid TOML settings{}; check syntax, required fields, and field types against config.example.toml", line.map(|n| format!(" near line {n}")).unwrap_or_default())
        })?;
        for (service, key, model, base_url) in [
            (
                "typesafe",
                &settings.typesafe.api_key,
                &settings.typesafe.model,
                &settings.typesafe.base_url,
            ),
            (
                "openrouter",
                &settings.openrouter.api_key,
                &settings.openrouter.model,
                &settings.openrouter.base_url,
            ),
        ] {
            ensure!(
                !key.expose().trim().is_empty(),
                "{service}.api_key must not be empty"
            );
            ensure!(
                reqwest::header::HeaderValue::from_str(&format!("Bearer {}", key.expose())).is_ok(),
                "{service}.api_key contains invalid header characters"
            );
            ensure!(
                !model.trim().is_empty(),
                "{service}.model must not be empty"
            );
            let url =
                url::Url::parse(base_url).map_err(|_| anyhow!("{service}.base_url is invalid"))?;
            ensure!(
                matches!(url.scheme(), "http" | "https")
                    && url.host().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none(),
                "{service}.base_url must be an absolute HTTP(S) URL without credentials, query, or fragment"
            );
        }
        Ok(settings)
    }

    pub fn keys(&self) -> Vec<Secret> {
        vec![
            self.typesafe.api_key.clone(),
            self.openrouter.api_key.clone(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_models_and_redacts_debug() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[typesafe]\napi_key = 'secret-typesafe'\n[openrouter]\napi_key = 'secret-openrouter'\nmodel = 'provider/reviewer'\n").unwrap();
        let settings = Services::load(&path).unwrap();
        assert_eq!(settings.typesafe.model, "jev-latest");
        assert_eq!(settings.openrouter.model, "provider/reviewer");
        let debug = format!("{settings:?}");
        assert!(!debug.contains("secret-typesafe"));
        assert!(!debug.contains("secret-openrouter"));
    }

    #[test]
    fn service_defaults_match_clap_defaults() {
        let parsed = Config::try_parse_from(["cn-swarms", "--input", "/dev/null"]).unwrap();
        assert_eq!(
            format!("{:?}", Config::service_defaults()),
            format!("{parsed:?}")
        );
    }

    #[test]
    fn parser_errors_do_not_echo_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for input in [
            "[typesafe]\napi_key = 'secret-do-not-print' trailing",
            "[typesafe]\napi_key='secret-do-not-print'\n[openrouter]\napi_key='secret-two'\nmodel=7",
        ] {
            fs::write(&path, input).unwrap();
            let error = format!("{:#}", Services::load(&path).unwrap_err());
            assert!(!error.contains("secret-do-not-print"));
            assert!(!error.contains("secret-two"));
        }
    }
}
