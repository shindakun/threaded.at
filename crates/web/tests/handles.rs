mod common;

use std::time::Duration;

use common::world::*;
use threaded_store::{CheckedHandle, HANDLE_CHECK_SECS, UNVERIFIED_CHECK_SECS};

async fn handle_of(w: &World, did: &str) -> (String, bool, i64) {
    sqlx::query_as("SELECT handle, handle_verified, handle_checked_at FROM users WHERE did = ?")
        .bind(did)
        .fetch_one(w.store.pool())
        .await
        .unwrap()
}

async fn checked_at(w: &World, did: &str, at: i64) {
    sqlx::query("UPDATE users SET handle_checked_at = ? WHERE did = ?")
        .bind(at)
        .bind(did)
        .execute(w.store.pool())
        .await
        .unwrap();
}

/// Waits for a background check to land.
async fn until(
    w: &World,
    did: &str,
    want: impl Fn(&(String, bool, i64)) -> bool,
) -> (String, bool, i64) {
    for _ in 0..100 {
        let h = handle_of(w, did).await;
        if want(&h) {
            return h;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    handle_of(w, did).await
}

#[tokio::test]
async fn a_changed_handle_shows_after_the_daily_check() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    w.handles.lock().unwrap().insert(
        "did:plc:alice".into(),
        CheckedHandle::Verified("alice.new.example".into()),
    );

    // Checked at sign-in, a moment ago: not due, so nothing changes.
    w.get("/", &alice).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(handle_of(&w, "did:plc:alice").await.0, "alice.example");

    // A day later the next page starts a check in the background.
    checked_at(&w, "did:plc:alice", NOW - HANDLE_CHECK_SECS).await;
    w.get("/", &alice).await;
    let h = until(&w, "did:plc:alice", |h| h.0 == "alice.new.example").await;
    assert_eq!(h, ("alice.new.example".to_string(), true, NOW));
    let page = w.get("/", &alice).await.body;
    assert!(
        page.contains(">alice.new.example</a>"),
        "the header shows the new handle"
    );
}

#[tokio::test]
async fn authors_on_a_page_are_checked_and_a_taken_handle_moves() {
    let w = world().await;
    let bob = w.user("bob.example", &base(), 100, 0).await;
    w.submit(&bob, "Bob's story", "https://e.example/b", &["rust"])
        .await;
    let carol = w.user("carol.example", &base(), 100, 0).await;
    w.submit(&carol, "Carol's story", "https://e.example/c", &["rust"])
        .await;

    // Carol's account now holds bob.example; a reader's front page shows her story.
    w.handles.lock().unwrap().insert(
        "did:plc:carol".into(),
        CheckedHandle::Verified("bob.example".into()),
    );
    checked_at(&w, "did:plc:carol", NOW - HANDLE_CHECK_SECS).await;
    w.get("/", "").await;
    until(&w, "did:plc:carol", |h| h.0 == "bob.example").await;

    let bob_now = handle_of(&w, "did:plc:bob").await;
    assert!(!bob_now.1, "the old holder loses the verification");
    let page = w.get("/", "").await.body;
    assert!(
        page.contains(r#"href="/u/did:plc:bob">did:plc:bob</a>"#),
        "{page}"
    );
    assert!(
        page.contains(r#"href="/u/bob.example">bob.example</a>"#),
        "Carol's story shows her new handle"
    );
}

#[tokio::test]
async fn a_failed_lookup_keeps_the_handle_and_retries_in_an_hour() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    checked_at(&w, "did:plc:alice", NOW - HANDLE_CHECK_SECS).await;
    // No answer set: the lookup fails.
    w.get("/", &alice).await;
    let h = until(&w, "did:plc:alice", |h| {
        h.2 != NOW && h.2 != NOW - HANDLE_CHECK_SECS
    })
    .await;
    assert_eq!(
        h,
        (
            "alice.example".to_string(),
            true,
            NOW - HANDLE_CHECK_SECS + UNVERIFIED_CHECK_SECS
        ),
        "kept, and due again an hour from now"
    );
}

#[tokio::test]
async fn a_handle_that_stops_verifying_shows_the_did_and_is_checked_hourly() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    w.handles
        .lock()
        .unwrap()
        .insert("did:plc:alice".into(), CheckedHandle::Unverified);
    checked_at(&w, "did:plc:alice", NOW - HANDLE_CHECK_SECS).await;
    w.get("/", &alice).await;
    let h = until(&w, "did:plc:alice", |h| !h.1).await;
    assert_eq!((h.1, h.2), (false, NOW));
    assert!(w.get("/", &alice).await.body.contains(">did:plc:alice</a>"));

    // Unverified accounts are due again after an hour, not a day.
    let due = w
        .store
        .handle_checks_due(&[1, 2, 3], NOW + UNVERIFIED_CHECK_SECS)
        .await
        .unwrap();
    assert!(due.iter().any(|(_, d)| d == "did:plc:alice"));
}
