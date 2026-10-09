//! Canonical URL form shared with the Python seed script and admin UI.
//!
//! The canonical form is the `url` crate's serialization with the fragment
//! removed. `queue.url` and `queue.url_hash` are always computed from it.

use anyhow::{Result, anyhow, bail};
use url::Url;

/// Parses `input` as an absolute http(s) URL, drops the fragment, and returns
/// the `url` crate's normalized form. Bare hosts without a scheme are rejected.
pub fn canonical_url(input: &str) -> Result<Url> {
    let mut url = Url::parse(input.trim()).map_err(|error| anyhow!("invalid URL: {error}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("only http and https URLs are supported");
    }
    if url.host_str().is_none_or(str::is_empty) {
        bail!("URL has no host");
    }
    url.set_fragment(None);
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cross_language_vectors() {
        let cases = [
            ("HTTP://Example.COM", Some("http://example.com/")),
            ("https://example.com:443/a#frag", Some("https://example.com/a")),
            ("https://example.com:8443/x?q=1", Some("https://example.com:8443/x?q=1")),
            ("example.com", None),
            ("ftp://example.com/", None),
            ("mailto:someone@example.com", None),
        ];
        for (input, expected) in cases {
            match (canonical_url(input), expected) {
                (Ok(url), Some(want)) => assert_eq!(url.as_str(), want, "input {input}"),
                (Err(_), None) => {}
                (got, want) => panic!("input {input}: got {got:?}, want {want:?}"),
            }
        }
    }
}
