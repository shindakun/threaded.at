//! Bluesky blocks: while signed in, two accounts in a block do not see each
//! other's stories or comments and cannot reply to each other. Everyone
//! else, signed-out readers, and moderation pages see everything.

mod common;

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use common::world::*;
use threaded_store::Block;
use threaded_web::blocks::BlockReader;
use threaded_web::write::BLOCKED_COMMENT;

/// `blocker` blocks `subject`, as if read from the blocker's repo.
async fn block(w: &World, blocker: &str, subject: &str) {
    w.store
        .replace_blocks(
            &format!("did:plc:{blocker}"),
            &[Block {
                rkey: format!("3{subject}"),
                subject_did: format!("did:plc:{subject}"),
            }],
            NOW,
        )
        .await
        .unwrap();
}

/// The short ID of the comment a comment form's redirect names.
fn comment_id(r: &Res) -> String {
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    r.location
        .as_deref()
        .unwrap()
        .split("#c-")
        .nth(1)
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn a_block_hides_stories_both_ways_and_only_between_the_two() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let bob = w.user("bob.example", &base(), 100, 0).await;
    let carol = w.user("carol.example", &base(), 100, 0).await;
    let a = w
        .submit(&alice, "Alice story", "https://e.example/a", &["rust"])
        .await;
    let b = w
        .submit(&bob, "Bob story", "https://e.example/b", &["rust"])
        .await;
    block(&w, "alice", "bob").await;

    for path in ["/", "/newest", "/t/rust", "/search?q=story"] {
        let to_alice = w.get(path, &alice).await.body;
        let to_bob = w.get(path, &bob).await.body;
        assert!(!to_alice.contains("Bob story"), "{path}: the blocker");
        assert!(!to_bob.contains("Alice story"), "{path}: the one blocked");
        for page in [w.get(path, &carol).await.body, w.get(path, "").await.body] {
            assert!(
                page.contains("Alice story") && page.contains("Bob story"),
                "{path}"
            );
        }
    }

    let r = w.get(&format!("/s/{a}"), &bob).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(
        r.body.contains("in a block with you on Bluesky"),
        "{}",
        r.body
    );
    assert!(!r.body.contains("Alice story"));
    assert_eq!(
        w.get(&format!("/s/{b}"), &alice).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        w.get(&format!("/s/{a}"), &carol).await.status,
        StatusCode::OK
    );
    assert_eq!(w.get(&format!("/s/{a}"), "").await.status, StatusCode::OK);
    assert_eq!(
        w.get(&format!("/s/{a}"), &alice).await.status,
        StatusCode::OK
    );

    let r = w.get("/u/alice.example", &bob).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(
        r.body.contains("This account is in a block with you"),
        "{}",
        r.body
    );
    assert_eq!(
        w.get("/u/bob.example", &alice).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        w.get("/u/alice.example", &carol).await.status,
        StatusCode::OK
    );
    assert_eq!(
        w.get("/u/alice.example", &alice).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_comment_in_a_block_is_a_placeholder_and_replies_by_others_stay() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let bob = w.user("bob.example", &base(), 100, 0).await;
    let carol = w.user("carol.example", &base(), 100, 0).await;
    let s = w
        .submit(&carol, "Carol story", "https://e.example/c", &["rust"])
        .await;
    let by_bob = comment_id(
        &w.post("/comment", &bob, &[("story", &s), ("text", "Bob was here")])
            .await,
    );
    w.post(
        "/comment",
        &carol,
        &[
            ("story", &s),
            ("parent", &by_bob),
            ("text", "Carol answers Bob"),
        ],
    )
    .await;
    w.post(
        "/comment",
        &alice,
        &[("story", &s), ("text", "Alice was here")],
    )
    .await;
    block(&w, "bob", "alice").await;

    let to_alice = w.get(&format!("/s/{s}"), &alice).await.body;
    assert!(to_alice.contains(BLOCKED_COMMENT), "{to_alice}");
    assert!(!to_alice.contains("Bob was here"));
    assert!(!to_alice.contains("bob.example"), "not even in a label");
    assert!(
        !to_alice.contains(&format!("/c/{by_bob}/reply")),
        "no reply link on the placeholder"
    );
    assert!(
        to_alice.contains("Carol answers Bob"),
        "a reply by another stays"
    );
    assert!(to_alice.contains("Alice was here"));

    let to_bob = w.get(&format!("/s/{s}"), &bob).await.body;
    assert!(to_bob.contains(BLOCKED_COMMENT), "both ways");
    assert!(!to_bob.contains("Alice was here"));
    assert!(to_bob.contains("Bob was here"));

    let to_carol = w.get(&format!("/s/{s}"), &carol).await.body;
    assert!(!to_carol.contains(BLOCKED_COMMENT));
    assert!(to_carol.contains("Bob was here") && to_carol.contains("Alice was here"));
}

#[tokio::test]
async fn replies_between_two_accounts_in_a_block_are_refused_and_not_written() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let bob = w.user("bob.example", &base(), 100, 0).await;
    let carol = w.user("carol.example", &base(), 100, 0).await;
    let a = w
        .submit(&alice, "Alice story", "https://e.example/a", &["rust"])
        .await;
    let c = w
        .submit(&carol, "Carol story", "https://e.example/c", &["rust"])
        .await;
    let by_alice = comment_id(
        &w.post(
            "/comment",
            &alice,
            &[("story", &c), ("text", "Alice on Carol")],
        )
        .await,
    );
    block(&w, "alice", "bob").await;
    let before = w.writes().len();

    let r = w
        .post(
            "/comment",
            &bob,
            &[("story", &a), ("text", "To Alice story")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(
        r.body
            .contains("you and its author are in a block on Bluesky"),
        "{}",
        r.body
    );
    let r = w
        .post(
            "/comment",
            &bob,
            &[
                ("story", &c),
                ("parent", &by_alice),
                ("text", "To the Alice comment"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(
        w.get(&format!("/c/{by_alice}/reply"), &bob).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(w.writes().len(), before, "nothing was written to a repo");

    // The block does not reach a story or a comment by anyone else.
    let r = w
        .post("/comment", &bob, &[("story", &c), ("text", "To Carol")])
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    assert_eq!(
        w.get(&format!("/c/{by_alice}/reply"), &carol).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn moderation_pages_ignore_blocks() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let moderator = w.user("mod.example", &base(), 100, 0).await;
    w.role("mod.example", "moderator").await;
    let a = w
        .submit(&alice, "Alice story", "https://e.example/a", &["rust"])
        .await;
    block(&w, "alice", "mod").await;

    assert_eq!(
        w.get(&format!("/s/{a}"), &moderator).await.status,
        StatusCode::FORBIDDEN
    );
    let r = w.get(&format!("/mod/s/{a}"), &moderator).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("Alice story"));
    let r = w.get("/mod/u/did:plc:alice", &moderator).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("alice.example"));
}

#[tokio::test]
async fn replies_from_an_account_in_a_block_leave_the_inbox() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let bob = w.user("bob.example", &base(), 100, 0).await;
    let a = w
        .submit(&alice, "Alice story", "https://e.example/a", &["rust"])
        .await;
    w.post(
        "/comment",
        &bob,
        &[("story", &a), ("text", "Before the block")],
    )
    .await;
    assert!(w
        .get("/inbox", &alice)
        .await
        .body
        .contains("Before the block"));
    block(&w, "bob", "alice").await;
    let inbox = w.get("/inbox", &alice).await.body;
    assert!(!inbox.contains("Before the block"), "{inbox}");
}

#[tokio::test]
async fn settings_read_the_blocks_again_and_keep_them_when_a_read_fails() {
    let fixed: threaded_web::blocks::FixedBlocks = Arc::new(Mutex::new(Default::default()));
    let reader = fixed.clone();
    let w = world_custom(NOW, None, move |s| s.blocks = BlockReader::Fixed(reader)).await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    w.user("bob.example", &base(), 100, 0).await;

    let page = w.get("/settings", &alice).await.body;
    assert!(
        page.contains("Your blocks have not been read yet."),
        "{page}"
    );

    fixed.lock().unwrap().insert(
        "did:plc:alice".into(),
        Ok(vec![Block {
            rkey: "3kb".into(),
            subject_did: "did:plc:bob".into(),
        }]),
    );
    let r = w.post("/settings/blocks", &alice, &[]).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(
        r.location.as_deref(),
        Some("/settings?notice=blocks#blocks")
    );
    let page = w.get("/settings?notice=blocks", &alice).await.body;
    assert!(page.contains("Your blocks were read again."), "{page}");
    assert!(page.contains("1 block, read just now."), "{page}");

    fixed
        .lock()
        .unwrap()
        .insert("did:plc:alice".into(), Err("the PDS answered 500".into()));
    let r = w.post("/settings/blocks", &alice, &[]).await;
    assert_eq!(
        r.location.as_deref(),
        Some("/settings?notice=blocks-failed#blocks")
    );
    let page = w.get("/settings?notice=blocks-failed", &alice).await.body;
    assert!(page.contains("Your blocks could not be read"), "{page}");
    assert!(
        page.contains("1 block, read just now."),
        "the blocks read before stay"
    );
    assert_eq!(
        w.store.block_count("did:plc:alice").await.unwrap(),
        1,
        "a failed read keeps what was held"
    );
}

#[tokio::test]
async fn a_new_read_replaces_the_blocks_held() {
    let w = world().await;
    w.user("alice.example", &base(), 100, 0).await;
    block(&w, "alice", "bob").await;
    w.store
        .replace_blocks(
            "did:plc:alice",
            &[Block {
                rkey: "3new".into(),
                subject_did: "did:plc:carol".into(),
            }],
            NOW + 60,
        )
        .await
        .unwrap();
    let held: Vec<(String, String)> =
        sqlx::query_as("SELECT rkey, subject_did FROM blocks WHERE blocker_did = 'did:plc:alice'")
            .fetch_all(w.store.pool())
            .await
            .unwrap();
    assert_eq!(
        held,
        vec![("3new".to_string(), "did:plc:carol".to_string())]
    );
}
