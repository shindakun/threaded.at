//! Atom feeds: well-formed, valid against RFC 4287's required elements, the
//! public view of each listing, and linked from the pages they follow.

mod common;

use std::collections::HashSet;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use common::world::*;
use http_body_util::BodyExt;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tower::ServiceExt;

const ATOM: &str = "http://www.w3.org/2005/Atom";
const ORIGIN: &str = "https://threaded.test";

#[derive(Debug)]
struct Entry {
    id: String,
    title: String,
    link: String,
    replies: Option<String>,
    published: String,
    author: Option<(String, Option<String>)>,
    categories: Vec<String>,
    summary: Option<String>,
    content: String,
}

#[derive(Debug)]
struct Feed {
    title: String,
    updated: String,
    author: Option<String>,
    entries: Vec<Entry>,
}

/// Fetches a feed with no cookies, as a feed reader would, and checks the
/// response and the document.
async fn feed(w: &World, path: &str) -> Feed {
    let (status, headers, body) = fetch(w, path, "").await;
    assert_eq!(status, StatusCode::OK, "{path}: {body}");
    assert_eq!(
        headers[header::CONTENT_TYPE],
        "application/atom+xml; charset=utf-8"
    );
    assert_eq!(headers[header::CACHE_CONTROL], "public, max-age=300");
    assert!(
        headers.get(header::SET_COOKIE).is_none(),
        "{path}: a feed sets no cookie"
    );
    validate(&body, &format!("{ORIGIN}{path}"))
}

async fn fetch(w: &World, path: &str, cookie: &str) -> (StatusCode, HeaderMap, String) {
    let res = w
        .app
        .clone()
        .oneshot(
            Request::get(path)
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8(body.to_vec()).unwrap())
}

fn date(s: &str) -> String {
    assert!(
        OffsetDateTime::parse(s, &Rfc3339).is_ok(),
        "{s:?} is not an RFC 3339 date"
    );
    s.to_string()
}

fn absolute(s: &str) -> String {
    assert!(
        s.starts_with(&format!("{ORIGIN}/")),
        "{s:?} is not absolute"
    );
    s.to_string()
}

/// The checks RFC 4287 section 4.1 makes of a feed and its entries: one
/// `id`, `title`, and `updated` each; dates in RFC 3339; an author for every
/// entry, its own or the feed's; an alternate link for every entry; and no
/// entry ID twice.
fn validate(xml: &str, self_url: &str) -> Feed {
    let doc = roxmltree::Document::parse(xml).unwrap_or_else(|e| panic!("{e}: {xml}"));
    let root = doc.root_element();
    assert!(root.has_tag_name((ATOM, "feed")), "{xml}");
    assert_eq!(
        root.attribute(("http://www.w3.org/XML/1998/namespace", "base")),
        Some("https://threaded.test/")
    );
    let one = |n: roxmltree::Node, name: &str| -> String {
        let found: Vec<_> = n
            .children()
            .filter(|c| c.has_tag_name((ATOM, name)))
            .collect();
        assert_eq!(found.len(), 1, "one <{name}> in {n:?}");
        found[0].text().unwrap_or_default().to_string()
    };
    let opt = |n: roxmltree::Node, name: &str| -> Option<String> {
        n.children()
            .find(|c| c.has_tag_name((ATOM, name)))
            .map(|c| c.text().unwrap_or_default().to_string())
    };
    let links = |n: roxmltree::Node, rel: &str| -> Vec<String> {
        n.children()
            .filter(|c| c.has_tag_name((ATOM, "link")))
            .filter(|c| c.attribute("rel").unwrap_or("alternate") == rel)
            .map(|c| c.attribute("href").unwrap().to_string())
            .collect()
    };
    let author = |n: roxmltree::Node| {
        n.children()
            .find(|c| c.has_tag_name((ATOM, "author")))
            .map(|a| (one(a, "name"), opt(a, "uri")))
    };

    assert_eq!(one(root, "id"), self_url);
    assert_eq!(links(root, "self"), vec![self_url.to_string()]);
    assert_eq!(links(root, "alternate").len(), 1);
    absolute(&links(root, "alternate")[0]);
    let feed_author = author(root).map(|a| a.0);
    let mut ids = HashSet::new();
    let entries: Vec<Entry> = root
        .children()
        .filter(|c| c.has_tag_name((ATOM, "entry")))
        .map(|e| {
            let id = absolute(&one(e, "id"));
            assert!(ids.insert(id.clone()), "entry {id} twice");
            let alt = links(e, "alternate");
            assert_eq!(alt.len(), 1, "{id}");
            date(&one(e, "updated"));
            let author = author(e);
            assert!(
                author.is_some() || feed_author.is_some(),
                "{id} has no author"
            );
            let content = e
                .children()
                .find(|c| c.has_tag_name((ATOM, "content")))
                .unwrap();
            assert_eq!(content.attribute("type"), Some("html"));
            Entry {
                id,
                title: one(e, "title"),
                link: alt[0].clone(),
                replies: links(e, "replies").first().cloned(),
                published: date(&one(e, "published")),
                author,
                categories: e
                    .children()
                    .filter(|c| c.has_tag_name((ATOM, "category")))
                    .map(|c| c.attribute("term").unwrap().to_string())
                    .collect(),
                summary: opt(e, "summary"),
                content: content.text().unwrap_or_default().to_string(),
            }
        })
        .collect();
    Feed {
        title: one(root, "title"),
        updated: date(&one(root, "updated")),
        author: feed_author,
        entries,
    }
}

