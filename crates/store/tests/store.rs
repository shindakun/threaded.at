use threaded_core::{link, search};
use threaded_store::{Filter, Listing, NewStory, Store};

const T0: i64 = 1_790_000_000;

async fn fixture() -> (Store, i64) {
    let s = Store::open("sqlite::memory:").await.unwrap();
    let alice = s
        .record_sign_in("did:plc:alice", "alice.example", true, T0)
        .await
        .unwrap();
    for t in ["rust", "databases", "show"] {
        s.upsert_tag(t, "", 0.0).await.unwrap();
    }
    s.upsert_tag("meta", "", -0.5).await.unwrap();
    (s, alice)
}

async fn post(
    s: &Store,
    user: i64,
    title: &str,
    url: Option<&str>,
    tags: &[&str],
    at: i64,
) -> String {
    let l = url.map(|u| link::parse(u).unwrap());
    s.insert_story(&NewStory {
        user_id: user,
        title,
        link: l.as_ref(),
        body: if url.is_none() { "text post" } else { "" },
        user_is_author: false,
        tags,
        record: None,
        created_at: at,
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn hot_new_and_tag_listings() {
    let (s, u) = fixture().await;
    post(
        &s,
        u,
        "Old rust story",
        Some("https://blog.example.com/a"),
        &["rust"],
        T0,
    )
    .await;
    post(
        &s,
        u,
        "New database story",
        Some("https://db.example.org/b"),
        &["databases"],
        T0 + 3600,
    )
    .await;
    post(&s, u, "Meta post", None, &["meta"], T0 + 7200).await;

    let f = Filter::default();
    let front = s.list(Listing::Front, &f, 10, 0).await.unwrap();
    let titles: Vec<_> = front.iter().map(|r| r.title.as_str()).collect();
    assert_eq!(
        titles,
        ["New database story", "Old rust story", "Meta post"]
    );

    let new = s.list(Listing::New, &f, 10, 0).await.unwrap();
    assert_eq!(new[0].title, "Meta post");

    let tagged = s
        .list(
            Listing::New,
            &Filter {
                tag: Some("rust".into()),
                ..f.clone()
            },
            10,
            0,
        )
        .await
        .unwrap();
    assert_eq!(tagged.len(), 1);
    assert_eq!(tagged[0].tag_list(), ["rust"]);
    assert_eq!(tagged[0].handle, "alice.example");

    let by_domain = s
        .list(
            Listing::New,
            &Filter {
                domain: Some("example.com".into()),
                ..f
            },
            10,
            0,
        )
        .await
        .unwrap();
    assert_eq!(by_domain.len(), 1);
    assert_eq!(by_domain[0].domain.as_deref(), Some("blog.example.com"));
}

#[tokio::test]
async fn search_combines_text_and_filters() {
    let (s, u) = fixture().await;
    let bob = s
        .record_sign_in("did:plc:bob", "bob.example", true, T0)
        .await
        .unwrap();
    post(
        &s,
        u,
        "Async error handling in Rust",
        Some("https://example.com/errors"),
        &["rust"],
        T0,
    )
    .await;
    post(
        &s,
        bob,
        "Error handling in Postgres",
        Some("https://pg.example.net/e"),
        &["databases"],
        T0 + 10,
    )
    .await;
    post(
        &s,
        u,
        "Show: my rust database",
        Some("https://example.com/show"),
        &["rust", "show"],
        T0 + 20,
    )
    .await;

    let titles =
        |rows: Vec<threaded_store::StoryRow>| rows.into_iter().map(|r| r.title).collect::<Vec<_>>();

    let r = s
        .search(&search::parse("error handling"), None, 10, 0)
        .await
        .unwrap();
    assert_eq!(r.len(), 2);

    let r = s
        .search(&search::parse("\"error handling\" tag:rust"), None, 10, 0)
        .await
        .unwrap();
    assert_eq!(titles(r), ["Async error handling in Rust"]);

    let r = s
        .search(&search::parse("tag:rust -tag:show"), None, 10, 0)
        .await
        .unwrap();
    assert_eq!(titles(r), ["Async error handling in Rust"]);

    let r = s
        .search(
            &search::parse("handling submitter:@bob.example"),
            None,
            10,
            0,
        )
        .await
        .unwrap();
    assert_eq!(titles(r), ["Error handling in Postgres"]);

    let r = s
        .search(&search::parse("domain:example.com sort:new"), None, 10, 0)
        .await
        .unwrap();
    assert_eq!(
        titles(r),
        ["Show: my rust database", "Async error handling in Rust"]
    );

    let r = s
        .search(&search::parse("title:postgres"), None, 10, 0)
        .await
        .unwrap();
    assert_eq!(titles(r), ["Error handling in Postgres"]);

    let r = s
        .search(&search::parse("NEAR(\"x"), None, 10, 0)
        .await
        .unwrap();
    assert!(r.is_empty());
}

#[tokio::test]
async fn unknown_tag_is_rejected() {
    let (s, u) = fixture().await;
    let l = link::parse("https://example.com/").unwrap();
    let err = s
        .insert_story(&NewStory {
            user_id: u,
            title: "x",
            link: Some(&l),
            body: "",
            user_is_author: false,
            tags: &["nope"],
            record: None,
            created_at: T0,
        })
        .await;
    assert!(err.is_err());
    assert!(s
        .list(Listing::New, &Filter::default(), 10, 0)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn story_lookup_by_short_id() {
    let (s, u) = fixture().await;
    let id = post(
        &s,
        u,
        "Find me",
        Some("https://example.com/f"),
        &["rust"],
        T0,
    )
    .await;
    assert_eq!(id.len(), 6);
    assert_eq!(s.story(&id).await.unwrap().unwrap().title, "Find me");
    assert!(s.story("zzzzzz").await.unwrap().is_none());
}

#[tokio::test]
async fn sign_in_records_and_display_names() {
    let s = Store::open("sqlite::memory:").await.unwrap();
    assert_eq!(s.display_name("did:plc:new").await.unwrap(), None);
    s.record_sign_in("did:plc:new", "new.example", true, T0)
        .await
        .unwrap();
    assert_eq!(
        s.display_name("did:plc:new").await.unwrap().as_deref(),
        Some("new.example")
    );
    s.record_sign_in("did:plc:new", "did:plc:new", false, T0 + 1)
        .await
        .unwrap();
    assert_eq!(
        s.display_name("did:plc:new").await.unwrap().as_deref(),
        Some("did:plc:new")
    );
}

#[tokio::test]
async fn an_unverified_handle_names_nobody() {
    let s = Store::open("sqlite::memory:").await.unwrap();
    s.upsert_tag("rust", "", 0.0).await.unwrap();
    let u = s
        .record_sign_in("did:plc:carol", "carol.example", false, T0)
        .await
        .unwrap();
    post(
        &s,
        u,
        "Carol's post",
        Some("https://example.com/c"),
        &["rust"],
        T0,
    )
    .await;
    let rows = s
        .list(Listing::New, &Filter::default(), 10, 0)
        .await
        .unwrap();
    assert_eq!(
        rows[0].handle, "did:plc:carol",
        "shown by DID until verified"
    );
    assert!(s.user_by_handle("carol.example").await.unwrap().is_none());
    assert!(s.user_by_handle("did:plc:carol").await.unwrap().is_some());
    let r = s
        .search(&search::parse("submitter:@carol.example"), None, 10, 0)
        .await
        .unwrap();
    assert!(
        r.is_empty(),
        "a search by an unverified handle finds nothing"
    );
    let r = s
        .search(&search::parse("submitter:@did:plc:carol"), None, 10, 0)
        .await
        .unwrap();
    assert_eq!(r.len(), 1, "a search by DID still works");
}

/// Migration 0007 rebuilds `mod_log`; every row written before it must come
/// through unchanged.
#[tokio::test]
async fn migration_0007_keeps_every_mod_log_row() {
    use std::str::FromStr;

    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}", dir.path().join("t.db").display());
    let opts = sqlx::sqlite::SqliteConnectOptions::from_str(&url)
        .unwrap()
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = sqlx::SqlitePool::connect_with(opts).await.unwrap();
    let mut through_6 = sqlx::migrate!("../../migrations");
    through_6.migrations = through_6
        .migrations
        .iter()
        .filter(|m| m.version <= 6)
        .cloned()
        .collect::<Vec<_>>()
        .into();
    through_6.run(&pool).await.unwrap();

    sqlx::query(
        "INSERT INTO users (id, did, handle, created_at) VALUES (1, 'did:plc:m', 'm.example', 0), \
         (2, 'did:plc:u', 'u.example', 0)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO mod_log (id, moderator_id, actor, action, user_id, reason, detail, created_at) VALUES \
         (1, 1, 'moderator', 'ban', 2, 'Spam', '', 10), \
         (2, NULL, 'console', 'grant role', 1, 'From the console', 'user to admin', 20), \
         (3, NULL, 'triage', 'hold story', 2, 'Spam', '', 30)",
    )
    .execute(&pool)
    .await
    .unwrap();
    type Row = (
        i64,
        Option<i64>,
        String,
        String,
        Option<i64>,
        String,
        String,
        i64,
    );
    let read = |pool: sqlx::SqlitePool| async move {
        sqlx::query_as::<_, Row>(
            "SELECT id, moderator_id, actor, action, user_id, reason, detail, created_at \
             FROM mod_log ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap()
    };
    let before = read(pool.clone()).await;
    pool.close().await;

    // Opening the store applies 0007 and anything after it.
    let s = Store::open(&url).await.unwrap();
    assert_eq!(read(s.pool().clone()).await, before);
    let check: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(s.pool())
        .await
        .unwrap();
    assert_eq!(check, "ok");
    // The rebuilt table takes the new actor and still refuses unknown ones.
    sqlx::query(
        "INSERT INTO mod_log (actor, action, created_at) VALUES ('indexer', 'detach story', 40)",
    )
    .execute(s.pool())
    .await
    .unwrap();
    assert!(sqlx::query(
        "INSERT INTO mod_log (actor, action, created_at) VALUES ('robot', 'x', 50)"
    )
    .execute(s.pool())
    .await
    .is_err());
}

#[tokio::test]
async fn a_readers_hides_and_tag_filters_change_only_their_listings() {
    let (s, alice) = fixture().await;
    let bob = s
        .record_sign_in("did:plc:bob", "bob.example", true, T0)
        .await
        .unwrap();
    let hid = post(&s, alice, "Hidden by bob", None, &["rust"], T0).await;
    post(&s, alice, "Database story", None, &["databases"], T0 + 1).await;
    post(
        &s,
        alice,
        "Rust and databases",
        None,
        &["rust", "databases"],
        T0 + 2,
    )
    .await;
    post(&s, alice, "Plain rust", None, &["rust"], T0 + 3).await;
    let id = |short: String| {
        let s = s.clone();
        async move { s.story(&short).await.unwrap().unwrap().id }
    };
    let hid_id = id(hid.clone()).await;
    s.set_hidden(bob, hid_id, true, T0 + 10).await.unwrap();
    s.set_tag_filters(bob, &["databases".into()]).await.unwrap();

    let titles = |rows: Vec<threaded_store::StoryRow>| {
        let mut t: Vec<String> = rows.into_iter().map(|r| r.title).collect();
        t.sort();
        t
    };
    let as_reader = |reader: Option<i64>, tag: Option<&str>| Filter {
        tag: tag.map(String::from),
        reader,
        ..Filter::default()
    };
    let everything = [
        "Database story",
        "Hidden by bob",
        "Plain rust",
        "Rust and databases",
    ];
    for listing in [Listing::Front, Listing::New, Listing::Top { since: 0 }] {
        let bobs = s
            .list(listing, &as_reader(Some(bob), None), 10, 0)
            .await
            .unwrap();
        assert_eq!(titles(bobs), ["Plain rust"], "{listing:?}");
        let alices = s
            .list(listing, &as_reader(Some(alice), None), 10, 0)
            .await
            .unwrap();
        assert_eq!(titles(alices), everything, "{listing:?}");
        let anyone = s
            .list(listing, &as_reader(None, None), 10, 0)
            .await
            .unwrap();
        assert_eq!(titles(anyone), everything, "{listing:?}");
    }

    // The filtered tag's own page shows its stories; another tag's page does
    // not show a story that also carries the filtered tag.
    let on_db = s
        .list(
            Listing::New,
            &as_reader(Some(bob), Some("databases")),
            10,
            0,
        )
        .await
        .unwrap();
    assert_eq!(titles(on_db), ["Database story", "Rust and databases"]);
    let on_rust = s
        .list(Listing::New, &as_reader(Some(bob), Some("rust")), 10, 0)
        .await
        .unwrap();
    assert_eq!(titles(on_rust), ["Plain rust"]);

    // Search: the same, and `tag:` names a filtered tag back in.
    let found = s
        .search(&search::parse("sort:new tag:rust"), Some(bob), 10, 0)
        .await
        .unwrap();
    assert_eq!(titles(found), ["Plain rust"]);
    let found = s
        .search(&search::parse("tag:databases"), Some(bob), 10, 0)
        .await
        .unwrap();
    assert_eq!(titles(found), ["Database story", "Rust and databases"]);
    let found = s
        .search(&search::parse("post"), Some(bob), 10, 0)
        .await
        .unwrap();
    assert_eq!(
        titles(found),
        ["Plain rust"],
        "a hidden story and filtered tags leave search"
    );
    let found = s.search(&search::parse("post"), None, 10, 0).await.unwrap();
    assert_eq!(titles(found), everything);

    // Profiles are listings too.
    let on_profile = s.stories_by_user(alice, Some(bob), 10).await.unwrap();
    assert_eq!(titles(on_profile), ["Plain rust"]);
    assert_eq!(s.stories_by_user(alice, None, 10).await.unwrap().len(), 4);

    // The hidden list holds what bob hid, whatever its tags; unhiding
    // brings it back.
    let hidden = s
        .list(
            Listing::Hidden { user_id: bob },
            &as_reader(Some(bob), None),
            10,
            0,
        )
        .await
        .unwrap();
    assert_eq!(titles(hidden), ["Hidden by bob"]);
    assert!(s
        .list(
            Listing::Hidden { user_id: alice },
            &Filter::default(),
            10,
            0
        )
        .await
        .unwrap()
        .is_empty());
    s.set_hidden(bob, hid_id, false, T0 + 20).await.unwrap();
    let bobs = s
        .list(Listing::New, &as_reader(Some(bob), None), 10, 0)
        .await
        .unwrap();
    assert_eq!(titles(bobs), ["Hidden by bob", "Plain rust"]);

    // Clearing the filters brings the tag back.
    s.set_tag_filters(bob, &[]).await.unwrap();
    let bobs = s
        .list(Listing::New, &as_reader(Some(bob), None), 10, 0)
        .await
        .unwrap();
    assert_eq!(titles(bobs), everything);
}

#[tokio::test]
async fn saved_stories_newest_save_first() {
    let (s, alice) = fixture().await;
    let bob = s
        .record_sign_in("did:plc:bob", "bob.example", true, T0)
        .await
        .unwrap();
    let mut ids = Vec::new();
    for (n, title) in ["First", "Second", "Third"].iter().enumerate() {
        let short = post(&s, alice, title, None, &["databases"], T0 + n as i64).await;
        ids.push(s.story(&short).await.unwrap().unwrap().id);
    }
    s.set_saved(bob, ids[2], true, T0 + 100).await.unwrap();
    s.set_saved(bob, ids[0], true, T0 + 200).await.unwrap();
    // Saving again keeps the first save time, so the order holds.
    s.set_saved(bob, ids[2], true, T0 + 300).await.unwrap();
    // A saved story stays on the saved list when it is hidden or filtered.
    s.set_hidden(bob, ids[0], true, T0 + 400).await.unwrap();
    s.set_tag_filters(bob, &["databases".into()]).await.unwrap();

    let saved = |user_id| {
        let s = s.clone();
        async move {
            s.list(
                Listing::Saved { user_id },
                &Filter {
                    reader: Some(user_id),
                    ..Filter::default()
                },
                10,
                0,
            )
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.title)
            .collect::<Vec<_>>()
        }
    };
    assert_eq!(saved(bob).await, ["First", "Third"]);
    assert!(saved(alice).await.is_empty(), "saves are per reader");

    let m = s.marks(bob, &ids).await.unwrap();
    assert_eq!(
        m.saved
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        [ids[0], ids[2]].into()
    );
    assert_eq!(m.hidden, [ids[0]]);
    assert_eq!(s.marks(alice, &ids).await.unwrap(), Default::default());
    assert_eq!(s.marks(bob, &[]).await.unwrap(), Default::default());

    s.set_saved(bob, ids[2], false, T0 + 500).await.unwrap();
    assert_eq!(saved(bob).await, ["First"]);
    // Taking away a save that is not there is not an error.
    s.set_saved(bob, ids[1], false, T0 + 500).await.unwrap();
}

#[tokio::test]
async fn tag_filters_take_only_active_tags() {
    let (s, alice) = fixture().await;
    s.set_tag_filters(alice, &["show".into(), "nope".into(), "show".into()])
        .await
        .unwrap();
    assert_eq!(s.tag_filters(alice).await.unwrap(), ["show"]);
    sqlx::query("UPDATE tags SET active = 0 WHERE name = 'rust'")
        .execute(s.pool())
        .await
        .unwrap();
    s.set_tag_filters(alice, &["rust".into(), "meta".into()])
        .await
        .unwrap();
    assert_eq!(
        s.tag_filters(alice).await.unwrap(),
        ["meta"],
        "the old filters are replaced"
    );
}
