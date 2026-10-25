use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use threaded_core::link;
use threaded_store::{NewStory, Store};
use threaded_web::blog::Blog;
use threaded_web::{router, AppState};
use tower::ServiceExt;

const NOW: i64 = 1_790_000_000;

async fn app() -> (axum::Router, String) {
    let store = Store::open("sqlite::memory:").await.unwrap();
    let u = store
        .upsert_user("did:plc:alice", "alice.example", NOW)
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
            created_at: NOW - 7200,
        })
        .await
        .unwrap();
    let state = AppState {
        store,
        blog: Arc::new(Blog::new("/nonexistent/blog", std::time::Duration::ZERO)),
        clock: Arc::new(|| NOW),
    };
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
