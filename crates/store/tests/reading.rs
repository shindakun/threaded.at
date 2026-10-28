//! The replies inbox and read ribbons: what is new for a reader.

use threaded_store::{NewComment, NewStory, Store};

const T0: i64 = 1_790_000_000;

async fn story(s: &Store, user_id: i64, title: &str) -> i64 {
    let short = s
        .insert_story(&NewStory {
            user_id,
            title,
            link: None,
            body: "text",
            user_is_author: false,
            tags: &["rust"],
            created_at: T0,
            record: None,
        })
        .await
        .unwrap();
    s.story(&short).await.unwrap().unwrap().id
}

async fn comment(s: &Store, story_id: i64, user_id: i64, parent: Option<i64>, at: i64) -> i64 {
    let short = s
        .insert_comment(&NewComment {
            story_id,
            user_id,
            parent_id: parent,
            body: "words",
            created_at: at,
            record: None,
        })
        .await
        .unwrap();
    s.comment(&short).await.unwrap().unwrap().id
}

fn ids(rows: &[threaded_store::CommentRow]) -> Vec<i64> {
    rows.iter().map(|c| c.id).collect()
}

#[tokio::test]
async fn the_inbox_holds_replies_to_the_reader_and_nothing_else() {
    let s = Store::open("sqlite::memory:").await.unwrap();
    s.upsert_tag("rust", "", 0.0).await.unwrap();
    let mut user = Vec::new();
    for n in ["alice", "bob", "carol"] {
        user.push(
            s.record_sign_in(&format!("did:plc:{n}"), &format!("{n}.example"), true, T0)
                .await
                .unwrap(),
        );
    }
    let (alice, bob, carol) = (user[0], user[1], user[2]);
    let hers = story(&s, alice, "Alice's story").await;
    let his = story(&s, bob, "Bob's story").await;

    let on_her_story = comment(&s, hers, bob, None, T0 + 10).await;
    let her_comment = comment(&s, his, alice, None, T0 + 20).await;
    let reply_to_her = comment(&s, his, carol, Some(her_comment), T0 + 30).await;
    // Not for alice: her own comments, and a reply to someone else.
    comment(&s, hers, alice, None, T0 + 40).await;
    comment(&s, hers, alice, Some(on_her_story), T0 + 41).await;
    comment(&s, his, bob, Some(reply_to_her), T0 + 42).await;
    comment(&s, his, carol, None, T0 + 43).await;

    let got = s.replies(alice, 10, 0).await.unwrap();
    assert_eq!(ids(&got), [reply_to_her, on_her_story], "newest first");
    assert_eq!(s.replies(alice, 1, 1).await.unwrap()[0].id, on_her_story);

    // Deleted, removed, held, and inactive authors' replies leave it.
    for (column, at) in [("deleted_at", 50), ("removed_at", 51), ("held_at", 52)] {
        let c = comment(&s, hers, carol, None, T0 + at).await;
        sqlx::query(&format!("UPDATE comments SET {column} = 1 WHERE id = ?"))
            .bind(c)
            .execute(s.pool())
            .await
            .unwrap();
    }
    let dave = s
        .record_sign_in("did:plc:dave", "dave.example", true, T0)
        .await
        .unwrap();
    comment(&s, hers, dave, None, T0 + 53).await;
    sqlx::query("UPDATE users SET inactive_at = 1 WHERE id = ?")
        .bind(dave)
        .execute(s.pool())
        .await
        .unwrap();
    // And so does every reply on a story readers cannot see.
    let removed = story(&s, alice, "Removed story").await;
    comment(&s, removed, bob, None, T0 + 54).await;
    sqlx::query("UPDATE stories SET removed_at = 1 WHERE id = ?")
        .bind(removed)
        .execute(s.pool())
        .await
        .unwrap();
    assert_eq!(
        ids(&s.replies(alice, 10, 0).await.unwrap()),
        [reply_to_her, on_her_story]
    );
}

