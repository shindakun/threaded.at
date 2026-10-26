use threaded_core::link;
use threaded_core::policy::Policy;
use threaded_store::{
    By, CommentInput, Context, ModError, Source, Store, StoryEdit, StoryInput, StoryOverride,
    Target, UserRow, VoteChange,
};

const NOW: i64 = 1_790_000_000;
const DAY: i64 = 86_400;

async fn store() -> Store {
    let s = Store::open("sqlite::memory:").await.unwrap();
    for t in ["rust", "web", "meta"] {
        s.upsert_tag(t, "", 0.0).await.unwrap();
    }
    s
}

async fn user(s: &Store, name: &str) -> UserRow {
    let did = format!("did:plc:{name}");
    s.record_sign_in(&did, &format!("{name}.example"), true, NOW - 100 * DAY)
        .await
        .unwrap();
    s.user_by_did(&did).await.unwrap().unwrap()
}

fn cx(p: &Policy) -> Context<'_> {
    Context {
        policy: p,
        source: Source::Web,
        triage: false,
        now: NOW,
    }
}

/// Posts a story and returns its ID and short ID.
async fn post(s: &Store, u: &UserRow, title: &str, url: &str) -> (i64, String) {
    let p = Policy::default();
    let tags = vec!["rust".to_string()];
    let a = s
        .accept_story(
            u,
            &StoryInput {
                title,
                url,
                text: "",
                tags: &tags,
                authored: false,
            },
            (&format!("at://{}/s/{title}", u.did), "cid"),
            &cx(&p),
        )
        .await
        .unwrap()
        .unwrap();
    (a.id, a.short_id)
}

async fn comment(s: &Store, u: &UserRow, short_id: &str, text: &str) -> i64 {
    let p = Policy::default();
    let st = s.story(short_id).await.unwrap().unwrap();
    s.accept_comment(
        u,
        &CommentInput {
            story: &st,
            parent: None,
            text,
        },
        (&format!("at://{}/c/{text}", u.did), "cid"),
        &cx(&p),
    )
    .await
    .unwrap()
    .unwrap()
    .id
}

async fn log_actions(s: &Store) -> Vec<(String, String, String)> {
    s.mod_log(100, 0)
        .await
        .unwrap()
        .into_iter()
        .rev()
        .map(|r| (r.actor, r.action, r.reason))
        .collect()
}

#[tokio::test]
async fn removal_hides_and_restore_returns_with_both_logged() {
    let s = store().await;
    let (alice, m) = (user(&s, "alice").await, user(&s, "mod").await);
    let (id, sid) = post(&s, &alice, "Spammy", "https://e.example/s").await;
    let by = By::Moderator(m.id);

    assert_eq!(
        s.remove(Target::Story(id), by, "  ", NOW).await.unwrap(),
        Err(ModError::NoReason)
    );
    s.remove(Target::Story(id), by, "Spam", NOW)
        .await
        .unwrap()
        .unwrap();
    assert!(
        s.story(&sid).await.unwrap().is_none(),
        "hidden from readers"
    );
    let any = s.story_any(&sid).await.unwrap().unwrap();
    assert_eq!(any.removed_reason.as_deref(), Some("Spam"));
    assert!(any.hidden());
    assert_eq!(
        s.remove(Target::Story(id), by, "Again", NOW).await.unwrap(),
        Err(ModError::Refused("It is already removed."))
    );

    s.restore(Target::Story(id), by, "Not spam after all", NOW)
        .await
        .unwrap()
        .unwrap();
    assert!(s.story(&sid).await.unwrap().is_some());
    assert_eq!(
        log_actions(&s).await,
        vec![
            ("moderator".into(), "remove story".into(), "Spam".into()),
            (
                "moderator".into(),
                "restore story".into(),
                "Not spam after all".into()
            ),
        ]
    );
    let row = &s.mod_log(1, 0).await.unwrap()[0];
    assert_eq!(row.moderator.as_deref(), Some("mod.example"));
    assert_eq!(row.user_handle.as_deref(), Some("alice.example"));
    assert_eq!(row.story_short_id.as_deref(), Some(sid.as_str()));
}

