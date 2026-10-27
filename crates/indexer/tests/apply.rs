//! Applying stream events to an in-memory store, one event at a time.

use serde_json::json;
use threaded_core::policy::Policy;
use threaded_indexer::apply::{apply, item_time, Applied, Collections, Settings};
use threaded_indexer::client::{Action, RecordEvent};
use threaded_store::{By, Context, Source, Store, StoryInput, Target, VoteChange};

const NOW: i64 = 1_790_000_000;
const SUB: &str = "at.threaded.submission";
const COM: &str = "at.threaded.comment";
const VOTE: &str = "at.threaded.vote";

struct T {
    store: Store,
    cols: Collections,
    policy: Policy,
}

impl T {
    async fn new() -> T {
        let store = Store::open("sqlite::memory:").await.unwrap();
        for t in ["rust", "show", "ask"] {
            store.upsert_tag(t, "", 0.0).await.unwrap();
        }
        T {
            store,
            cols: Collections::under("at.threaded"),
            policy: Policy::default(),
        }
    }

    async fn apply_at(&self, ev: &RecordEvent, now: i64, triage: bool) -> Applied {
        let s = Settings {
            collections: &self.cols,
            policy: &self.policy,
            triage,
            now,
        };
        apply(&self.store, &s, ev).await.unwrap()
    }

    async fn apply(&self, ev: &RecordEvent) -> Applied {
        self.apply_at(ev, NOW, false).await
    }

    async fn count(&self, sql: &str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(self.store.pool())
            .await
            .unwrap()
    }
}

fn ev(
    did: &str,
    collection: &str,
    rkey: &str,
    action: Action,
    record: Option<serde_json::Value>,
    cid: &str,
    live: bool,
) -> RecordEvent {
    RecordEvent {
        live,
        did: did.into(),
        rev: "3k".into(),
        collection: collection.into(),
        rkey: rkey.into(),
        action,
        record,
        cid: (action != Action::Delete).then(|| cid.to_string()),
    }
}

fn story(did: &str, rkey: &str, title: &str, url: &str, live: bool) -> RecordEvent {
    ev(
        did,
        SUB,
        rkey,
        Action::Create,
        Some(
            json!({"$type": SUB, "title": title, "url": url, "tags": ["rust"], "authored": false,
                    "createdAt": "2026-09-01T00:00:00Z"}),
        ),
        &format!("cid-{rkey}"),
        live,
    )
}

fn uri(did: &str, collection: &str, rkey: &str) -> String {
    format!("at://{did}/{collection}/{rkey}")
}

fn comment(did: &str, rkey: &str, subject: &str, parent: Option<&str>, text: &str) -> RecordEvent {
    let mut r = json!({"$type": COM, "subject": {"uri": subject, "cid": "x"}, "text": text,
                       "createdAt": "2026-09-01T00:00:00Z"});
    if let Some(p) = parent {
        r["parent"] = json!({"uri": p, "cid": "x"});
    }
    ev(
        did,
        COM,
        rkey,
        Action::Create,
        Some(r),
        &format!("cid-{rkey}"),
        true,
    )
}

fn vote(did: &str, rkey: &str, subject: &str) -> RecordEvent {
    ev(
        did,
        VOTE,
        rkey,
        Action::Create,
        Some(
            json!({"$type": VOTE, "subject": {"uri": subject, "cid": "x"}, "createdAt": "2026-09-01T00:00:00Z"}),
        ),
        &format!("cid-{rkey}"),
        true,
    )
}

const ALICE: &str = "did:plc:alice";
const BOB: &str = "did:plc:bob";

#[tokio::test]
async fn a_network_story_is_indexed_once() {
    let t = T::new().await;
    let e = story(ALICE, "s1", "From another app", "https://e.example/a", true);
    let Applied::Indexed { short_id } = t.apply(&e).await else {
        panic!("indexed")
    };
    assert_eq!(
        t.apply(&e).await,
        Applied::Known,
        "a replay changes nothing"
    );

    let s = t.store.story(&short_id).await.unwrap().unwrap();
    assert_eq!((s.title.as_str(), s.created_at), ("From another app", NOW));
    assert_eq!(s.at_uri.as_deref(), Some(uri(ALICE, SUB, "s1").as_str()));
    let author = t.store.user_by_did(ALICE).await.unwrap().unwrap();
    assert!(
        !author.signed_in(),
        "a network author is not a signed-in account"
    );
    assert_eq!(
        author.name(),
        ALICE,
        "shown by DID until its handle verifies"
    );
}

