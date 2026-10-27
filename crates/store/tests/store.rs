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
        .search(&search::parse("error handling"), 10, 0)
        .await
        .unwrap();
    assert_eq!(r.len(), 2);

    let r = s
        .search(&search::parse("\"error handling\" tag:rust"), 10, 0)
        .await
        .unwrap();
    assert_eq!(titles(r), ["Async error handling in Rust"]);

    let r = s
        .search(&search::parse("tag:rust -tag:show"), 10, 0)
        .await
        .unwrap();
    assert_eq!(titles(r), ["Async error handling in Rust"]);

    let r = s
        .search(&search::parse("handling submitter:@bob.example"), 10, 0)
        .await
        .unwrap();
    assert_eq!(titles(r), ["Error handling in Postgres"]);

    let r = s
        .search(&search::parse("domain:example.com sort:new"), 10, 0)
        .await
        .unwrap();
    assert_eq!(
        titles(r),
        ["Show: my rust database", "Async error handling in Rust"]
    );

    let r = s
        .search(&search::parse("title:postgres"), 10, 0)
        .await
        .unwrap();
    assert_eq!(titles(r), ["Error handling in Postgres"]);

    let r = s.search(&search::parse("NEAR(\"x"), 10, 0).await.unwrap();
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
        .search(&search::parse("submitter:@carol.example"), 10, 0)
        .await
        .unwrap();
    assert!(
        r.is_empty(),
        "a search by an unverified handle finds nothing"
    );
    let r = s
        .search(&search::parse("submitter:@did:plc:carol"), 10, 0)
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
