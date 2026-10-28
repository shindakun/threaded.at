//! Bluesky cross-posting: the setting and its scope, the post, a failed
//! post and trying again, and where it is never offered.

mod common;

use axum::http::StatusCode;
use common::world::*;
use serde_json::json;
use threaded_core::policy::Policy;
use threaded_indexer::apply::{apply, Collections, Settings};
use threaded_indexer::client::{Action, RecordEvent};

const OFFER: &str = r#"<input type="checkbox" name="bsky" value="1""#;

/// A user who turned cross-posting on, with the scope for it.
async fn poster(w: &World, handle: &str) -> String {
    let who = w.user(handle, &bsky(), 100, 0).await;
    let r = w.post("/settings/bsky", &who, &[("on", "1")]).await;
    assert_eq!(
        r.location.as_deref(),
        Some("/settings?notice=saved"),
        "{}",
        r.body
    );
    who
}

fn bsky_posts(w: &World) -> Vec<serde_json::Value> {
    w.writes()
        .into_iter()
        .filter(|x| x.collection == "app.bsky.feed.post")
        .map(|x| x.record.unwrap())
        .collect()
}

#[tokio::test]
async fn settings_ask_for_the_scope_then_turn_it_on() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let page = w.get("/settings", &alice).await.body;
    assert!(page.contains("<h2>Cross-posting to Bluesky</h2>"));
    assert!(
        page.contains(r#"<input type="hidden" name="flow" value="bsky">"#),
        "the grant keeps the base scopes and adds Bluesky posts"
    );
    assert!(page.contains(r#"<input type="hidden" name="flow" value="votes">"#));
    let r = w.post("/settings/bsky", &alice, &[("on", "1")]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r
        .body
        .contains("Grant Threaded permission to post to Bluesky first."));
    assert!(!w.get("/submit", &alice).await.body.contains(OFFER));

    // Granting one optional scope keeps the other.
    let both = w.user("bob.example", &votes(), 100, 0).await;
    assert!(w
        .get("/settings", &both)
        .await
        .body
        .contains(r#"<input type="hidden" name="flow" value="votes-bsky">"#));
    let carol = w.user("carol.example", &bsky(), 100, 0).await;
    let page = w.get("/settings", &carol).await.body;
    assert!(page.contains(r#"<input type="hidden" name="flow" value="votes-bsky">"#));
    assert!(page.contains(
        r#"<input type="checkbox" name="on" value="1"> Offer to post my stories to Bluesky"#
    ));
    assert!(
        !w.get("/submit", &carol).await.body.contains(OFFER),
        "the scope alone does not turn it on"
    );

    let r = w.post("/settings/bsky", &carol, &[("on", "1")]).await;
    assert_eq!(r.location.as_deref(), Some("/settings?notice=saved"));
    assert!(w.get("/settings", &carol).await.body.contains(
        r#"<input type="checkbox" name="on" value="1" checked> Offer to post my stories to Bluesky"#
    ));
    let submit = w.get("/submit", &carol).await.body;
    assert!(submit.contains(
        r#"<input type="checkbox" name="bsky" value="1" aria-describedby="bsky-hint"> Also post to Bluesky"#
    ), "offered and unticked");

    w.post("/settings/bsky", &carol, &[]).await;
    assert!(!w.get("/submit", &carol).await.body.contains(OFFER));
    assert!(w.writes().is_empty(), "settings write no records");
}

#[tokio::test]
async fn a_ticked_story_is_posted_with_a_card_to_its_page() {
    let w = world().await;
    let alice = poster(&w, "alice.example").await;
    let r = w
        .post(
            "/submit",
            &alice,
            &[
                ("title", "Fish & chips"),
                ("url", "https://e.example/f"),
                ("tags", "rust"),
                ("bsky", "1"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    let id = r.location.unwrap().trim_start_matches("/s/").to_string();

    let writes = w.writes();
    assert_eq!(writes.len(), 2);
    assert_eq!(
        writes[0].collection, "at.threaded.dev.submission",
        "the story first"
    );
    let post = &writes[1];
    assert_eq!(post.collection, "app.bsky.feed.post");
    assert_eq!(post.did, "did:plc:alice");
    let rec = post.record.as_ref().unwrap();
    assert_eq!(rec["$type"], "app.bsky.feed.post");
    assert_eq!(rec["text"], "Fish & chips");
    assert_eq!(rec["createdAt"], "2026-09-21T14:13:20Z");
    assert_eq!(
        rec["embed"],
        json!({
            "$type": "app.bsky.embed.external",
            "external": {
                "uri": format!("https://threaded.test/s/{id}"),
                "title": "Fish & chips",
                "description": "A link to e.example, on Threaded.",
            },
        })
    );

    let link = format!(
        r#"<a href="https://bsky.app/profile/did:plc:alice/post/{}" target="_blank" rel="noopener">On Bluesky"#,
        post.rkey
    );
    assert!(w
        .get(&format!("/s/{id}"), &alice)
        .await
        .body
        .contains(&link));
    assert!(w.get(&format!("/s/{id}"), "").await.body.contains(&link));

    // Trying again once posted does nothing: a story is posted once.
    let r = w.post(&format!("/s/{id}/bsky"), &alice, &[]).await;
    assert_eq!(r.location, Some(format!("/s/{id}")));
    assert_eq!(bsky_posts(&w).len(), 1);

    // Unticked, nothing goes to Bluesky.
    w.submit(&alice, "Quiet", "https://e.example/q", &["rust"])
        .await;
    assert_eq!(bsky_posts(&w).len(), 1);
}

#[tokio::test]
async fn a_text_posts_card_starts_with_its_text() {
    let w = world().await;
    let alice = poster(&w, "alice.example").await;
    w.post(
        "/submit",
        &alice,
        &[
            ("title", "Ask: why?"),
            ("url", ""),
            ("text", "Is *this* the way?"),
            ("tags", "ask"),
            ("bsky", "1"),
        ],
    )
    .await;
    let posts = bsky_posts(&w);
    assert_eq!(
        posts[0]["embed"]["external"]["description"],
        "Is this the way?"
    );
}

#[tokio::test]
async fn a_failed_post_keeps_the_story_and_can_be_tried_again() {
    let w = world().await;
    let alice = poster(&w, "alice.example").await;
    let bob = w.user("bob.example", &base(), 100, 0).await;
    *w.repo.fail_collection.lock().unwrap() =
        Some(("app.bsky.feed.post".into(), "rate limited".into()));
    let r = w
        .post(
            "/submit",
            &alice,
            &[
                ("title", "Story"),
                ("url", "https://e.example/s"),
                ("tags", "rust"),
                ("bsky", "1"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let id = r.location.unwrap().trim_start_matches("/s/").to_string();
    assert!(
        w.get("/", "").await.body.contains("Story"),
        "the story stands"
    );
    assert!(bsky_posts(&w).is_empty());

    let page = w.get(&format!("/s/{id}"), &alice).await.body;
    assert!(
        page.contains("<p>Not posted to Bluesky. Your server refused the post: rate limited</p>"),
        "{page}"
    );
    assert!(page.contains(&format!(r#"<form method="post" action="/s/{id}/bsky">"#)));
    for other in [&bob, ""] {
        let body = w.get(&format!("/s/{id}"), other).await.body;
        assert!(
            !body.contains("Not posted to Bluesky"),
            "only the author sees it"
        );
        assert!(!body.contains("On Bluesky"));
    }

    let r = w.post(&format!("/s/{id}/bsky"), &bob, &[]).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = w
        .post(&format!("/s/{id}/bsky"), &format!("csrf={}", w.csrf), &[])
        .await;
    assert_eq!(r.location, Some(format!("/login?next=/s/{id}")));

    *w.repo.fail_collection.lock().unwrap() = None;
    let r = w.post(&format!("/s/{id}/bsky"), &alice, &[]).await;
    assert_eq!(r.location, Some(format!("/s/{id}")));
    assert_eq!(bsky_posts(&w).len(), 1);
    let page = w.get(&format!("/s/{id}"), &alice).await.body;
    assert!(!page.contains("Not posted to Bluesky"));
    assert!(page.contains("On Bluesky"));
}

#[tokio::test]
async fn a_story_readers_cannot_see_is_not_posted() {
    let w = world().await;
    let alice = poster(&w, "alice.example").await;
    let id = w
        .submit(&alice, "Story", "https://e.example/h", &["rust"])
        .await;
    sqlx::query("UPDATE stories SET removed_at = 1 WHERE short_id = ?")
        .bind(&id)
        .execute(w.store.pool())
        .await
        .unwrap();
    w.post(&format!("/s/{id}/bsky"), &alice, &[]).await;
    assert!(bsky_posts(&w).is_empty());
    assert!(w
        .get(&format!("/s/{id}"), &alice)
        .await
        .body
        .contains("Not posted to Bluesky. The story is not visible to readers yet"));
}

#[tokio::test]
async fn a_lost_scope_is_reported_not_attempted() {
    let w = world().await;
    let alice = poster(&w, "alice.example").await;
    let id = w
        .submit(&alice, "Story", "https://e.example/l", &["rust"])
        .await;
    // The same account, signed in again without the Bluesky scope.
    let again = w.user("alice.example", &base(), 100, 0).await;
    w.post(&format!("/s/{id}/bsky"), &again, &[]).await;
    assert!(bsky_posts(&w).is_empty());
    assert!(w
        .get(&format!("/s/{id}"), &again)
        .await
        .body
        .contains("Threaded does not have permission to post to Bluesky for you."));
}

#[tokio::test]
async fn never_offered_where_turned_off_or_unavailable() {
    // Turned off: a form that still says bsky=1 posts nothing.
    let w = world().await;
    let alice = w.user("alice.example", &bsky(), 100, 0).await;
    w.post(
        "/submit",
        &alice,
        &[
            ("title", "Story"),
            ("url", "https://e.example/o"),
            ("tags", "rust"),
            ("bsky", "1"),
        ],
    )
    .await;
    assert!(bsky_posts(&w).is_empty());

    // A build that does not offer it, such as a local one.
    let w = world_custom(NOW, None, |s| s.bsky = false).await;
    let alice = w.user("alice.example", &bsky(), 100, 0).await;
    sqlx::query("UPDATE users SET cross_post = 1")
        .execute(w.store.pool())
        .await
        .unwrap();
    assert!(!w
        .get("/settings", &alice)
        .await
        .body
        .contains("Cross-posting to Bluesky"));
    assert!(!w.get("/submit", &alice).await.body.contains(OFFER));
    let r = w.post("/settings/bsky", &alice, &[("on", "1")]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    w.post(
        "/submit",
        &alice,
        &[
            ("title", "Story"),
            ("url", "https://e.example/o"),
            ("tags", "rust"),
            ("bsky", "1"),
        ],
    )
    .await;
    assert!(bsky_posts(&w).is_empty());
}

#[tokio::test]
async fn stories_from_the_network_are_never_cross_posted() {
    let w = world().await;
    poster(&w, "alice.example").await;
    let ev = RecordEvent {
        live: true,
        did: "did:plc:alice".into(),
        rev: "3k".into(),
        collection: "at.threaded.dev.submission".into(),
        rkey: "3net".into(),
        action: Action::Create,
        record: Some(json!({
            "$type": "at.threaded.dev.submission",
            "title": "From another app",
            "url": "https://e.example/n",
            "tags": ["rust"],
            "createdAt": "2026-09-21T14:00:00Z",
        })),
        cid: Some("cid-net".into()),
    };
    let cols = Collections::under("at.threaded.dev");
    let policy = Policy::default();
    let settings = Settings {
        collections: &cols,
        policy: &policy,
        triage: false,
        now: NOW,
    };
    apply(&w.store, &settings, &ev).await.unwrap();
    assert!(w.get("/newest", "").await.body.contains("From another app"));
    assert!(w.writes().is_empty());
}
