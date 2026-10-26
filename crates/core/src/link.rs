use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The URL as shown and followed, normalised by the `url` crate.
    pub url: String,
    /// Key for duplicate detection. Never shown.
    pub canonical: String,
    /// Host without `www.`, for `domain:` search and domain pages.
    pub domain: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    #[error("not a valid URL")]
    Invalid,
    #[error("links must use https")]
    Scheme,
    #[error("the URL has no host")]
    NoHost,
}

const TRACKING_PARAMS: &[&str] = &["fbclid", "gclid", "mc_cid", "mc_eid", "ref", "ref_src"];

pub fn parse(input: &str) -> Result<Link, LinkError> {
    let url = Url::parse(input.trim()).map_err(|_| LinkError::Invalid)?;
    if url.scheme() != "https" {
        return Err(LinkError::Scheme);
    }
    let host = url
        .host_str()
        .ok_or(LinkError::NoHost)?
        .to_ascii_lowercase();
    let domain = host.strip_prefix("www.").unwrap_or(&host).to_string();

    let mut params: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| !k.starts_with("utm_") && !TRACKING_PARAMS.contains(&k.as_ref()))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    params.sort();

    let path = url.path().trim_end_matches('/');
    let mut canonical = format!("{domain}{path}");
    if !params.is_empty() {
        canonical.push('?');
        let q: Vec<String> = params.iter().map(|(k, v)| format!("{k}={v}")).collect();
        canonical.push_str(&q.join("&"));
    }

    Ok(Link {
        url: url.to_string(),
        canonical,
        domain,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_share_a_canonical_form() {
        let a = parse("https://www.Example.com/post/?utm_source=x&b=2&a=1#frag").unwrap();
        let b = parse("https://example.com/post?a=1&b=2&fbclid=zz").unwrap();
        assert_eq!(a.canonical, "example.com/post?a=1&b=2");
        assert_eq!(a.canonical, b.canonical);
        assert_eq!(a.domain, "example.com");
    }

    #[test]
    fn display_url_keeps_what_the_user_gave() {
        let l = parse("https://example.com/a?utm_source=x").unwrap();
        assert_eq!(l.url, "https://example.com/a?utm_source=x");
    }

    #[test]
    fn rejects_other_schemes() {
        assert_eq!(parse("javascript:alert(1)"), Err(LinkError::Scheme));
        assert_eq!(parse("ftp://example.com/"), Err(LinkError::Scheme));
        assert_eq!(parse("http://example.com/"), Err(LinkError::Scheme));
        assert_eq!(parse("not a url"), Err(LinkError::Invalid));
    }
}
