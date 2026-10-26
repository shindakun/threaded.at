use pulldown_cmark::{html, CowStr, Event, Options, Parser, Tag, TagEnd};

/// Markdown written by users: no images, no headings, no raw HTML.
/// Headings become paragraphs, images become their alt text, and HTML shows as
/// written. Only `https` links keep their target, open in a new tab, and carry
/// `rel="nofollow ugc noopener noreferrer"`; any other link is plain text.
pub fn user(md: &str) -> String {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut in_image = 0usize;
    let events = Parser::new_ext(md, opts).filter_map(|e| match e {
        Event::Start(Tag::Heading { .. }) => Some(Event::Start(Tag::Paragraph)),
        Event::End(TagEnd::Heading(_)) => Some(Event::End(TagEnd::Paragraph)),
        Event::Start(Tag::Image { .. }) => {
            in_image += 1;
            None
        }
        Event::End(TagEnd::Image) => {
            in_image = in_image.saturating_sub(1);
            None
        }
        Event::Html(h) | Event::InlineHtml(h) => Some(Event::Text(h)),
        Event::Text(t) if in_image > 0 => Some(Event::Text(CowStr::from(t.into_string()))),
        e => Some(e),
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    ammonia::Builder::default()
        .rm_tags(&["img", "h1", "h2", "h3", "h4", "h5", "h6"])
        .link_rel(Some("nofollow ugc noopener noreferrer"))
        .set_tag_attribute_value("a", "target", "_blank")
        .url_schemes(["https"].into_iter().collect())
        .clean(&out)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_text_has_no_images_headings_or_html() {
        let h = user("# Big\n\n![alt text](https://x.example/i.png) and <b>bold</b> <script>x</script>\n\n[link](https://example.com)");
        assert!(
            !h.contains("<h1") && !h.contains("<img") && !h.contains("<script"),
            "{h}"
        );
        assert!(h.contains("<p>Big</p>"), "{h}");
        assert!(h.contains("alt text"), "{h}");
        assert!(h.contains("&lt;b&gt;bold&lt;/b&gt;"), "{h}");
        assert!(h.contains(r#"target="_blank""#), "{h}");
        assert!(
            h.contains(r#"rel="nofollow ugc noopener noreferrer""#),
            "{h}"
        );
    }

    #[test]
    fn only_https_links_keep_their_target() {
        let h = user("[safe](https://example.com) [plain](http://example.com)");
        assert!(h.contains(r#"href="https://example.com""#), "{h}");
        assert!(!h.contains("http://example.com"), "{h}");
        assert!(h.contains("plain"), "the text stays: {h}");
    }

    #[test]
    fn javascript_links_are_removed() {
        let h = user("[x](javascript:alert(1))");
        assert!(!h.contains("javascript:"), "{h}");
    }
}
