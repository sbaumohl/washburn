use std::{collections::HashSet, fs, net::IpAddr, path::Path};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Serialize, Deserialize)]
pub struct RejectedTarget {
    pub row: usize,
    pub value: String,
    pub error: String,
}

#[derive(Debug, Default)]
pub struct Targets {
    pub urls: Vec<Url>,
    pub rejected: Vec<RejectedTarget>,
}

pub fn normalize(value: &str) -> Result<Url> {
    let value = value.trim();
    ensure!(!value.is_empty(), "empty target");
    let candidate = if value.contains("://") {
        value.to_owned()
    } else if let Ok(ip) = value.parse::<IpAddr>() {
        match ip {
            IpAddr::V6(_) => format!("https://[{ip}]/"),
            IpAddr::V4(_) => format!("https://{ip}/"),
        }
    } else {
        format!("https://{value}")
    };
    let mut url = Url::parse(&candidate).context("invalid URL")?;
    ensure!(
        matches!(url.scheme(), "http" | "https"),
        "only HTTP(S) targets are supported"
    );
    ensure!(url.host().is_some(), "target has no host");
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "credentials in URLs are not supported"
    );
    if !value.contains("://") {
        ensure!(
            url.path() == "/" || !url.host_str().unwrap_or_default().parse::<IpAddr>().is_ok(),
            "IP ranges/CIDRs are not supported; provide individual IPs"
        );
        ensure!(
            !url.host_str()
                .unwrap_or_default()
                .chars()
                .any(char::is_whitespace),
            "invalid host"
        );
    }
    url.set_fragment(None);
    Ok(url)
}

fn values(cell: &str) -> Result<Vec<String>> {
    let cell = cell.trim();
    if cell.starts_with('[') && !cell.starts_with("[::") {
        // A bare bracketed IPv6 literal is a target, not a JSON array.
        if cell
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .is_some_and(|s| s.parse::<std::net::Ipv6Addr>().is_ok())
        {
            return Ok(vec![cell.to_owned()]);
        }
        return serde_json::from_str(cell).context("expected a JSON array of targets");
    }
    Ok(cell
        .split([';', '\n', '\r'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect())
}

pub fn load(path: &Path, column: Option<&str>) -> Result<Targets> {
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let content = content.trim_start_matches('\u{feff}');
    let mut targets = Targets::default();
    let mut seen = HashSet::new();
    let mut add = |row: usize, cell: &str| {
        let list = match values(cell) {
            Ok(list) => list,
            Err(error) => {
                targets.rejected.push(RejectedTarget {
                    row,
                    value: cell.to_owned(),
                    error: error.to_string(),
                });
                return;
            }
        };
        for value in list {
            match normalize(&value) {
                Ok(url) if seen.insert(url.to_string()) => targets.urls.push(url),
                Ok(_) => {}
                Err(error) => targets.rejected.push(RejectedTarget {
                    row,
                    value,
                    error: error.to_string(),
                }),
            }
        }
    };
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("csv"))
    {
        let mut reader = csv::Reader::from_reader(content.as_bytes());
        let headers = reader.headers()?.clone();
        let selected = column.map(|c| vec![c]).unwrap_or_else(|| {
            vec![
                "target",
                "url",
                "domain",
                "ip",
                "target_domain",
                "primary_urls",
            ]
        });
        let indices: Vec<_> = headers
            .iter()
            .enumerate()
            .filter(|(_, h)| selected.iter().any(|c| h.trim().eq_ignore_ascii_case(c)))
            .map(|(i, _)| i)
            .collect();
        if indices.is_empty() {
            bail!("no target column found; choose one with --column");
        }
        for (row, record) in reader.records().enumerate() {
            let record = record.with_context(|| format!("CSV row {}", row + 2))?;
            for i in &indices {
                add(row + 2, record.get(*i).unwrap_or_default());
            }
        }
    } else {
        ensure!(column.is_none(), "--column requires a CSV input");
        for (row, line) in content.lines().enumerate() {
            let line = line.trim();
            if !line.is_empty() && !line.starts_with('#') {
                add(row + 1, line);
            }
        }
    }
    ensure!(!targets.urls.is_empty(), "input contains no valid targets");
    Ok(targets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_and_rejects_unsupported_targets() {
        assert_eq!(
            normalize("EXAMPLE.com#x").unwrap().as_str(),
            "https://example.com/"
        );
        assert_eq!(
            normalize("2001:db8::1").unwrap().as_str(),
            "https://[2001:db8::1]/"
        );
        assert_eq!(
            normalize("127.0.0.1:8080").unwrap().as_str(),
            "https://127.0.0.1:8080/"
        );
        for value in [
            "ftp://example.com",
            "https://a:b@example.com",
            "10.0.0.0/24",
            "bad host",
        ] {
            assert!(normalize(value).is_err(), "{value}");
        }
    }

    #[test]
    fn reads_bom_csv_arrays_dedup_and_rejections() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("targets.csv");
        fs::write(&file, "\u{feff}target_domain,primary_urls\n\"[\"\"example.com\"\"]\",https://example.com/#x\nftp://bad,https://other.test/path\n").unwrap();
        let targets = load(&file, None).unwrap();
        assert_eq!(targets.urls.len(), 2);
        assert_eq!(targets.rejected.len(), 1);
        assert_eq!(targets.rejected[0].row, 3);
    }
}
