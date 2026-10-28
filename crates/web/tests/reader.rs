//! Saved stories, hidden stories, and tag filters: one reader's own view.

mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use common::world::*;
use http_body_util::BodyExt;
use tower::ServiceExt;

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

#[tokio::test]
async fn rows_say_how_many_comments_are_new_since_the_last_read() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let bob = w.user("bob.example", &base(), 100, 0).await;
    let id = w
        .submit(&alice, "Story", "https://e.example/n", &["rust"])
        .await;
    w.post("/comment", &alice, &[("story", &id), ("text", "One")])
        .await;
    assert!(
        !w.get("/", &bob).await.body.contains("new</strong>"),
        "nothing is new on a story bob never opened"
    );

    assert_eq!(
        w.get(&format!("/s/{id}"), &bob).await.status,
        StatusCode::OK
    );
    // The test clock stands still, so date the first comment and bob's
    // read in the past; comments posted from here on come after the read.
    sqlx::query("UPDATE comments SET created_at = created_at - 20")
        .execute(w.store.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE story_reads SET read_at = read_at - 10")
        .execute(w.store.pool())
        .await
        .unwrap();
    assert!(!w.get("/", &bob).await.body.contains("new</strong>"));
    w.post("/comment", &alice, &[("story", &id), ("text", "Two")])
        .await;
    w.post("/comment", &bob, &[("story", &id), ("text", "Mine")])
        .await;
    let front = w.get("/", &bob).await.body;
    assert!(
        front.contains(r#"3 comments</a>, <strong class="new">1 new</strong>"#),
        "{front}"
    );
    assert!(
        !w.get("/", &alice).await.body.contains("new</strong>"),
        "ribbons are per reader"
    );
    let page = w.get(&format!("/s/{id}"), &bob).await.body;
    assert!(
        !page.contains("new</strong>"),
        "the story page itself shows nothing new"
    );
    assert!(!w.get("/", &bob).await.body.contains("new</strong>"));
}

#[tokio::test]
async fn a_link_storys_page_links_its_archived_copies() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let id = w
        .submit(&alice, "Linked", "https://e.example/a?b=1&c=2", &["rust"])
        .await;
    let page = w.get(&format!("/s/{id}"), "").await.body;
    assert!(
        page.contains(r#"<a href="https://web.archive.org/web/https://e.example/a?b=1&#38;c=2" target="_blank" rel="noopener">archive.org<span class="vh"> (opens in a new tab)</span></a>"#),
        "{page}"
    );
    assert!(page.contains(
        r#"<a href="https://archive.today/newest/https://e.example/a?b=1&#38;c=2" target="_blank" rel="noopener">archive.today"#
    ));

    let r = w
        .post(
            "/submit",
            &alice,
            &[
                ("title", "Text only"),
                ("url", ""),
                ("text", "Words."),
                ("tags", "rust"),
            ],
        )
        .await;
    let text = r.location.unwrap();
    assert!(!w.get(&text, "").await.body.contains("Archived copies"));
}

#[tokio::test]
async fn folding_loads_only_where_there_are_comments() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let id = w
        .submit(&alice, "Story", "https://e.example/f", &["rust"])
        .await;
    let script = r#"<script src="/static/thread.js" defer></script>"#;
    assert!(!w.get(&format!("/s/{id}"), "").await.body.contains(script));
    w.post("/comment", &alice, &[("story", &id), ("text", "One")])
        .await;
    assert!(w.get(&format!("/s/{id}"), "").await.body.contains(script));
    assert!(!w.get("/", "").await.body.contains(script));
}

#[tokio::test]
async fn keyboard_shortcuts_are_off_until_turned_on() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let script = r#"<script src="/static/keys.js" defer></script>"#;
    let checkbox = r#"<input type="checkbox" name="on" value="1"> Turn on keyboard shortcuts"#;
    assert!(!w.get("/", &alice).await.body.contains(script));
    assert!(w.get("/settings", &alice).await.body.contains(checkbox));

    let r = w.post("/settings/keys", &alice, &[("on", "1")]).await;
    assert_eq!(r.location.as_deref(), Some("/settings?notice=saved"));
    for page in ["/", "/newest", "/settings"] {
        assert!(w.get(page, &alice).await.body.contains(script), "{page}");
    }
    assert!(w.get("/settings", &alice).await.body.contains(
        r#"<input type="checkbox" name="on" value="1" checked> Turn on keyboard shortcuts"#
    ));
    let bob = w.user("bob.example", &base(), 100, 0).await;
    assert!(
        !w.get("/", &bob).await.body.contains(script),
        "one reader's choice"
    );
    assert!(!w.get("/", "").await.body.contains(script));

    let r = w.post("/settings/keys", &alice, &[]).await;
    assert_eq!(r.location.as_deref(), Some("/settings?notice=saved"));
    assert!(!w.get("/", &alice).await.body.contains(script));

    let signed_out = format!("csrf={}", w.csrf);
    let r = w.post("/settings/keys", &signed_out, &[("on", "1")]).await;
    assert_eq!(r.location.as_deref(), Some("/login?next=/settings"));
}

#[tokio::test]
async fn the_scripts_are_served() {
    let w = world().await;
    for path in ["/static/thread.js", "/static/keys.js"] {
        let (status, headers, body) = {
            let res = w
                .app
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            let status = res.status();
            let headers = res.headers().clone();
            let body = res.into_body().collect().await.unwrap().to_bytes();
            (status, headers, body)
        };
        assert_eq!(status, StatusCode::OK, "{path}");
        assert_eq!(
            headers[header::CONTENT_TYPE],
            "text/javascript; charset=utf-8"
        );
        assert!(body.starts_with(b"//"), "{path}");
    }
}