fn titles(f: &Feed) -> Vec<&str> {
    f.entries.iter().map(|e| e.title.as_str()).collect()
}

#[tokio::test]
async fn feeds_carry_the_public_listings_with_absolute_links() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let bob = w.user("bob.example", &base(), 100, 0).await;
    let link = w
        .submit(
            &alice,
            "Fish & chips <b>\"quoted\"</b>",
            "https://e.example/fish?a=1&b=2",
            &["meta", "rust"],
        )
        .await;
    // A text post: no link, a body in Markdown.
    let r = w
        .post(
            "/submit",
            &bob,
            &[
                ("title", "Ask about feeds"),
                ("url", ""),
                ("text", "Is *this* <script>x</script> fine?"),
                ("tags", "ask"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    let text = r.location.unwrap().trim_start_matches("/s/").to_string();
    let gone = w
        .submit(&alice, "Removed story", "https://e.example/gone", &["rust"])
        .await;
    sqlx::query("UPDATE stories SET removed_at = 1 WHERE short_id = ?")
        .bind(&gone)
        .execute(w.store.pool())
        .await
        .unwrap();

    let front = feed(&w, "/feed").await;
    assert_eq!(front.title, "Threaded: front page");
    assert_eq!(front.author, None);
    let mut t = titles(&front);
    t.sort();
    assert_eq!(t, vec!["Ask about feeds", "Fish & chips <b>\"quoted\"</b>"]);

    let fish = front
        .entries
        .iter()
        .find(|e| e.title.starts_with("Fish"))
        .unwrap();
    assert_eq!(fish.id, format!("{ORIGIN}/s/{link}"));
    assert_eq!(fish.link, "https://e.example/fish?a=1&b=2");
    assert_eq!(fish.replies.as_deref(), Some(fish.id.as_str()));
    assert_eq!(fish.published, "2026-09-21T14:13:20Z");
    assert_eq!(
        fish.author,
        Some((
            "alice.example".to_string(),
            Some(format!("{ORIGIN}/u/did:plc:alice"))
        ))
    );
    assert_eq!(fish.categories, vec!["meta", "rust"]);
    assert!(fish.content.contains(&format!(
        r#"<a href="{ORIGIN}/t/meta">meta</a>, <a href="{ORIGIN}/t/rust">rust</a> | by <a href="{ORIGIN}/u/did:plc:alice">alice.example</a> | <a href="{ORIGIN}/s/{link}">Comments</a>"#
    )), "{}", fish.content);

    let ask = front
        .entries
        .iter()
        .find(|e| e.title == "Ask about feeds")
        .unwrap();
    assert_eq!(
        ask.link,
        format!("{ORIGIN}/s/{text}"),
        "a text post links its page"
    );
    assert_eq!(ask.replies, None);
    assert!(ask.content.contains("Is <em>this</em>"), "{}", ask.content);
    assert!(!ask.content.contains("<script"), "the body is sanitized");

    let newest = feed(&w, "/newest/feed").await;
    assert_eq!(newest.entries.len(), 2);
    let rust = feed(&w, "/t/rust/feed").await;
    assert_eq!(rust.title, "Threaded: stories tagged rust");
    assert_eq!(titles(&rust), vec!["Fish & chips <b>\"quoted\"</b>"]);
    let domain = feed(&w, "/domain/e.example/feed").await;
    assert_eq!(domain.title, "Threaded: stories from e.example");
    assert_eq!(titles(&domain), vec!["Fish & chips <b>\"quoted\"</b>"]);
    // Any case finds the domain; the feed names itself in lowercase.
    let (_, _, body) = fetch(&w, "/domain/E.Example/feed", "").await;
    assert_eq!(
        titles(&validate(&body, &format!("{ORIGIN}/domain/e.example/feed"))).len(),
        1
    );
    let user = feed(&w, "/u/did:plc:bob/feed").await;
    assert_eq!(user.title, "Threaded: stories by bob.example");
    assert_eq!(titles(&user), vec!["Ask about feeds"]);
    assert!(feed(&w, "/t/show/feed").await.entries.is_empty());

    // A feed is the public view even with a reader's cookie: bob's hides and
    // tag filters stay his.
    w.post(&format!("/s/{link}/hide"), &bob, &[("on", "1")])
        .await;
    w.post("/settings/tags", &bob, &[("tags", "ask")]).await;
    assert!(!w.get("/", &bob).await.body.contains("Ask about feeds"));
    let (_, headers, body) = fetch(&w, "/feed", &bob).await;
    assert!(headers.get(header::SET_COOKIE).is_none());
    assert_eq!(titles(&validate(&body, &format!("{ORIGIN}/feed"))).len(), 2);
}

#[tokio::test]
async fn characters_xml_cannot_carry_are_dropped() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let id = w
        .submit(&alice, "Plain", "https://e.example/c", &["rust"])
        .await;
    sqlx::query("UPDATE stories SET title = ?, body = ? WHERE short_id = ?")
        .bind("Bell\u{7} and null\u{0} and \u{fffe}end")
        .bind("Text with a form feed\u{c}here")
        .bind(&id)
        .execute(w.store.pool())
        .await
        .unwrap();
    let f = feed(&w, "/newest/feed").await;
    assert_eq!(titles(&f), vec!["Bell and null and end"]);
    assert!(f.entries[0].content.contains("form feedhere"));
}

#[tokio::test]
async fn feed_addresses_are_checked() {
    let w = world().await;
    w.user("alice.example", &base(), 100, 0).await;
    let (status, headers, _) = fetch(&w, "/u/alice.example/feed", "").await;
    assert_eq!(status, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(headers[header::LOCATION], "/u/did:plc:alice/feed");
    for path in [
        "/u/nobody.example/feed",
        "/u/did:plc:nobody/feed",
        "/t/Not%20a%20tag/feed",
        "/domain/bad_domain!/feed",
    ] {
        let (status, _, _) = fetch(&w, path, "").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
    let empty = feed(&w, "/newest/feed").await;
    assert!(empty.entries.is_empty());
    assert_eq!(
        empty.updated, "2026-09-21T14:13:20Z",
        "an empty feed is dated now"
    );
}

#[tokio::test]
async fn pages_link_their_feeds() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    w.submit(&alice, "Story", "https://e.example/p", &["rust"])
        .await;
    for (page, href, title) in [
        ("/", "/feed", "Threaded: front page"),
        ("/?page=2", "/feed", "Threaded: front page"),
        ("/newest", "/newest/feed", "Threaded: newest stories"),
        ("/t/rust", "/t/rust/feed", "Threaded: stories tagged rust"),
        (
            "/domain/e.example",
            "/domain/e.example/feed",
            "Threaded: stories from e.example",
        ),
        (
            "/u/alice.example",
            "/u/did:plc:alice/feed",
            "Threaded: stories by alice.example",
        ),
        (
            "/u/did:plc:alice",
            "/u/did:plc:alice/feed",
            "Threaded: stories by alice.example",
        ),
        ("/blog", "/blog/feed", "Threaded blog"),
    ] {
        let body = w.get(page, &alice).await.body;
        assert!(
            body.contains(&format!(
                r#"<link rel="alternate" type="application/atom+xml" title="{title}" href="{href}">"#
            )),
            "{page}: {body}"
        );
        assert!(
            body.contains(&format!(
                r#"<a href="{href}" type="application/atom+xml">Atom feed</a>"#
            )),
            "{page}"
        );
    }
    for page in ["/active", "/top", "/saved", "/search?q=story", "/tags"] {
        let body = w.get(page, &alice).await.body;
        assert!(!body.contains("application/atom+xml"), "{page}");
    }
}

#[tokio::test]
async fn the_blog_feed_carries_published_posts() {
    let dir = tempfile::tempdir().unwrap();
    let blog = dir.path().to_path_buf();
    let w = world_custom(NOW, None, move |s| {
        s.blog = Arc::new(threaded_web::blog::Blog::new(
            &blog,
            std::time::Duration::ZERO,
        ))
    })
    .await;
    assert!(feed(&w, "/blog/feed").await.entries.is_empty());
    std::fs::write(
        dir.path().join("first.md"),
        "---\ntitle: First & best\ndate: 2026-10-20\nsummary: What changed\nauthor: Ana Writer\n---\n# Changes\n\nSee [the story](/s/abc).\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("second.md"),
        "---\ntitle: Second\ndate: 2026-10-22\n---\nMore.\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("draft.md"),
        "---\ntitle: Draft\ndate: 2026-10-23\ndraft: true\n---\nNot yet.\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("feed.md"),
        "---\ntitle: Shadowed\ndate: 2026-10-24\n---\nThe feed has this path.\n",
    )
    .unwrap();

    let f = feed(&w, "/blog/feed").await;
    assert_eq!(f.title, "Threaded blog");
    assert_eq!(f.author.as_deref(), Some("Threaded"));
    assert_eq!(titles(&f), vec!["Second", "First & best"]);
    assert_eq!(f.updated, "2026-10-22T00:00:00Z");
    let first = &f.entries[1];
    assert_eq!(first.id, format!("{ORIGIN}/blog/first"));
    assert_eq!(first.link, first.id);
    assert_eq!(first.published, "2026-10-20T00:00:00Z");
    assert_eq!(first.author, Some(("Ana Writer".to_string(), None)));
    assert_eq!(first.summary.as_deref(), Some("What changed"));
    assert!(
        first.content.contains("<h2>Changes</h2>"),
        "{}",
        first.content
    );
    assert!(
        first.content.contains(r#"<a href="/s/abc""#),
        "xml:base resolves it"
    );
    assert_eq!(f.entries[0].author, None, "the feed's author stands in");
    let (status, _, _) = fetch(&w, "/blog/feed", "").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a post named feed does not take the path"
    );
}