#[tokio::test]
async fn threadeds_own_write_coming_back_is_known() {
    let t = T::new().await;
    let alice = t
        .store
        .record_sign_in(ALICE, "alice.example", true, NOW)
        .await
        .unwrap();
    let user = t.store.user_by_did(ALICE).await.unwrap().unwrap();
    assert_eq!(user.id, alice);
    let tags = vec!["rust".to_string()];
    t.store
        .accept_story(
            &user,
            &StoryInput {
                title: "Via the web",
                url: "https://e.example/w",
                text: "",
                tags: &tags,
                authored: false,
            },
            (&uri(ALICE, SUB, "w1"), "cid-w1"),
            &Context {
                policy: &t.policy,
                source: Source::Web,
                triage: false,
                now: NOW,
            },
        )
        .await
        .unwrap()
        .unwrap();
    let back = story(ALICE, "w1", "Via the web", "https://e.example/w", true);
    assert_eq!(t.apply(&back).await, Applied::Known);
    assert_eq!(t.count("SELECT count(*) FROM stories").await, 1);
}

#[tokio::test]
async fn bad_records_are_refused_with_a_reason() {
    let t = T::new().await;
    let mut wrong_type = story(ALICE, "s1", "T", "https://e.example/a", true);
    wrong_type.record.as_mut().unwrap()["$type"] = json!("app.bsky.feed.post");
    assert!(matches!(t.apply(&wrong_type).await, Applied::Refused(r) if r.contains("$type")));

    let http = story(ALICE, "s2", "T", "http://e.example/a", true);
    assert!(
        matches!(t.apply(&http).await, Applied::Refused(r) if r.contains("https")),
        "links must be https"
    );

    let mut no_tags = story(ALICE, "s3", "T", "https://e.example/b", true);
    no_tags.record.as_mut().unwrap()["tags"] = json!(["nope"]);
    assert_eq!(
        t.apply(&no_tags).await,
        Applied::Refused("Pick at least one tag.".into())
    );

    let mut malformed = story(ALICE, "s4", "T", "https://e.example/c", true);
    malformed.record.as_mut().unwrap()["title"] = json!(42);
    assert!(matches!(t.apply(&malformed).await, Applied::Refused(r) if r.contains("malformed")));
    assert_eq!(t.count("SELECT count(*) FROM stories").await, 0);
}

#[test]
fn live_items_count_from_arrival_and_backfill_from_created_at() {
    assert_eq!(
        item_time(true, "2026-01-01T00:00:00Z", NOW),
        NOW,
        "live ignores createdAt"
    );
    assert_eq!(item_time(false, "2026-09-21T14:13:00Z", NOW), NOW - 20);
    assert_eq!(
        item_time(false, "2099-01-01T00:00:00Z", NOW),
        NOW,
        "never later than now"
    );
    assert_eq!(item_time(false, "yesterday", NOW), NOW);
}

#[tokio::test]
async fn only_live_items_are_queued_for_triage() {
    let t = T::new().await;
    t.apply_at(
        &story(ALICE, "s1", "Old", "https://e.example/old", false),
        NOW,
        true,
    )
    .await;
    assert_eq!(
        t.count("SELECT count(*) FROM triage_queue").await,
        0,
        "backfill is not triaged"
    );
    t.apply_at(
        &story(ALICE, "s2", "New", "https://e.example/new", true),
        NOW,
        true,
    )
    .await;
    assert_eq!(t.count("SELECT count(*) FROM triage_queue").await, 1);
}

