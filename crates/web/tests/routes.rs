mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use threaded_core::link;
use threaded_store::{NewStory, Store};
use threaded_web::router;
use tower::ServiceExt;

const NOW: i64 = 1_790_000_000;

async fn app() -> (axum::Router, String) {
    let store = Store::open("sqlite::memory:").await.unwrap();
    let u = store
        .record_sign_in("did:plc:alice", "alice.example", true, NOW)
        .await
        .unwrap();
    store
        .upsert_tag("rust", "Rust the language", 0.0)
        .await
        .unwrap();
    let l = link::parse("https://blog.example.com/post").unwrap();
    let id = store
        .insert_story(&NewStory {
            user_id: u,
            title: "Borrowing <b>without</b> tears",
            link: Some(&l),
            body: "",
            user_is_author: false,
            tags: &["rust"],
            record: None,
            created_at: NOW - 7200,
        })
        .await
        .unwrap();
    let state = common::state(
        &store,
        Default::default(),
        NOW,
        std::path::Path::new("/nonexistent/blog"),
    );
    (router(state), id)
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, String) {
    let res = app
        .clone()
        .oneshot(Request::get(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn front_page_lists_and_escapes() {
    let (app, _) = app().await;
    let (status, body) = get(&app, "/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("Borrowing &#60;b&#62;without&#60;/b&#62; tears"),
        "{body}"
    );
    assert!(body.contains(r#"href="/t/rust""#));
    assert!(body.contains("blog.example.com"));
    assert!(body.contains("2 hours ago"));
    assert!(body.contains("no comments"));
}

#[tokio::test]
async fn every_listing_renders() {
    let (app, id) = app().await;
    for uri in [
        "/newest",
        "/active",
        "/top",
        "/top?t=all&page=2",
        "/t/rust",
        "/domain/example.com",
        "/tags",
        "/search",
        "/search?q=borrowing+tag%3Arust",
        &format!("/s/{id}"),
    ] {
        let (status, _) = get(&app, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
    }
}

#[tokio::test]
async fn search_finds_by_text() {
    let (app, _) = app().await;
    let (_, body) = get(&app, "/search?q=tears").await;
    assert!(body.contains("Borrowing"));
    let (_, body) = get(&app, "/search?q=tears+-tag%3Arust").await;
    assert!(body.contains("No results."));
}

#[tokio::test]
async fn unknown_things_are_404() {
    let (app, _) = app().await;
    for uri in [
        "/s/nope00",
        "/t/Not%20A%20Tag",
        "/domain/%3Cx%3E",
        "/nowhere",
    ] {
        let (status, _) = get(&app, uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test]
async fn health_and_css() {
    let (app, _) = app().await;
    assert_eq!(get(&app, "/healthz").await, (StatusCode::OK, "ok".into()));
    let res = app
        .oneshot(
            Request::get("/static/site.css")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.headers()["content-type"], "text/css; charset=utf-8");
}

#[tokio::test]
async fn pages_have_one_h1_a_distinct_title_and_a_skip_link() {
    let (app, id) = app().await;
    for uri in [
        "/",
        "/newest",
        "/active",
        "/top",
        "/t/rust",
        "/domain/example.com",
        "/tags",
        "/search?q=tears",
        &format!("/s/{id}"),
        "/s/nope00",
        "/blog",
    ] {
        let (_, body) = get(&app, uri).await;
        assert_eq!(body.matches("<h1").count(), 1, "{uri}");
        assert!(!body.contains("<title>Threaded</title>"), "{uri}");
        assert!(body.contains(r##"<a class="skip" href="#main">"##), "{uri}");
    }
    let (_, body) = get(&app, "/newest").await;
    assert!(body.contains(r#"<a href="/newest" aria-current="page">"#));
    assert_eq!(body.matches("aria-current").count(), 1);
}

/// The `content` of each `<meta property="og:NAME">` on a page.
fn og<'a>(body: &'a str, name: &str) -> Vec<&'a str> {
    let key = format!(r#"<meta property="og:{name}" content=""#);
    body.match_indices(&key)
        .map(|(i, _)| {
            let rest = &body[i + key.len()..];
            &rest[..rest.find('"').unwrap()]
        })
        .collect()
}

#[tokio::test]
async fn pages_carry_link_preview_tags() {
    let (app, id) = app().await;
    let site = "A link aggregator on the AT Protocol: tagged stories and threaded comments, with moderation in the open.";

    // The home page and other listings get the site's own preview.
    for uri in ["/", "/newest", "/tags"] {
        let (_, body) = get(&app, uri).await;
        assert_eq!(og(&body, "title"), ["Threaded"], "{uri}");
        assert_eq!(og(&body, "description"), [site], "{uri}");
        assert_eq!(og(&body, "type"), ["website"], "{uri}");
        assert_eq!(og(&body, "site_name"), ["Threaded"], "{uri}");
        assert!(body.contains(r#"<meta name="twitter:card" content="summary">"#));
    }

    // A story previews as itself, escaped.
    let (_, body) = get(&app, &format!("/s/{id}")).await;
    assert_eq!(
        og(&body, "title"),
        ["Borrowing &#60;b&#62;without&#60;/b&#62; tears"]
    );
    assert_eq!(
        og(&body, "description"),
        ["A link to blog.example.com, posted by alice.example, with no comments."]
    );
    assert_eq!(og(&body, "type"), ["article"]);
    assert_eq!(og(&body, "site_name"), ["Threaded"]);
}

/// Search terms are bound parameters and FTS phrases, never SQL: input that
/// would widen the match if it were spliced in matches nothing.
#[tokio::test]
async fn hostile_search_input_matches_nothing_and_breaks_nothing() {
    let (app, _) = app().await;
    for q in [
        "'; DROP TABLE stories; --",
        "\" OR 1=1 --",
        "borrowing\" OR \"1\"=\"1",
        "*",
        "NEAR(a b)",
        "title:\"x",
        "a AND",
        ")",
        "\"",
        "\\",
        "tag:rust' OR '1'='1",
        "domain:%",
        "domain:_",
        "by:x' OR 1=1--",
        "commenter:\")) OR 1=1--",
    ] {
        let uri = format!("/search?q={}", url_encode(q));
        let (status, body) = get(&app, &uri).await;
        assert_eq!(status, StatusCode::OK, "{q}");
        assert!(!body.contains("Borrowing"), "{q} matched the story");
    }
    // The story is still there, and plain search still finds it.
    let (_, body) = get(&app, "/search?q=borrowing").await;
    assert!(body.contains("Borrowing"));
}

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[tokio::test]
async fn every_page_names_its_shareable_url() {
    let (app, id) = app().await;
    for (uri, want) in [
        ("/", "https://threaded.test/"),
        ("/newest", "https://threaded.test/newest"),
        ("/top?t=week", "https://threaded.test/top?t=week"),
        ("/search?q=rust", "https://threaded.test/search?q=rust"),
    ] {
        let (_, body) = get(&app, uri).await;
        assert_eq!(og(&body, "url"), [want], "{uri}");
    }
    let (_, body) = get(&app, &format!("/s/{id}?duplicate=1")).await;
    assert_eq!(
        og(&body, "url"),
        [format!("https://threaded.test/s/{id}")],
        "a one-time notice is not part of the shared URL"
    );
    let (status, body) = get(&app, "/s/nope00").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(og(&body, "url"), ["https://threaded.test/s/nope00"]);
}
