mod common;

use std::path::Path;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use threaded_store::Store;
use threaded_web::router;
use tower::ServiceExt;

async fn app(dir: &Path) -> axum::Router {
    let store = Store::open("sqlite::memory:").await.unwrap();
    router(common::state(
        &store,
        Default::default(),
        1_790_000_000,
        dir,
    ))
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

fn write(dir: &Path, name: &str, title: &str, date: &str, extra: &str, body: &str) {
    let src = format!("---\ntitle: {title}\ndate: {date}\n{extra}---\n{body}\n");
    std::fs::write(dir.join(name), src).unwrap();
}

#[tokio::test]
async fn posts_load_change_and_disappear_without_restart() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path()).await;

    let (status, body) = get(&app, "/blog").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("No posts."));

    write(
        dir.path(),
        "first-update.md",
        "First update",
        "2026-10-20",
        "summary: What changed\n",
        "# Changes\n\nWe shipped it.",
    );
    let (_, body) = get(&app, "/blog").await;
    assert!(
        body.contains(r#"<a href="/blog/first-update">First update</a>"#),
        "{body}"
    );
    assert!(body.contains(r#"<time datetime="2026-10-20">20 October 2026</time>"#));
    assert!(body.contains("What changed"));

    let (status, body) = get(&app, "/blog/first-update").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("<title>First update | Threaded</title>"));
    assert_eq!(body.matches("<h1").count(), 1);
    assert!(body.contains("<h2>Changes</h2>"));
    assert!(body.contains("We shipped it."));

    write(
        dir.path(),
        "first-update.md",
        "First update, revised",
        "2026-10-20",
        "",
        "Edited body, now longer than before.",
    );
    let (_, body) = get(&app, "/blog/first-update").await;
    assert!(body.contains("First update, revised"));
    assert!(body.contains("Edited body"));

    std::fs::remove_file(dir.path().join("first-update.md")).unwrap();
    let (status, _) = get(&app, "/blog/first-update").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn index_is_newest_first_and_paginated() {
    let dir = tempfile::tempdir().unwrap();
    for day in 1..=12 {
        let date = format!("2026-09-{day:02}");
        write(
            dir.path(),
            &format!("post-{day:02}.md"),
            &format!("Post {day}"),
            &date,
            "",
            "Body.",
        );
    }
    let app = app(dir.path()).await;

    let (_, page1) = get(&app, "/blog").await;
    assert!(page1.find("Post 12<").unwrap() < page1.find("Post 3<").unwrap());
    assert!(!page1.contains("Post 2<"));
    assert!(page1.contains(r#"href="/blog?page=2" rel="next""#));
    assert!(!page1.contains(r#"rel="prev""#));

    let (status, page2) = get(&app, "/blog?page=2").await;
    assert_eq!(status, StatusCode::OK);
    assert!(page2.contains("<title>Blog, page 2 | Threaded</title>"));
    assert!(page2.contains("Post 2<") && page2.contains("Post 1<"));
    assert!(!page2.contains("Post 12<"));
    assert!(page2.contains(r#"href="/blog?page=1" rel="prev""#));
    assert!(!page2.contains(r#"rel="next""#));

    let (status, _) = get(&app, "/blog?page=3").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn drafts_bad_files_and_duplicates_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "good.md", "Good", "2026-10-01", "", "ok");
    write(
        dir.path(),
        "draft.md",
        "Draft",
        "2026-10-02",
        "draft: true\n",
        "wip",
    );
    write(dir.path(), "bad-date.md", "Bad", "2026-02-30", "", "x");
    std::fs::write(dir.path().join("no-header.md"), "just text").unwrap();
    std::fs::write(
        dir.path().join("notes.txt"),
        "---\ntitle: Txt\ndate: 2026-10-03\n---\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join(".hidden.md"),
        "---\ntitle: Hidden\ndate: 2026-10-03\n---\n",
    )
    .unwrap();
    write(
        dir.path(),
        "a-dup.md",
        "Dup A",
        "2026-10-04",
        "slug: same\n",
        "a",
    );
    write(
        dir.path(),
        "b-dup.md",
        "Dup B",
        "2026-10-04",
        "slug: same\n",
        "b",
    );
    let app = app(dir.path()).await;

    let (_, body) = get(&app, "/blog").await;
    assert!(body.contains("Good"));
    for absent in ["Draft", "Bad", "Txt", "Hidden", "Dup B"] {
        assert!(!body.contains(absent), "{absent} should be skipped");
    }
    assert!(body.contains("Dup A"));
    assert_eq!(get(&app, "/blog/draft").await.0, StatusCode::NOT_FOUND);
    assert!(get(&app, "/blog/same").await.1.contains("Dup A"));
}

#[tokio::test]
async fn slugs_never_reach_the_filesystem() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "real.md", "Real", "2026-10-01", "", "ok");
    let app = app(dir.path()).await;
    for uri in [
        "/blog/..%2F..%2Fetc%2Fpasswd",
        "/blog/real.md",
        "/blog/REAL",
        "/blog/%2e%2e",
    ] {
        assert_eq!(get(&app, uri).await.0, StatusCode::NOT_FOUND, "{uri}");
    }
}

#[tokio::test]
async fn missing_directory_is_an_empty_blog() {
    let app = app(Path::new("/nonexistent/threaded-blog")).await;
    let (status, body) = get(&app, "/blog").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("No posts."));
}