#[tokio::test]
async fn comments_wait_for_their_story_and_parent() {
    let t = T::new().await;
    let s_uri = uri(ALICE, SUB, "s1");
    let parent = comment(BOB, "c1", &s_uri, None, "First");
    assert_eq!(
        t.apply(&parent).await,
        Applied::Pending {
            waits_for: s_uri.clone()
        }
    );

    t.apply(&story(ALICE, "s1", "Story", "https://e.example/s", true))
        .await;
    let reply = comment(ALICE, "c2", &s_uri, Some(&uri(BOB, COM, "c1")), "A reply");
    assert_eq!(
        t.apply(&reply).await,
        Applied::Pending {
            waits_for: uri(BOB, COM, "c1")
        }
    );

    assert!(matches!(t.apply(&parent).await, Applied::Indexed { .. }));
    assert!(matches!(t.apply(&reply).await, Applied::Indexed { .. }));
    assert_eq!(t.apply(&reply).await, Applied::Known);
    let depth: i64 = t
        .count("SELECT depth FROM comments WHERE body = 'A reply'")
        .await;
    assert_eq!(depth, 1);
}

#[tokio::test]
async fn a_comment_on_a_merged_story_lands_on_the_story_it_merged_into() {
    let t = T::new().await;
    let Applied::Indexed { short_id: a } = t
        .apply(&story(ALICE, "a", "A", "https://e.example/a", true))
        .await
    else {
        panic!()
    };
    let Applied::Indexed { short_id: b } = t
        .apply(&story(ALICE, "b", "B", "https://e.example/b", true))
        .await
    else {
        panic!()
    };
    let a_id = t.store.story(&a).await.unwrap().unwrap().id;
    let b_id = t.store.story(&b).await.unwrap().unwrap().id;
    t.store
        .merge(a_id, b_id, By::Console, "Same article", NOW)
        .await
        .unwrap()
        .unwrap();
    t.apply(&comment(BOB, "c1", &uri(ALICE, SUB, "a"), None, "On A"))
        .await;
    assert_eq!(t.store.story(&b).await.unwrap().unwrap().comment_count, 1);
}

#[tokio::test]
async fn votes_count_only_from_accounts_that_signed_in_here() {
    let t = T::new().await;
    let Applied::Indexed { short_id } = t
        .apply(&story(ALICE, "s1", "S", "https://e.example/s", true))
        .await
    else {
        panic!()
    };
    let s_uri = uri(ALICE, SUB, "s1");

    assert_eq!(
        t.apply(&vote(BOB, "v1", &s_uri)).await,
        Applied::Ignored("the voter has not signed in here")
    );
    t.store
        .record_sign_in(BOB, "bob.example", true, NOW)
        .await
        .unwrap();
    assert_eq!(t.apply(&vote(BOB, "v1", &s_uri)).await, Applied::Voted);
    assert_eq!(t.apply(&vote(BOB, "v1", &s_uri)).await, Applied::Known);
    assert_eq!(t.store.story(&short_id).await.unwrap().unwrap().score, 2);

    // A second record for the same subject does not count twice.
    assert_eq!(
        t.apply(&vote(BOB, "v2", &s_uri)).await,
        Applied::Ignored("already upvoted by another record")
    );
    // Deleting the record removes the upvote.
    let del = ev(BOB, VOTE, "v1", Action::Delete, None, "", true);
    assert_eq!(t.apply(&del).await, Applied::Unvoted);
    assert_eq!(t.store.story(&short_id).await.unwrap().unwrap().score, 1);

    // A vote record for an upvote made on the web attaches to it.
    let bob = t.store.user_by_did(BOB).await.unwrap().unwrap();
    let id = t.store.story(&short_id).await.unwrap().unwrap().id;
    t.store
        .set_vote(
            bob.id,
            Target::Story(id),
            VoteChange::Up { at_uri: None },
            NOW,
        )
        .await
        .unwrap();
    assert_eq!(t.apply(&vote(BOB, "v3", &s_uri)).await, Applied::Voted);
    assert_eq!(
        t.store.story(&short_id).await.unwrap().unwrap().score,
        2,
        "still one vote"
    );
    assert_eq!(
        t.count("SELECT count(*) FROM votes WHERE at_uri IS NOT NULL")
            .await,
        1
    );

    // Votes on one's own post do not count.
    t.store
        .record_sign_in(ALICE, "alice.example", true, NOW)
        .await
        .unwrap();
    assert_eq!(
        t.apply(&vote(ALICE, "v4", &s_uri)).await,
        Applied::Ignored("a vote on one's own post")
    );
}