#[tokio::test]
async fn unread_counts_replies_since_the_last_visit() {
    let s = Store::open("sqlite::memory:").await.unwrap();
    s.upsert_tag("rust", "", 0.0).await.unwrap();
    let alice = s
        .record_sign_in("did:plc:alice", "alice.example", true, T0)
        .await
        .unwrap();
    let bob = s
        .record_sign_in("did:plc:bob", "bob.example", true, T0)
        .await
        .unwrap();
    let hers = story(&s, alice, "Alice's story").await;

    // A new account counts from when it was created.
    assert_eq!(s.inbox_read_at(alice).await.unwrap(), T0);
    comment(&s, hers, bob, None, T0 + 10).await;
    comment(&s, hers, bob, None, T0 + 20).await;
    assert_eq!(s.unread_replies(alice).await.unwrap(), 2);
    assert_eq!(s.unread_replies(bob).await.unwrap(), 0);

    s.mark_inbox_read(alice, T0 + 20).await.unwrap();
    assert_eq!(s.inbox_read_at(alice).await.unwrap(), T0 + 20);
    assert_eq!(s.unread_replies(alice).await.unwrap(), 0);

    comment(&s, hers, bob, None, T0 + 30).await;
    assert_eq!(s.unread_replies(alice).await.unwrap(), 1);
    // A reply backfilled from the network keeps its record's older date and
    // does not count as unread.
    comment(&s, hers, bob, None, T0 + 5).await;
    assert_eq!(s.unread_replies(alice).await.unwrap(), 1);
    assert_eq!(s.replies(alice, 10, 0).await.unwrap().len(), 4);
}

#[tokio::test]
async fn accounts_from_before_the_inbox_start_with_it_read() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}", dir.path().join("t.db").display());
    let opts = <sqlx::sqlite::SqliteConnectOptions as std::str::FromStr>::from_str(&url)
        .unwrap()
        .create_if_missing(true);
    let pool = sqlx::SqlitePool::connect_with(opts).await.unwrap();
    // The migrations before the inbox, an account, then the store runs 0008.
    let mut through_7 = sqlx::migrate!("../../migrations");
    through_7.migrations = through_7
        .migrations
        .iter()
        .filter(|m| m.version <= 7)
        .cloned()
        .collect::<Vec<_>>()
        .into();
    through_7.run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO users (did, handle, created_at) VALUES ('did:plc:old', 'old.example', 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    let s = Store::open(&url).await.unwrap();
    let id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE did = 'did:plc:old'")
        .fetch_one(s.pool())
        .await
        .unwrap();
    let read_at = s.inbox_read_at(id).await.unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert!((now - read_at).abs() < 60, "read_at {read_at}, now {now}");
}

#[tokio::test]
async fn read_ribbons_count_visible_comments_since_the_last_read() {
    let s = Store::open("sqlite::memory:").await.unwrap();
    s.upsert_tag("rust", "", 0.0).await.unwrap();
    let alice = s
        .record_sign_in("did:plc:alice", "alice.example", true, T0)
        .await
        .unwrap();
    let bob = s
        .record_sign_in("did:plc:bob", "bob.example", true, T0)
        .await
        .unwrap();
    let read = story(&s, alice, "Read story").await;
    let unread = story(&s, alice, "Never opened").await;
    comment(&s, read, alice, None, T0 + 5).await;
    s.record_read(bob, read, T0 + 10).await.unwrap();

    // Before and at the read time, and bob's own, do not count.
    comment(&s, read, alice, None, T0 + 10).await;
    comment(&s, read, bob, None, T0 + 11).await;
    let counted = comment(&s, read, alice, None, T0 + 12).await;
    comment(&s, read, alice, Some(counted), T0 + 13).await;
    for column in ["deleted_at", "removed_at", "held_at"] {
        let c = comment(&s, read, alice, None, T0 + 14).await;
        sqlx::query(&format!("UPDATE comments SET {column} = 1 WHERE id = ?"))
            .bind(c)
            .execute(s.pool())
            .await
            .unwrap();
    }
    comment(&s, unread, alice, None, T0 + 15).await;

    let both = [read, unread];
    assert_eq!(s.new_comments(bob, &both).await.unwrap(), [(read, 2)]);
    assert!(s.new_comments(alice, &both).await.unwrap().is_empty());
    assert!(s.new_comments(bob, &[]).await.unwrap().is_empty());

    // A later read clears it; an earlier one, such as a slow request
    // finishing late, does not move the time back.
    s.record_read(bob, read, T0 + 20).await.unwrap();
    assert!(s.new_comments(bob, &both).await.unwrap().is_empty());
    s.record_read(bob, read, T0 + 1).await.unwrap();
    assert!(s.new_comments(bob, &both).await.unwrap().is_empty());

    // Pruning drops reads of stories older than the cutoff.
    let reads = || async {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM story_reads")
            .fetch_one(s.pool())
            .await
            .unwrap()
    };
    assert_eq!(
        s.prune_story_reads(T0).await.unwrap(),
        0,
        "created at T0, not before"
    );
    assert_eq!(reads().await, 1);
    assert_eq!(s.prune_story_reads(T0 + 1).await.unwrap(), 1);
    assert_eq!(reads().await, 0);
}
