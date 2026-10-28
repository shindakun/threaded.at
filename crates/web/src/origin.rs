//! The site's public origin, such as `https://threaded.at`, for the absolute
//! URLs that feeds and link previews need. It is checked once at startup, so
//! a bad setting stops the server instead of producing broken links.

use std::fmt;

/// A scheme, a host, and an optional port: no path, query, or fragment, and
/// no trailing slash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadOrigin(String);

impl fmt::Display for BadOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} is not an origin: give a scheme and host, such as https://threaded.at, with no path",
            self.0
        )
    }
}

impl std::error::Error for BadOrigin {}

impl Origin {
    pub fn parse(s: &str) -> Result<Origin, BadOrigin> {
        let bad = || BadOrigin(s.to_string());
        let trimmed = s.strip_suffix('/').unwrap_or(s);
        let rest = trimmed
            .strip_prefix("https://")
            .or_else(|| trimmed.strip_prefix("http://"))
            .ok_or_else(bad)?;
        let (host, port) = match rest.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (rest, None),
        };
        let host_ok = !host.is_empty()
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
        let port_ok = port.is_none_or(|p| p.parse::<u16>().is_ok_and(|n| n > 0));
        if !host_ok || !port_ok {
            return Err(bad());
        }
        Ok(Origin(trimmed.to_ascii_lowercase()))
    }

    /// The origin a local build serves on, from its listen address.
    pub fn local(port: u16) -> Origin {
        Origin(format!("http://127.0.0.1:{port}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// An absolute URL for a path on this site. `path` starts with `/`.
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.0)
    }
}

/// Query parameters that only report what just happened, such as `done=1`
/// after a moderator action. A page's shareable URL leaves them out.
const TRANSIENT: &[&str] = &["done", "notice", "duplicate"];

/// The address of a page as someone would share it: its path and query,
/// without transient parameters.
pub fn shareable(path: &str, query: Option<&str>) -> String {
    let kept: Vec<&str> = query
        .unwrap_or("")
        .split('&')
        .filter(|kv| !kv.is_empty())
        .filter(|kv| {
            let key = kv.split('=').next().unwrap_or("");
            !TRANSIENT.contains(&key)
        })
        .collect();
    if kept.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", kept.join("&"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins() {
        for (input, want) in [
            ("https://threaded.at", "https://threaded.at"),
            ("https://threaded.at/", "https://threaded.at"),
            ("https://Threaded.AT", "https://threaded.at"),
            ("http://127.0.0.1:8080", "http://127.0.0.1:8080"),
        ] {
            assert_eq!(Origin::parse(input).unwrap().as_str(), want, "{input}");
        }
        for input in [
            "threaded.at",
            "ftp://threaded.at",
            "https://",
            "https://threaded.at/path",
            "https://threaded.at?x=1",
            "https://threaded.at#x",
            "https://threaded.at:0",
            "https://threaded.at:99999",
            "https://thr eaded.at",
            "https://user@threaded.at",
        ] {
            assert!(Origin::parse(input).is_err(), "{input}");
        }
        assert_eq!(
            Origin::parse("https://threaded.at").unwrap().url("/s/abc"),
            "https://threaded.at/s/abc"
        );
        assert_eq!(Origin::local(8787).as_str(), "http://127.0.0.1:8787");
    }

    #[test]
    fn shareable_urls_drop_transient_parameters() {
        assert_eq!(shareable("/", None), "/");
        assert_eq!(
            shareable("/mod", Some("tab=holds&done=1")),
            "/mod?tab=holds"
        );
        assert_eq!(shareable("/settings", Some("notice=saved")), "/settings");
        assert_eq!(
            shareable("/search", Some("q=rust+tag%3Aweb&page=2")),
            "/search?q=rust+tag%3Aweb&page=2"
        );
        assert_eq!(shareable("/s/abc", Some("duplicate=1")), "/s/abc");
        assert_eq!(shareable("/top", Some("&&t=week")), "/top?t=week");
    }
}