#[tokio::test]
async fn edits_apply_and_a_late_link_change_detaches() {
    let t = T::new().await;
    let Applied::Indexed { short_id } = t
        .apply(&story(
            ALICE,
            "s1",
            "Old title",
            "https://e.example/s",
            true,
        ))
        .await
    else {
        panic!()
    };
    let mut same = story(ALICE, "s1", "Old title", "https://e.example/s", true);
    same.action = Action::Update;
    assert_eq!(
        t.apply(&same).await,
        Applied::Known,
        "an update with the stored CID changes nothing"
    );

    let mut edit = story(ALICE, "s1", "New title", "https://e.example/s", true);
    edit.action = Action::Update;
    edit.cid = Some("cid-2".into());
    assert_eq!(t.apply(&edit).await, Applied::Edited);
    assert_eq!(
        t.store.story(&short_id).await.unwrap().unwrap().title,
        "New title"
    );

    // Two hours later the link changes: detached and hidden.
    let mut late = story(ALICE, "s1", "New title", "https://elsewhere.example/", true);
    late.action = Action::Update;
    late.cid = Some("cid-3".into());
    assert_eq!(
        t.apply_at(&late, NOW + 7_200, false).await,
        Applied::Detached
    );
    assert!(
        t.store.story(&short_id).await.unwrap().is_none(),
        "hidden from readers"
    );
    let any = t.store.story_any(&short_id).await.unwrap().unwrap();
    assert!(any.detached_at.is_some());
    assert_eq!(
        t.count("SELECT count(*) FROM story_revisions").await,
        2,
        "every version is kept"
    );
}

#[tokio::test]
async fn deletes_remove_and_comments_with_replies_stay_as_placeholders() {
    let t = T::new().await;
    let Applied::Indexed { short_id } = t
        .apply(&story(ALICE, "s1", "S", "https://e.example/s", true))
        .await
    else {
        panic!()
    };
    let s_uri = uri(ALICE, SUB, "s1");
    t.apply(&comment(BOB, "c1", &s_uri, None, "Parent")).await;
    t.apply(&comment(
        ALICE,
        "c2",
        &s_uri,
        Some(&uri(BOB, COM, "c1")),
        "Child",
    ))
    .await;

    let mut edit = comment(BOB, "c1", &s_uri, None, "Parent, edited");
    edit.action = Action::Update;
    edit.cid = Some("cid-c1b".into());
    assert_eq!(t.apply(&edit).await, Applied::Edited);

    assert_eq!(
        t.apply(&ev(BOB, COM, "c1", Action::Delete, None, "", true))
            .await,
        Applied::Deleted
    );
    assert_eq!(
        t.apply(&ev(BOB, COM, "c1", Action::Delete, None, "", true))
            .await,
        Applied::Ignored("the comment is not indexed")
    );
    let deleted: i64 = t
        .count("SELECT count(*) FROM comments WHERE deleted_at IS NOT NULL")
        .await;
    assert_eq!(deleted, 1);

    assert_eq!(
        t.apply(&ev(ALICE, SUB, "s1", Action::Delete, None, "", true))
            .await,
        Applied::Deleted
    );
    assert!(t.store.story(&short_id).await.unwrap().is_none());
    assert_eq!(
        t.apply(&ev(ALICE, SUB, "s1", Action::Delete, None, "", true))
            .await,
        Applied::Ignored("the story is not indexed")
    );
}

#[tokio::test]
async fn other_collections_and_dev_records_are_ignored_in_production() {
    let t = T::new().await;
    let mut dev = story(ALICE, "s1", "Dev", "https://e.example/d", true);
    dev.collection = "at.threaded.dev.submission".into();
    dev.record.as_mut().unwrap()["$type"] = json!("at.threaded.dev.submission");
    assert_eq!(
        t.apply(&dev).await,
        Applied::Ignored("not a Threaded collection")
    );
}

#[tokio::test]
async fn network_accounts_get_new_user_limits() {
    let t = T::new().await;
    for n in 0..2 {
        let e = story(
            ALICE,
            &format!("s{n}"),
            "S",
            &format!("https://e.example/{n}"),
            true,
        );
        assert!(matches!(t.apply(&e).await, Applied::Indexed { .. }));
    }
    let third = story(ALICE, "s9", "S", "https://e.example/9", true);
    assert_eq!(
        t.apply(&third).await,
        Applied::Refused("You can submit 2 stories a day. Try again tomorrow.".into())
    );
}
