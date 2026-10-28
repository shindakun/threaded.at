//! The replies inbox and the unread count in the header.

mod common;

use axum::http::StatusCode;
use common::world::*;
use serde_json::json;
use threaded_core::policy::Policy;
use threaded_indexer::apply::{apply, Collections, Settings};
use threaded_indexer::client::{Action, RecordEvent};

const HEADER_UNREAD_1: &str = r#">inbox (1<span class="vh"> unread</span>)</a>"#;

#[tokio::test]
async fn replies_from_the_web_and_the_network_reach_the_inbox() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let bob = w.user("bob.example", &base(), 100, 0).await;
    let id = w
        .submit(&alice, "Alice's story", "https://e.example/i", &["rust"])
        .await;
    assert!(w.get("/", &alice).await.body.contains(r#">inbox</a>"#));

    let r = w
        .post("/comment", &bob, &[("story", &id), ("text", "First!")])
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let page = w.get("/", &alice).await.body;
    assert!(page.contains(HEADER_UNREAD_1), "{page}");
    assert!(
        w.get("/", &bob).await.body.contains(r#">inbox</a>"#),
        "your own comment is not a reply to you"
    );

    // A reply written straight to a repo, read from the network.
    let story = w.store.story(&id).await.unwrap().unwrap();
    let ev = RecordEvent {
        live: true,
        did: "did:plc:carol".into(),
        rev: "3k".into(),
        collection: "at.threaded.dev.comment".into(),
        rkey: "3net".into(),
        action: Action::Create,
        record: Some(json!({
            "$type": "at.threaded.dev.comment",
            "subject": {"uri": story.at_uri.unwrap(), "cid": story.at_cid.unwrap()},
            "text": "From the network",
            "createdAt": "2026-09-01T00:00:00Z",
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
    assert!(w
        .get("/", &alice)
        .await
        .body
        .contains(r#">inbox (2<span class="vh"> unread</span>)</a>"#));

    let inbox = w.get("/inbox", &alice).await;
    assert_eq!(inbox.status, StatusCode::OK);
    assert!(inbox.body.contains("First!"));
    assert!(inbox.body.contains("From the network"));
    assert_eq!(
        inbox
            .body
            .matches(r#"<strong class="new">new</strong>"#)
            .count(),
        2
    );
    assert!(inbox.body.contains("commented on your story"));
    assert!(
        inbox.body.contains(r#">inbox</a>"#),
        "opening the inbox clears the count in its own header"
    );

    // Read now: the count is gone and nothing is marked new.
    assert!(w.get("/", &alice).await.body.contains(r#">inbox</a>"#));
    let again = w.get("/inbox", &alice).await.body;
    assert!(again.contains("First!"));
    assert!(!again.contains(r#"class="new""#));
}

#[tokio::test]
async fn a_reply_to_a_comment_names_it_and_links_both_ways() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let bob = w.user("bob.example", &base(), 100, 0).await;
    let id = w
        .submit(&bob, "Bob's story", "https://e.example/j", &["rust"])
        .await;
    let r = w
        .post("/comment", &alice, &[("story", &id), ("text", "A thought")])
        .await;
    let parent = r
        .location
        .unwrap()
        .rsplit("#c-")
        .next()
        .unwrap()
        .to_string();
    let r = w
        .post(
            "/comment",
            &bob,
            &[("story", &id), ("parent", &parent), ("text", "An answer")],
        )
        .await;
    let reply = r
        .location
        .unwrap()
        .rsplit("#c-")
        .next()
        .unwrap()
        .to_string();

    let inbox = w.get("/inbox", &alice).await.body;
    assert!(inbox.contains("replied to your comment on"), "{inbox}");
    assert!(inbox.contains(&format!(r#"href="/s/{id}#c-{reply}""#)));
    assert!(inbox.contains(&format!(r#"href="/c/{reply}/reply""#)));
    assert!(
        !inbox.contains("A thought"),
        "her own comment is not listed"
    );
}

#[tokio::test]
async fn the_inbox_needs_a_reader() {
    let w = world().await;
    let r = w.get("/inbox", "").await;
    assert_eq!(r.location.as_deref(), Some("/login?next=/inbox"));
    assert!(!w.get("/", "").await.body.contains("/inbox"));
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let empty = w.get("/inbox", &alice).await.body;
    assert!(empty.contains("No replies yet."));
}
