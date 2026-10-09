//! Loads the single `washburn.toml` and maps it onto the crawler's settings.
//!
//! Every struct is `deny_unknown_fields` and mirrors `washburn.example.toml`.
//! Secrets are `cn_swarms::config::Secret` so Debug output is always redacted.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use cn_swarms::config::{
    Config as CrawlerConfig, OpenRouterSettings, Secret, Services, TypeSafeSettings,
};
use serde::{Deserialize, Serialize};
use toml::{Table, Value};

pub const CONFIG_ENV: &str = "WASHBURN_CONFIG";
pub const DEFAULT_CONFIG_PATH: &str = "/etc/washburn/washburn.toml";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WashburnConfig {
    // Required on every VM role.
    pub typesafe: TypeSafeSettings,
    pub openrouter: OpenRouterSettings,
    pub postgres: PostgresConfig,
    pub crawl: CrawlConfig,
    pub judge: JudgeConfig,
    // Optional: each VM receives only the tables it needs (see accessors).
    pub domain: Option<DomainConfig>,
    pub vultr: Option<VultrConfig>,
    pub hetzner: Option<HetznerConfig>,
    pub tailscale: Option<TailscaleConfig>,
    pub web: Option<WebConfig>,
    pub images: Option<ImagesConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImagesConfig {
    pub registry: String,
    pub tag: String,
    /// May be empty.
    pub ghcr_user: String,
    /// May be empty. When both user and token are set, deploy.sh logs in on each VM.
    pub ghcr_token: Secret,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DomainConfig {
    pub root: String,
    pub public_host: String,
    pub admin_host: String,
    pub acme_email: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VultrConfig {
    pub api_key: Secret,
    pub region: String,
    pub ssh_key_ids: Vec<String>,
    pub os_id: u32,
    pub plans: VultrPlans,
    pub counts: VultrCounts,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VultrPlans {
    pub crawler: String,
    pub data: String,
    pub web: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VultrCounts {
    pub crawler: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostgresConfig {
    pub host: String,
    pub db: String,
    pub user: String,
    pub password: Secret,
    pub port: u16,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HetznerConfig {
    pub storage_box_host: String,
    pub storage_box_user: String,
    pub storage_box_port: u16,
    pub repo_path: String,
    pub borg_passphrase: Secret,
    pub schedule: String,
    pub keep_daily: u32,
    pub keep_weekly: u32,
    pub keep_monthly: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TailscaleConfig {
    pub auth_key: Secret,
    pub tailnet: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CrawlConfig {
    pub user_agent: String,
    pub workers: usize,
    pub concurrency: usize,
    pub classifier_concurrency: usize,
    pub delay_ms: u64,
    pub timeout_secs: u64,
    pub retries: usize,
    pub max_body_bytes: usize,
    pub chunk_chars: usize,
    pub max_redirects: usize,
    pub max_depth: usize,
    pub importance_threshold: f64,
    pub report_threshold: f64,
    pub ignore_robots: bool,
    pub host_lease_secs: u64,
    pub heartbeat_secs: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeConfig {
    pub bind: String,
    pub url: String,
    pub concurrency: usize,
    pub timeout_secs: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    pub admin_user: String,
    pub admin_password: Secret,
    pub public_bind: String,
    pub admin_bind: String,
}

/// Serialization-only view of `[typesafe]` / `[openrouter]`, so the crawler's
/// own `Services` validation runs unchanged. `Secret` is not `Serialize`, so
/// the raw key is borrowed here and never leaves this function's TOML string.
#[derive(Serialize)]
struct ModelSettings<'a> {
    api_key: &'a str,
    model: &'a str,
    base_url: &'a str,
}

#[derive(Serialize)]
struct ModelServices<'a> {
    typesafe: ModelSettings<'a>,
    openrouter: ModelSettings<'a>,
}

fn required<'a, T>(table: &'a Option<T>, name: &str) -> Result<&'a T> {
    table.as_ref().ok_or_else(|| {
        anyhow!("washburn.toml has no [{name}] table; this role's config does not include it")
    })
}

impl WashburnConfig {
    pub fn domain(&self) -> Result<&DomainConfig> {
        required(&self.domain, "domain")
    }
    pub fn vultr(&self) -> Result<&VultrConfig> {
        required(&self.vultr, "vultr")
    }
    pub fn hetzner(&self) -> Result<&HetznerConfig> {
        required(&self.hetzner, "hetzner")
    }
    pub fn tailscale(&self) -> Result<&TailscaleConfig> {
        required(&self.tailscale, "tailscale")
    }
    pub fn web(&self) -> Result<&WebConfig> {
        required(&self.web, "web")
    }
    pub fn images(&self) -> Result<&ImagesConfig> {
        required(&self.images, "images")
    }

    /// Reads and validates the file at `path`. Error messages never include values.
    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("read washburn config at {}", path.display()))?;
        Self::parse(&content)
    }

    /// Parses TOML text with the same checks as `load`.
    pub fn parse(content: &str) -> Result<Self> {
        let table: Table = toml::from_str(content).map_err(|error: toml::de::Error| {
            anyhow!(
                "washburn.toml is not valid TOML{}",
                line_suffix(content, &error)
            )
        })?;
        let mut placeholders = Vec::new();
        collect_placeholders("", &Value::Table(table), &mut placeholders);
        placeholders.dedup();
        if !placeholders.is_empty() {
            bail!(
                "washburn.toml still has placeholder values (starting with REPLACE) in: {}; fill them in (values are not printed)",
                placeholders.join(", ")
            );
        }
        toml::from_str(content).map_err(|error: toml::de::Error| {
            anyhow!(
                "invalid washburn.toml{}; check field names and types against washburn.example.toml",
                line_suffix(content, &error)
            )
        })
    }

    /// Crawler `Services` built from `[typesafe]` and `[openrouter]` only.
    pub fn services(&self) -> Result<Services> {
        let export = ModelServices {
            typesafe: ModelSettings {
                api_key: self.typesafe.api_key.expose(),
                model: &self.typesafe.model,
                base_url: &self.typesafe.base_url,
            },
            openrouter: ModelSettings {
                api_key: self.openrouter.api_key.expose(),
                model: &self.openrouter.model,
                base_url: &self.openrouter.base_url,
            },
        };
        let toml = toml::to_string(&export).context("serialize model settings")?;
        Services::from_toml_str(&toml)
    }

    /// Crawler `Config` for the sweeper, from `[crawl]` and `[judge]`.
    /// `config` is `None` and `max_pages` keeps its default: the daemon has no
    /// run-level budget. Call `services()` for the model settings.
    pub fn crawler_config(&self) -> CrawlerConfig {
        let crawl = &self.crawl;
        let mut config = CrawlerConfig::service_defaults();
        config.user_agent = crawl.user_agent.clone();
        config.concurrency = crawl.concurrency;
        config.classifier_concurrency = crawl.classifier_concurrency;
        config.judge_concurrency = self.judge.concurrency;
        config.delay_ms = crawl.delay_ms;
        config.timeout_secs = crawl.timeout_secs;
        config.judge_timeout_secs = self.judge.timeout_secs;
        config.retries = crawl.retries;
        config.max_body_bytes = crawl.max_body_bytes;
        config.chunk_chars = crawl.chunk_chars;
        config.max_redirects = crawl.max_redirects;
        config.max_depth = crawl.max_depth;
        config.importance_threshold = crawl.importance_threshold;
        config.report_threshold = crawl.report_threshold;
        config.ignore_robots = crawl.ignore_robots;
        config
    }
}

/// `WASHBURN_CONFIG` (if non-empty), then `arg`, then `/etc/washburn/washburn.toml`.
pub fn config_path_from_env_or_arg(arg: Option<PathBuf>) -> PathBuf {
    std::env::var_os(CONFIG_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or(arg)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH))
}

fn collect_placeholders(prefix: &str, value: &Value, found: &mut Vec<String>) {
    match value {
        Value::String(text) if text.starts_with("REPLACE") => found.push(prefix.to_owned()),
        Value::Array(items) => {
            for item in items {
                collect_placeholders(prefix, item, found);
            }
        }
        Value::Table(table) => {
            for (key, child) in table {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                collect_placeholders(&path, child, found);
            }
        }
        _ => {}
    }
}

fn line_suffix(content: &str, error: &toml::de::Error) -> String {
    error
        .span()
        .map(|span| {
            let line = content[..span.start.min(content.len())]
                .matches('\n')
                .count()
                + 1;
            format!(" near line {line}")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../../../washburn.example.toml");

    /// Replaces each uncommented `REPLACE...` value with a distinctive secret
    /// (`SECRET-<table>-<key>-VALUE`) so the Debug assertion is meaningful.
    fn substitute(example: &str) -> String {
        let mut section = String::new();
        let mut out = Vec::new();
        for line in example.lines() {
            let trimmed = line.trim_start();
            if let Some(name) = trimmed.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
                section = name.to_owned();
            }
            if trimmed.starts_with('#') || !line.contains("REPLACE") {
                out.push(line.to_owned());
                continue;
            }
            let key = line.split('=').next().unwrap_or_default().trim();
            let value = match key {
                "ssh_key_ids" => "[\"ssh-key-id-1\"]".to_owned(),
                "tailnet" => "\"tail-test.ts.net\"".to_owned(),
                _ => format!("\"SECRET-{section}-{key}-VALUE\""),
            };
            out.push(format!("{} = {}", key, value));
        }
        out.join("\n")
    }

    #[test]
    fn example_with_placeholders_is_rejected_without_echoing_values() {
        let error = format!("{:#}", WashburnConfig::parse(EXAMPLE).unwrap_err());
        assert!(error.contains("postgres.password"), "{error}");
        assert!(error.contains("vultr.ssh_key_ids"), "{error}");
        assert!(!error.contains("REPLACE-long-random"), "{error}");
        assert!(!error.contains("REPLACE.ts.net"), "{error}");
    }

    #[test]
    fn substituted_example_loads_and_redacts_secrets() {
        let config = WashburnConfig::parse(&substitute(EXAMPLE)).unwrap();
        let services = config.services().unwrap();
        assert_eq!(services.typesafe.model, "jev-latest");
        assert_eq!(services.openrouter.model, "provider/strong-review-model");

        let debug = format!("{config:?}{services:?}");
        assert!(
            !debug.contains("SECRET-"),
            "secret leaked into Debug output"
        );
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("SECRET-openrouter-api_key-VALUE"));
    }

    #[test]
    fn crawler_role_subset_loads_without_optional_tables() {
        let full: Table = toml::from_str(&substitute(EXAMPLE)).unwrap();
        let keep = ["typesafe", "openrouter", "postgres", "crawl", "judge"];
        let subset: Table = full
            .into_iter()
            .filter(|(key, _)| keep.contains(&key.as_str()))
            .collect();
        let text = toml::to_string(&subset).unwrap();
        let config = WashburnConfig::parse(&text).unwrap();
        assert!(config.vultr.is_none() && config.images.is_none());
        assert!(config.services().is_ok());
        let error = format!("{:#}", config.vultr().unwrap_err());
        assert!(error.contains("[vultr]"), "{error}");
    }

    #[test]
    fn full_example_exposes_optional_tables() {
        let config = WashburnConfig::parse(&substitute(EXAMPLE)).unwrap();
        assert_eq!(config.vultr().unwrap().os_id, 2284);
        assert_eq!(config.images().unwrap().tag, "latest");
        assert!(config.images().unwrap().ghcr_user.is_empty());
    }

    #[test]
    fn crawler_config_maps_crawl_and_judge_tables() {
        let config = WashburnConfig::parse(&substitute(EXAMPLE)).unwrap();
        let crawler = config.crawler_config();
        assert_eq!(crawler.concurrency, 1);
        assert_eq!(crawler.classifier_concurrency, 4);
        assert_eq!(crawler.judge_concurrency, 2);
        assert_eq!(crawler.judge_timeout_secs, 120);
        assert_eq!(crawler.max_depth, 2);
        assert_eq!(crawler.chunk_chars, 12_000);
        assert!((crawler.report_threshold - 0.80).abs() < f64::EPSILON);
        assert!(crawler.config.is_none());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let injected = substitute(EXAMPLE).replace("[crawl]\n", "[crawl]\nbogus_field = 1\n");
        let error = format!("{:#}", WashburnConfig::parse(&injected).unwrap_err());
        assert!(error.contains("near line"), "{error}");
        assert!(!error.contains("bogus_field"), "{error}");
    }

    #[test]
    fn config_path_prefers_env_then_arg_then_default() {
        // Only the default branch is asserted: the env var is process-global.
        if std::env::var_os(CONFIG_ENV).is_none() {
            assert_eq!(
                config_path_from_env_or_arg(None),
                PathBuf::from(DEFAULT_CONFIG_PATH)
            );
            assert_eq!(
                config_path_from_env_or_arg(Some(PathBuf::from("/x/a.toml"))),
                PathBuf::from("/x/a.toml")
            );
        }
    }
}
