//! Saved stories, hidden stories, and tag filters: one reader's own view.

mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use common::world::*;

async fn json_post(w: &World, uri: &str, who: &str, body: &str) -> Res {
    w.send(
        Request::post(uri)
            .header(header::COOKIE, who)
            .header(header::ACCEPT, "application/json")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(format!("csrf={}&{body}", w.csrf)))
            .unwrap(),
    )
    .await
}

#[tokio::test]
async fn saving_and_hiding_change_only_the_readers_lists() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let bob = w.user("bob.example", &base(), 100, 0).await;
    let id = w
        .submit(&alice, "Kept story", "https://e.example/k", &["rust"])
        .await;
    let other = w
        .submit(&alice, "Other story", "https://e.example/o", &["rust"])
        .await;

    // Without JavaScript: a form post and back to the page it came from.
    let r = w
        .post(
            &format!("/s/{id}/save"),
            &bob,
            &[("on", "1"), ("next", "/newest")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.location.as_deref(), Some("/newest"));
    let saved = w.get("/saved", &bob).await;
    assert_eq!(saved.status, StatusCode::OK);
    assert!(saved.body.contains("Kept story"));
    assert!(!saved.body.contains("Other story"));
    assert!(
        saved
            .body
            .contains(r#"<span data-label>unsave</span><span class="vh">: Kept story</span>"#),
        "{}",
        saved.body
    );
    assert!(
        !w.get("/saved", &alice).await.body.contains("Kept story"),
        "saves are per reader"
    );

    // With JavaScript: JSON, and the next form flips back.
    let r = json_post(&w, &format!("/s/{other}/hide"), &bob, "on=1").await;
    assert_eq!(r.body, r#"{"on":true}"#);
    for page in [
        "/",
        "/newest",
        "/t/rust",
        "/domain/e.example",
        "/search?q=story",
    ] {
        let body = w.get(page, &bob).await.body;
        assert!(!body.contains("Other story"), "{page}: hidden from bob");
        assert!(body.contains("Kept story"), "{page}");
        assert!(
            w.get(page, &alice).await.body.contains("Other story"),
            "{page}: alice still sees it"
        );
    }
    let hidden = w.get("/hidden", &bob).await.body;
    assert!(hidden.contains("Other story"));
    assert!(hidden.contains(r#"<span data-label>unhide</span>"#));
    assert!(!hidden.contains("Kept story"));

    let r = json_post(&w, &format!("/s/{other}/hide"), &bob, "on=0").await;
    assert_eq!(r.body, r#"{"on":false}"#);
    assert!(w.get("/", &bob).await.body.contains("Other story"));
    assert!(w
        .get("/hidden", &bob)
        .await
        .body
        .contains("No hidden stories."));

    let r = w.post(&format!("/s/{id}/save"), &bob, &[("on", "0")]).await;
    assert_eq!(r.location, Some(format!("/s/{id}")), "back to the story");
    assert!(w
        .get("/saved", &bob)
        .await
        .body
        .contains("No saved stories."));
    assert!(w.writes().len() == 2, "saves and hides write no records");
}

#[tokio::test]
async fn marks_need_a_reader_a_story_and_a_clear_answer() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let bob = w.user("bob.example", &base(), 100, 0).await;
    let id = w
        .submit(&alice, "Story", "https://e.example/m", &["rust"])
        .await;

    for page in ["/saved", "/hidden"] {
        let r = w.get(page, "").await;
        assert_eq!(r.location, Some(format!("/login?next={page}")));
    }
    let r = w
        .post(
            &format!("/s/{id}/save"),
            &format!("csrf={}", w.csrf),
            &[("on", "1"), ("next", "/newest")],
        )
        .await;
    assert_eq!(r.location.as_deref(), Some("/login?next=/newest"));

    let r = w
        .post(&format!("/s/{id}/save"), &bob, &[("on", "yes")])
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.body.contains("That form is missing a field."));
    let r = w.post("/s/nope00/hide", &bob, &[("on", "1")]).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = w
        .post(
            &format!("/s/{id}/save"),
            &bob,
            &[("on", "1"), ("next", "//evil.example")],
        )
        .await;
    assert_eq!(r.location.as_deref(), Some("/"), "next stays on this site");

    // A story moderators removed takes no new mark, but an old one can go.
    sqlx::query("UPDATE stories SET removed_at = 1 WHERE short_id = ?")
        .bind(&id)
        .execute(w.store.pool())
        .await
        .unwrap();
    let r = w.post(&format!("/s/{id}/hide"), &bob, &[("on", "1")]).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let authors_view = w.get(&format!("/s/{id}"), &alice).await.body;
    assert!(authors_view.contains("Story"));
    assert!(
        !authors_view.contains(&format!(r#"action="/s/{id}/save""#)),
        "a removed story's page offers no save"
    );
    let r = w.post(&format!("/s/{id}/save"), &bob, &[("on", "0")]).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM saved_stories")
        .fetch_one(w.store.pool())
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn tag_filters_from_settings() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let bob = w.user("bob.example", &base(), 100, 0).await;
    w.submit(&alice, "Rust story", "https://e.example/r", &["rust"])
        .await;
    let settings = w.get("/settings", &bob).await.body;
    assert!(settings.contains(r#"<legend>Hide stories tagged</legend>"#));
    assert!(
        settings.contains(r#"name="tags" value="rust">"#),
        "{settings}"
    );

    let r = w.post("/settings/tags", &bob, &[("tags", "rust")]).await;
    assert_eq!(r.location.as_deref(), Some("/settings?notice=saved"));
    assert!(w
        .get("/settings", &bob)
        .await
        .body
        .contains(r#"name="tags" value="rust" checked>"#));
    assert!(!w.get("/", &bob).await.body.contains("Rust story"));
    assert!(!w
        .get("/search?q=story", &bob)
        .await
        .body
        .contains("Rust story"));
    assert!(w.get("/t/rust", &bob).await.body.contains("Rust story"));
    assert!(w
        .get("/search?q=tag%3Arust", &bob)
        .await
        .body
        .contains("Rust story"));
    assert!(w.get("/", &alice).await.body.contains("Rust story"));
    assert!(w.get("/", "").await.body.contains("Rust story"));

    let r = w
        .post(
            "/settings/tags",
            &bob,
            &[("tags", "rust"), ("tags", "nope")],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(
        r.body.contains("There is no tag named &#34;nope&#34;."),
        "{}",
        r.body
    );
    assert!(
        !w.get("/", &bob).await.body.contains("Rust story"),
        "a refused form changes nothing"
    );

    let r = w.post("/settings/tags", &bob, &[]).await;
    assert_eq!(r.location.as_deref(), Some("/settings?notice=saved"));
    assert!(w.get("/", &bob).await.body.contains("Rust story"));

    let signed_out = format!("csrf={}", w.csrf);
    let r = w
        .post("/settings/tags", &signed_out, &[("tags", "rust")])
        .await;
    assert_eq!(r.location.as_deref(), Some("/login?next=/settings"));
}

#[tokio::test]
async fn rows_offer_save_and_hide_to_readers_who_are_signed_in() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let id = w
        .submit(&alice, "Story", "https://e.example/s", &["rust"])
        .await;
    let out = w.get("/", "").await.body;
    assert!(!out.contains("/save\""));
    assert!(!out.contains(r#"href="/saved""#));
    let signed_in = w.get("/", &alice).await.body;
    assert!(signed_in.contains(&format!(r#"action="/s/{id}/save""#)));
    assert!(signed_in.contains(&format!(r#"action="/s/{id}/hide""#)));
    assert!(signed_in.contains(r#"href="/saved""#));

    // Forms on a search page come back to the same search.
    let found = w.get("/search?q=story&x=1", &alice).await.body;
    assert!(
        found.contains(r#"name="next" value="/search?q=story&#38;x=1""#),
        "{found}"
    );
}
