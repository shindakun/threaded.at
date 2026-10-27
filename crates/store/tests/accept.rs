use threaded_core::policy::Policy;
use threaded_store::{CommentInput, Context, Refusal, Source, Store, StoryInput, UserRow};

const NOW: i64 = 1_790_000_000;
const DAY: i64 = 86_400;

async fn store() -> Store {
    let s = Store::open("sqlite::memory:").await.unwrap();
    for t in ["rust", "show", "ask"] {
        s.upsert_tag(t, "", 0.0).await.unwrap();
    }
    s.upsert_tag("mods", "", 0.0).await.unwrap();
    sqlx::query("UPDATE tags SET privileged = 1 WHERE name = 'mods'")
        .execute(s.pool())
        .await
        .unwrap();
    s
}

/// A user who signed in `age_days` ago.
async fn user(s: &Store, name: &str, age_days: i64) -> UserRow {
    let did = format!("did:plc:{name}");
    s.record_sign_in(&did, &format!("{name}.example"), true, NOW - age_days * DAY)
        .await
        .unwrap();
    s.user_by_did(&did).await.unwrap().unwrap()
}

fn cx(policy: &Policy, source: Source, triage: bool) -> Context<'_> {
    Context {
        policy,
        source,
        triage,
        now: NOW,
    }
}

fn tags(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

fn story<'a>(title: &'a str, url: &'a str, tags: &'a [String]) -> StoryInput<'a> {
    StoryInput {
        title,
        url,
        text: "",
        tags,
        authored: false,
    }
}