#[tokio::test]
async fn removed_comments_leave_the_count() {
    let s = store().await;
    let (alice, bob, m) = (
        user(&s, "alice").await,
        user(&s, "bob").await,
        user(&s, "mod").await,
    );
    let (_, sid) = post(&s, &alice, "Story", "https://e.example/c").await;
    let c = comment(&s, &bob, &sid, "rude").await;
    assert_eq!(s.story(&sid).await.unwrap().unwrap().comment_count, 1);
    s.remove(Target::Comment(c), By::Moderator(m.id), "Unkind", NOW)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s.story(&sid).await.unwrap().unwrap().comment_count, 0);
    assert!(s.comments_by_user(bob.id, 10).await.unwrap().is_empty());
    s.restore(Target::Comment(c), By::Moderator(m.id), "Appeal", NOW)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(s.story(&sid).await.unwrap().unwrap().comment_count, 1);
}

#[tokio::test]
async fn a_moderator_edit_survives_the_authors_edit_until_cleared() {
    let s = store().await;
    let (alice, m) = (user(&s, "alice").await, user(&s, "mod").await);
    let (id, sid) = post(&s, &alice, "CLICK THIS", "https://e.example/a").await;
    let by = By::Moderator(m.id);

    let story = s.story(&sid).await.unwrap().unwrap();
    let l = link::parse("https://e.example/b").unwrap();
    let tags = vec!["web".to_string()];
    s.override_story(
        &story,
        &StoryOverride {
            title: "A calm title",
            link: Some(&l),
            tags: &tags,
        },
        by,
        "Editorialized title",
        NOW,
    )
    .await
    .unwrap()
    .unwrap();
    let story = s.story(&sid).await.unwrap().unwrap();
    assert_eq!(
        (
            story.title.as_str(),
            story.url.as_deref(),
            story.tags.as_str(),
            story.mod_edited
        ),
        ("A calm title", Some("https://e.example/b"), "web", true)
    );

    // The author edits: their values change underneath, readers still see the override.
    let al = link::parse("https://e.example/a2").unwrap();
    s.edit_story(
        &story,
        &StoryEdit {
            title: "CLICK THIS NOW",
            link: Some(&al),
            body: "new body",
            tags: &["rust", "meta"],
            cid: None,
        },
        NOW + 10,
    )
    .await
    .unwrap();
    let story = s.story(&sid).await.unwrap().unwrap();
    assert_eq!(story.title, "A calm title");
    assert_eq!(story.tags, "web");
    assert_eq!(story.body, "new body", "the body is the author's");

    s.clear_override(&story, by, "Author fixed it", NOW + 20)
        .await
        .unwrap()
        .unwrap();
    let story = s.story(&sid).await.unwrap().unwrap();
    assert_eq!(
        (
            story.title.as_str(),
            story.url.as_deref(),
            story.tags.as_str(),
            story.mod_edited
        ),
        (
            "CLICK THIS NOW",
            Some("https://e.example/a2"),
            "meta rust",
            false
        )
    );
    let log = s
        .mod_log_for(threaded_store::LogTarget::Story(id))
        .await
        .unwrap();
    assert_eq!(log[0].action, "edit story");
    assert!(
        log[0].detail.contains("title: CLICK THIS to A calm title"),
        "{}",
        log[0].detail
    );
    assert_eq!(log[1].action, "clear story edit");
}

