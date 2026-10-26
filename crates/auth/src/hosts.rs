/// How a host is shown on the sign-in pages. `unicode` is set only when the
/// host is an internationalized name, so look-alike domains show both forms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostView {
    pub ascii: String,
    pub unicode: Option<String>,
}

pub fn host_of(url: &str) -> Option<HostView> {
    let u = url::Url::parse(url).ok()?;
    let ascii = u.host_str()?.to_ascii_lowercase();
    let (uni, res) = idna::domain_to_unicode(&ascii);
    let unicode = (res.is_ok() && uni != ascii).then_some(uni);
    Some(HostView { ascii, unicode })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_host() {
        assert_eq!(
            host_of("https://bsky.social/oauth/authorize?x=1"),
            Some(HostView {
                ascii: "bsky.social".into(),
                unicode: None
            })
        );
    }

    #[test]
    fn idn_host_shows_both_forms() {
        let h = host_of("https://bücher.example/").unwrap();
        assert_eq!(h.ascii, "xn--bcher-kva.example");
        assert_eq!(h.unicode.as_deref(), Some("bücher.example"));
    }

    #[test]
    fn rejects_non_urls() {
        assert_eq!(host_of("not a url"), None);
    }
}