async fn queued(s: &Store) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM triage_queue")
        .fetch_one(s.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn accepts_and_queues_for_triage_only_when_on() {
    let s = store().await;
    let alice = user(&s, "alice", 100).await;
    let p = Policy::default();
    let t = tags(&["rust"]);

    let off = s
        .accept_story(
            &alice,
            &story("One", "https://e.example/1", &t),
            ("at://a/s/1", "c1"),
            &cx(&p, Source::Web, false),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(!off.existing);
    assert_eq!(queued(&s).await, 0, "triage off queues nothing");

    let on = s
        .accept_story(
            &alice,
            &story("Two", "https://e.example/2", &t),
            ("at://a/s/2", "c2"),
            &cx(&p, Source::Web, true),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(queued(&s).await, 1);

    let st = s.story(&on.short_id).await.unwrap().unwrap();
    s.accept_comment(
        &alice,
        &CommentInput {
            story: &st,
            parent: None,
            text: "A comment",
        },
        ("at://a/c/1", "c3"),
        &cx(&p, Source::Network, true),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(queued(&s).await, 2, "comments are queued too");
}

#[tokio::test]
async fn a_known_record_is_not_indexed_twice() {
    let s = store().await;
    let alice = user(&s, "alice", 100).await;
    let p = Policy::default();
    let t = tags(&["rust"]);
    let input = story("Once", "https://e.example/once", &t);
    let first = s
        .accept_story(
            &alice,
            &input,
            ("at://a/s/1", "c1"),
            &cx(&p, Source::Web, true),
        )
        .await
        .unwrap()
        .unwrap();
    // The same record back from the network: not a duplicate, not a second row.
    let again = s
        .accept_story(
            &alice,
            &input,
            ("at://a/s/1", "c1"),
            &cx(&p, Source::Network, true),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(again.existing);
    assert_eq!(again.short_id, first.short_id);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM stories")
        .fetch_one(s.pool())
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(queued(&s).await, 1);
}

#[tokio::test]
async fn refusals() {
    let s = store().await;
    let alice = user(&s, "alice", 100).await;
    let newbie = user(&s, "new", 1).await;
    let p = Policy::default();
    let web = cx(&p, Source::Web, false);
    let check = |u: &UserRow, title: &str, url: &str, names: &[&str]| {
        let (s, u, title, url, t) = (
            s.clone(),
            u.clone(),
            title.to_string(),
            url.to_string(),
            tags(names),
        );
        let p = p.clone();
        async move {
            s.check_story(&u, &story(&title, &url, &t), &cx(&p, Source::Web, false))
                .await
                .unwrap()
        }
    };
    let url = "https://e.example/x";

    assert_eq!(check(&alice, "", url, &["rust"]).await, Err(Refusal::Title));
    assert_eq!(check(&alice, "T", "", &["rust"]).await, Err(Refusal::Empty));
    assert!(matches!(
        check(&alice, "T", "ftp://e.example/x", &["rust"]).await,
        Err(Refusal::Link(_))
    ));
    assert_eq!(check(&alice, "T", url, &[]).await, Err(Refusal::NoTags));
    assert_eq!(
        check(&alice, "T", url, &["nope"]).await,
        Err(Refusal::UnknownTag("nope".into()))
    );
    assert_eq!(
        check(&alice, "T", url, &["mods"]).await,
        Err(Refusal::Privileged("mods".into()))
    );
    assert_eq!(
        check(&alice, "T", url, &["show"]).await,
        Err(Refusal::ShowNeedsAuthored)
    );
    assert_eq!(
        check(&alice, "T", url, &["ask"]).await,
        Err(Refusal::AskIsText)
    );
    for t in ["a", "b", "c", "d", "e", "f"] {
        s.upsert_tag(t, "", 0.0).await.unwrap();
    }
    assert_eq!(
        check(&alice, "T", url, &["a", "b", "c", "d", "e", "f"]).await,
        Err(Refusal::TooManyTags)
    );

    // Duplicates point at the first story.
    let rust = tags(&["rust"]);
    let first = s
        .accept_story(
            &alice,
            &story("First", "https://e.example/dup", &rust),
            ("at://a/s/1", "c1"),
            &web,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        check(
            &alice,
            "Again",
            "https://www.e.example/dup/?utm_source=x",
            &["rust"]
        )
        .await,
        Err(Refusal::Duplicate(first.short_id))
    );

    // New accounts post two stories a day.
    for i in 0..2 {
        let u = format!("https://e.example/n{i}");
        s.accept_story(
            &newbie,
            &story("N", &u, &rust),
            (&format!("at://n/s/{i}"), "c"),
            &web,
        )
        .await
        .unwrap()
        .unwrap();
    }
    assert_eq!(
        check(&newbie, "N", "https://e.example/n3", &["rust"]).await,
        Err(Refusal::StoryLimit(2))
    );

    // Banned accounts post nothing.
    sqlx::query("UPDATE users SET banned_at = ? WHERE id = ?")
        .bind(NOW)
        .bind(alice.id)
        .execute(s.pool())
        .await
        .unwrap();
    let banned = s.user_by_did("did:plc:alice").await.unwrap().unwrap();
    assert_eq!(
        check(&banned, "T", "https://e.example/b", &["rust"]).await,
        Err(Refusal::Banned)
    );
    let st = s
        .story(&first_id(&s, alice.id).await)
        .await
        .unwrap()
        .unwrap();
    let c = s
        .check_comment(
            &banned,
            &CommentInput {
                story: &st,
                parent: None,
                text: "hi",
            },
            &web,
        )
        .await
        .unwrap();
    assert_eq!(c, Err(Refusal::Banned));
}

async fn first_id(s: &Store, user_id: i64) -> String {
    s.stories_by_user(user_id, 1).await.unwrap()[0]
        .short_id
        .clone()
}

#[tokio::test]
async fn network_records_drop_unusable_tags() {
    let s = store().await;
    let alice = user(&s, "alice", 100).await;
    let p = Policy::default();
    let net = cx(&p, Source::Network, false);

    let mixed = tags(&["rust", "nope", "mods"]);
    let checked = s
        .check_story(&alice, &story("T", "https://e.example/m", &mixed), &net)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        checked.tags,
        vec!["rust".to_string()],
        "unknown and privileged tags are dropped"
    );

    let unknown = tags(&["nope"]);
    assert_eq!(
        s.check_story(&alice, &story("T", "https://e.example/u", &unknown), &net)
            .await
            .unwrap(),
        Err(Refusal::NoTags),
        "a record left with no known tag is not indexed"
    );
}

#[tokio::test]
async fn comments_on_locked_stories_are_refused() {
    let s = store().await;
    let alice = user(&s, "alice", 100).await;
    let p = Policy::default();
    let web = cx(&p, Source::Web, false);
    let t = tags(&["rust"]);
    let acc = s
        .accept_story(
            &alice,
            &story("Old", "https://e.example/old", &t),
            ("at://a/s/old", "c"),
            &web,
        )
        .await
        .unwrap()
        .unwrap();
    let st = s.story(&acc.short_id).await.unwrap().unwrap();
    let later = Context {
        now: NOW + 91 * DAY,
        ..web
    };
    let r = s
        .check_comment(
            &alice,
            &CommentInput {
                story: &st,
                parent: None,
                text: "late",
            },
            &later,
        )
        .await
        .unwrap();
    assert_eq!(r, Err(Refusal::Locked));
}

fn top_level<'a>(story: &'a threaded_store::StoryRow, text: &'a str) -> CommentInput<'a> {
    CommentInput {
        story,
        parent: None,
        text,
    }
}

#[tokio::test]
async fn comment_refusals_and_the_hourly_limit() {
    let s = store().await;
    let alice = user(&s, "alice", 100).await;
    let newbie = user(&s, "newbie", 3).await;
    let p = Policy::default();
    let web = cx(&p, Source::Web, false);
    let t = tags(&["rust"]);
    let mut stories = Vec::new();
    for n in 0..2 {
        let acc = s
            .accept_story(
                &alice,
                &story("S", &format!("https://e.example/c{n}"), &t),
                (&format!("at://a/s/c{n}"), "c"),
                &web,
            )
            .await
            .unwrap()
            .unwrap();
        stories.push(s.story(&acc.short_id).await.unwrap().unwrap());
    }
    let (st, other) = (&stories[0], &stories[1]);
    let input = |text| top_level(st, text);

    let long = "w".repeat(5_001);
    for (text, want) in [("   ", Refusal::NoText), (long.as_str(), Refusal::TooLong)] {
        assert_eq!(
            s.check_comment(&newbie, &top_level(st, text), &web)
                .await
                .unwrap(),
            Err(want)
        );
    }

    // A reply must be to a live comment on the same story.
    let parent = s
        .accept_comment(&alice, &input("Parent"), ("at://a/c/p", "c"), &web)
        .await
        .unwrap()
        .unwrap();
    let again = s
        .accept_comment(&alice, &input("Parent"), ("at://a/c/p", "c"), &web)
        .await
        .unwrap()
        .unwrap();
    assert!(
        again.existing && again.id == parent.id,
        "a known record is not indexed twice"
    );
    let p_row = s.comment(&parent.short_id).await.unwrap().unwrap();
    let elsewhere = CommentInput {
        story: other,
        parent: Some(&p_row),
        text: "Wrong story",
    };
    assert_eq!(
        s.check_comment(&newbie, &elsewhere, &web).await.unwrap(),
        Err(Refusal::ParentClosed)
    );
    s.delete_comment(&p_row, NOW).await.unwrap();
    let p_row = s.comment(&parent.short_id).await.unwrap().unwrap();
    let to_deleted = CommentInput {
        story: st,
        parent: Some(&p_row),
        text: "Too late",
    };
    assert_eq!(
        s.check_comment(&newbie, &to_deleted, &web).await.unwrap(),
        Err(Refusal::ParentClosed)
    );

    // A new account posts six comments an hour; the seventh waits.
    let (_, per_hour) = p.new_limits;
    for n in 0..per_hour {
        s.accept_comment(&newbie, &input("Hi"), (&format!("at://n/c/{n}"), "c"), &web)
            .await
            .unwrap()
            .unwrap();
    }
    assert_eq!(
        s.accept_comment(&newbie, &input("Hi"), ("at://n/c/x", "c"), &web)
            .await
            .unwrap(),
        Err(Refusal::CommentLimit(per_hour))
    );
    let hour_later = Context {
        now: NOW + 3_601,
        ..web
    };
    assert_eq!(
        s.check_comment(&newbie, &input("Hi"), &hour_later)
            .await
            .unwrap(),
        Ok(())
    );
}

#[tokio::test]
async fn tags_closed_to_new_accounts() {
    let s = store().await;
    sqlx::query("UPDATE tags SET new_users_ok = 0 WHERE name = 'show'")
        .execute(s.pool())
        .await
        .unwrap();
    let member = user(&s, "member", 100).await;
    let newbie = user(&s, "newbie", 3).await;
    let p = Policy::default();
    let web = cx(&p, Source::Web, false);
    let net = cx(&p, Source::Network, false);

    assert_eq!(
        s.check_tags(&newbie, &tags(&["rust", "show"]), &web)
            .await
            .unwrap(),
        Err(Refusal::Restricted("show".into()))
    );
    assert_eq!(
        s.check_tags(&newbie, &tags(&["rust", "show", "mods"]), &net)
            .await
            .unwrap(),
        Ok(tags(&["rust"])),
        "the network drops tags the author may not use"
    );
    assert_eq!(
        s.check_tags(&member, &tags(&["show", "rust", "show"]), &web)
            .await
            .unwrap(),
        Ok(tags(&["show", "rust"])),
        "members may; a repeated tag counts once"
    );
}