#[tokio::test]
async fn merge_moves_comments_and_votes_and_unmerge_moves_them_back() {
    let s = store().await;
    let (alice, bob, carol, m) = (
        user(&s, "alice").await,
        user(&s, "bob").await,
        user(&s, "carol").await,
        user(&s, "mod").await,
    );
    let by = By::Moderator(m.id);
    let (a, a_sid) = post(&s, &alice, "First", "https://e.example/m1").await;
    let (b, b_sid) = post(&s, &bob, "Second", "https://e.example/m2").await;
    comment(&s, &carol, &a_sid, "on A").await;
    // Carol votes on both; Bob votes on A only.
    s.set_vote(
        carol.id,
        Target::Story(a),
        VoteChange::Up { at_uri: None },
        NOW,
    )
    .await
    .unwrap();
    s.set_vote(
        carol.id,
        Target::Story(b),
        VoteChange::Up { at_uri: None },
        NOW,
    )
    .await
    .unwrap();
    s.set_vote(
        bob.id,
        Target::Story(a),
        VoteChange::Up { at_uri: None },
        NOW,
    )
    .await
    .unwrap();

    assert_eq!(
        s.merge(a, a, by, "x", NOW).await.unwrap(),
        Err(ModError::Refused("A story cannot merge into itself."))
    );
    s.merge(a, b, by, "Same article", NOW)
        .await
        .unwrap()
        .unwrap();
    let merged = s.story_any(&a_sid).await.unwrap().unwrap();
    assert_eq!(merged.merged_into.as_deref(), Some(b_sid.as_str()));
    assert!(s.story(&a_sid).await.unwrap().is_none());
    let target = s.story(&b_sid).await.unwrap().unwrap();
    assert_eq!(target.comment_count, 1, "A's comment moved");
    assert_eq!(
        target.score, 3,
        "B: 1 + carol + bob; carol's vote counts once"
    );
    assert_eq!(
        s.merge(b, a, by, "x", NOW).await.unwrap(),
        Err(ModError::Refused(
            "The target story is itself merged; merge into the story it points to."
        ))
    );

    s.unmerge(a, by, "Different articles", NOW)
        .await
        .unwrap()
        .unwrap();
    let a_row = s.story(&a_sid).await.unwrap().unwrap();
    let b_row = s.story(&b_sid).await.unwrap().unwrap();
    assert_eq!((a_row.comment_count, b_row.comment_count), (1, 0));
    assert_eq!(
        (a_row.score, b_row.score),
        (3, 2),
        "A: carol and bob again; B: carol"
    );
    assert_eq!(
        log_actions(&s)
            .await
            .into_iter()
            .map(|r| r.1)
            .collect::<Vec<_>>(),
        vec!["merge story", "unmerge story"]
    );
}

#[tokio::test]
async fn locks_bans_badges_and_notes() {
    let s = store().await;
    let (alice, m, admin) = (
        user(&s, "alice").await,
        user(&s, "mod").await,
        user(&s, "admin").await,
    );
    sqlx::query("UPDATE users SET role = 'moderator' WHERE id = ?")
        .bind(m.id)
        .execute(s.pool())
        .await
        .unwrap();
    let by = By::Moderator(m.id);
    let (id, sid) = post(&s, &alice, "Heated", "https://e.example/l").await;

    s.set_locked(id, true, by, "Thread got out of hand", NOW)
        .await
        .unwrap()
        .unwrap();
    let st = s.story(&sid).await.unwrap().unwrap();
    assert!(st.locked);
    assert_eq!(st.locked_reason.as_deref(), Some("Thread got out of hand"));
    s.set_locked(id, false, by, "Calmer now", NOW)
        .await
        .unwrap()
        .unwrap();
    assert!(!s.story(&sid).await.unwrap().unwrap().locked);

    assert_eq!(
        s.set_banned(m.id, true, by, false, "self", NOW)
            .await
            .unwrap(),
        Err(ModError::Refused("You cannot ban yourself."))
    );
    assert_eq!(
        s.set_banned(m.id, true, By::Moderator(alice.id), false, "x", NOW)
            .await
            .unwrap(),
        Err(ModError::Refused(
            "Only an admin can ban a moderator or admin."
        ))
    );
    s.set_banned(alice.id, true, by, false, "Spam account", NOW)
        .await
        .unwrap()
        .unwrap();
    assert!(s
        .user_by_did(&alice.did)
        .await
        .unwrap()
        .unwrap()
        .banned_at
        .is_some());
    s.set_banned(
        m.id,
        true,
        By::Moderator(admin.id),
        true,
        "Abuse of tools",
        NOW,
    )
    .await
    .unwrap()
    .unwrap();

    s.grant_badge(
        alice.id,
        "Maintainer",
        Some("https://e.example/p"),
        by,
        "Runs the project",
        NOW,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        s.grant_badge(alice.id, "x", Some("javascript:alert(1)"), by, "r", NOW)
            .await
            .unwrap(),
        Err(ModError::Refused("The badge link must be an https URL."))
    );
    let badges = s.badges(&[alice.id]).await.unwrap();
    assert_eq!(badges.len(), 1);
    assert_eq!(badges[0].label, "Maintainer");
    s.remove_badge(badges[0].id, by, "Left the project", NOW)
        .await
        .unwrap()
        .unwrap();
    assert!(s.badges(&[alice.id]).await.unwrap().is_empty());

    s.add_note(alice.id, m.id, "Second spam wave this month.", NOW)
        .await
        .unwrap()
        .unwrap();
    let notes = s.notes(alice.id).await.unwrap();
    assert_eq!(notes[0].moderator, "mod.example");
    assert!(
        !log_actions(&s).await.iter().any(|r| r.1.contains("note")),
        "notes are private"
    );
}
