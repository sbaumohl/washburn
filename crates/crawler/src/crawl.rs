use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, StatusCode, header};
use scraper::{Html, Node, Selector};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{Mutex, OnceCell},
    time::{Instant, sleep},
};
use url::Url;

use crate::{config::Config, source::normalize};

pub const ROBOT_AGENT: &str = "cn-swarms";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub url: Url,
    pub seed: Url,
    pub parent: Option<Url>,
    pub depth: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    pub job: Job,
    pub final_url: Url,
    pub redirects: Vec<Url>,
    pub fetched_at: String,
    pub status: u16,
    pub content_type: String,
    pub title: String,
    pub text: String,
    pub links: Vec<Url>,
    #[serde(skip)]
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub enum Fetch {
    Page(Box<Page>),
    Skipped(String),
}

#[derive(Default)]
struct OriginState {
    last_start: Mutex<Option<Instant>>,
    robots: OnceCell<std::result::Result<String, String>>,
}

#[derive(Clone)]
pub struct Crawler {
    client: Client,
    config: Arc<Config>,
    scope: Arc<HashSet<(String, Option<u16>)>>,
    origins: Arc<Mutex<HashMap<String, Arc<OriginState>>>>,
}

impl Crawler {
    pub fn new(config: Arc<Config>, seeds: &[Url]) -> Result<Self> {
        let client = Client::builder()
            .user_agent(&config.user_agent)
            .timeout(Duration::from_secs(config.timeout_secs))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            client,
            config,
            scope: Arc::new(
                seeds
                    .iter()
                    .map(|u| (u.host_str().unwrap_or_default().to_owned(), u.port()))
                    .collect(),
            ),
            origins: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn in_scope(&self, url: &Url) -> bool {
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && self
                .scope
                .contains(&(url.host_str().unwrap_or_default().to_owned(), url.port()))
    }

    async fn state(&self, url: &Url) -> Arc<OriginState> {
        self.origins
            .lock()
            .await
            .entry(url.origin().ascii_serialization())
            .or_default()
            .clone()
    }

    async fn pace(&self, url: &Url) {
        let state = self.state(url).await;
        let mut last = state.last_start.lock().await;
        if let Some(start) = *last {
            sleep(
                (start + Duration::from_millis(self.config.delay_ms))
                    .saturating_duration_since(Instant::now()),
            )
            .await;
        }
        *last = Some(Instant::now());
    }

    async fn get(&self, url: &Url) -> Result<reqwest::Response> {
        for attempt in 0..=self.config.retries {
            self.pace(url).await;
            let result = self.client.get(url.clone()).send().await;
            let retry = match &result {
                Ok(r) => {
                    r.status() == StatusCode::TOO_MANY_REQUESTS || r.status().is_server_error()
                }
                Err(_) => true,
            };
            if !retry || attempt == self.config.retries {
                return result.context("HTTP request failed");
            }
            let retry_after = result
                .as_ref()
                .ok()
                .and_then(|r| r.headers().get(header::RETRY_AFTER))
                .and_then(|h| h.to_str().ok())
                .and_then(|h| h.parse::<u64>().ok());
            let wait = retry_after.unwrap_or(1_u64 << attempt.min(6)).min(60);
            sleep(Duration::from_secs(wait)).await;
        }
        unreachable!()
    }

    async fn body(&self, mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
        if response
            .content_length()
            .is_some_and(|len| len > limit as u64)
        {
            bail!("response exceeds {limit} byte limit");
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.context("read response body")? {
            ensure!(
                chunk.len() <= limit.saturating_sub(body.len()),
                "response exceeds {limit} byte limit"
            );
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    async fn allowed(&self, url: &Url) -> Result<bool> {
        if self.config.ignore_robots {
            return Ok(true);
        }
        let state = self.state(url).await;
        let robots = state
            .robots
            .get_or_init(|| async {
                let result: Result<String> = async {
                    let mut robots_url = url.clone();
                    robots_url.set_path("/robots.txt");
                    robots_url.set_query(None);
                    robots_url.set_fragment(None);
                    let response = self.get(&robots_url).await?;
                    if response.status().is_client_error()
                        && response.status() != StatusCode::TOO_MANY_REQUESTS
                    {
                        return Ok(String::new());
                    }
                    // Fail visibly if policy cannot be obtained; never follow robots redirects blindly.
                    ensure!(
                        response.status().is_success(),
                        "robots.txt returned {}",
                        response.status()
                    );
                    let bytes = self.body(response, 512_000).await?;
                    Ok(String::from_utf8_lossy(&bytes).into_owned())
                }
                .await;
                result.map_err(|e| format!("{e:#}"))
            })
            .await;
        match robots {
            Ok(body) => {
                let agent = self
                    .config
                    .user_agent
                    .split('/')
                    .next()
                    .unwrap_or(ROBOT_AGENT);
                Ok(
                    robotstxt::DefaultMatcher::default().one_agent_allowed_by_robots(
                        body,
                        agent,
                        url.as_str(),
                    ),
                )
            }
            Err(error) => bail!("robots policy unavailable: {error}"),
        }
    }

    pub async fn fetch(&self, job: Job) -> Result<Fetch> {
        let mut current = job.url.clone();
        let mut redirects = Vec::new();
        loop {
            ensure!(self.in_scope(&current), "URL outside seed scope: {current}");
            if !self.allowed(&current).await? {
                return Ok(Fetch::Skipped(format!("robots.txt disallows {current}")));
            }
            let response = self.get(&current).await?;
            if response.status().is_redirection() {
                ensure!(
                    redirects.len() < self.config.max_redirects,
                    "redirect limit reached"
                );
                let location = response
                    .headers()
                    .get(header::LOCATION)
                    .context("redirect has no Location")?
                    .to_str()?;
                let next = normalize(current.join(location)?.as_str())?;
                ensure!(
                    self.in_scope(&next),
                    "redirect outside seed scope: {next}; add its host to input to allow it"
                );
                ensure!(
                    next != job.url && !redirects.contains(&next),
                    "redirect loop"
                );
                redirects.push(next.clone());
                current = next;
                continue;
            }
            let status = response.status().as_u16();
            let content_type = response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            let mime = content_type
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            if !mime.is_empty() && !is_text(&mime) {
                return Ok(Fetch::Skipped(format!(
                    "unsupported content type: {content_type} (HTTP {status})"
                )));
            }
            let encoding = content_type
                .split(';')
                .find_map(|s| s.trim().strip_prefix("charset="))
                .unwrap_or("utf-8")
                .trim_matches('"');
            let body = self.body(response, self.config.max_body_bytes).await?;
            // Explicitly log encodings this first version cannot decode losslessly.
            ensure!(
                encoding.eq_ignore_ascii_case("utf-8")
                    || encoding.eq_ignore_ascii_case("utf8")
                    || encoding.eq_ignore_ascii_case("us-ascii"),
                "unsupported charset: {encoding}"
            );
            let decoded = std::str::from_utf8(&body).context("response is not UTF-8 text")?;
            let html = mime == "text/html"
                || mime == "application/xhtml+xml"
                || (mime.is_empty() && decoded.trim_start().starts_with('<'));
            let (title, text, links) = if html {
                extract_html(decoded, &current)
            } else {
                (String::new(), decoded.to_owned(), Vec::new())
            };
            return Ok(Fetch::Page(Box::new(Page {
                job,
                final_url: current,
                redirects,
                fetched_at: chrono::Utc::now().to_rfc3339(),
                status,
                content_type,
                title,
                text,
                links,
                body,
            })));
        }
    }
}

fn is_text(mime: &str) -> bool {
    mime.starts_with("text/")
        || matches!(
            mime,
            "application/json"
                | "application/xml"
                | "application/xhtml+xml"
                | "application/javascript"
        )
        || mime.ends_with("+json")
        || mime.ends_with("+xml")
}

pub fn extract_html(body: &str, base: &Url) -> (String, String, Vec<Url>) {
    let doc = Html::parse_document(body);
    let title = doc
        .select(&Selector::parse("title").unwrap())
        .next()
        .map(|e| e.text().collect::<Vec<_>>().join(" "))
        .unwrap_or_default();
    let mut text = String::new();
    for node in doc.tree.nodes() {
        if let Node::Text(value) = node.value() {
            if node.ancestors().any(|a| matches!(a.value(), Node::Element(e) if matches!(e.name(), "script" | "style" | "noscript" | "template"))) { continue; }
            let part = value.split_whitespace().collect::<Vec<_>>().join(" ");
            if !part.is_empty() {
                text.push_str(&part);
                text.push('\n');
            }
        }
    }
    let link_base = doc
        .select(&Selector::parse("base[href]").unwrap())
        .next()
        .and_then(|e| e.value().attr("href"))
        .and_then(|h| base.join(h).ok())
        .unwrap_or_else(|| base.clone());
    let mut seen = HashSet::new();
    let links = doc
        .select(&Selector::parse("a[href]").unwrap())
        .filter_map(|a| a.value().attr("href"))
        .filter_map(|href| link_base.join(href).ok())
        .filter_map(|url| normalize(url.as_str()).ok())
        .filter(|url| seen.insert(url.to_string()))
        .collect();
    (title, text, links)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extracts_text_without_scripts_and_resolves_links() {
        let base = Url::parse("https://example.com/start").unwrap();
        let (title, text, links) = extract_html(
            "<title>Trace</title><script>injected()</script><p>Agent &amp; logs</p><a href='/next#x'>next</a><a href='/next'>same</a><a href='javascript:alert(1)'>bad</a>",
            &base,
        );
        assert_eq!(title, "Trace");
        assert!(text.contains("Agent & logs"));
        assert!(!text.contains("injected"));
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].as_str(), "https://example.com/next");
    }
}
