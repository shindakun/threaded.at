mod common;

use axum::http::StatusCode;
use common::world::*;

#[tokio::test]
async fn mod_and_admin_need_their_roles() {
    let w = world().await;
    let user = w.user("user.example", &base(), 100, 0).await;
    let moderator = w.user("mod.example", &base(), 100, 0).await;
    let admin = w.user("admin.example", &base(), 100, 0).await;
    w.role("mod.example", "moderator").await;
    w.role("admin.example", "admin").await;

    let out = w.get("/mod", "").await;
    assert_eq!(out.location.as_deref(), Some("/login?next=/mod"));
    let out = w.get("/admin", "").await;
    assert_eq!(out.location.as_deref(), Some("/login?next=/admin"));

    let r = w.get("/mod", &user).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(r.body.contains("Moderators only."));
    assert_eq!(w.get("/admin", &user).await.status, StatusCode::FORBIDDEN);

    assert_eq!(w.get("/mod", &moderator).await.status, StatusCode::OK);
    let r = w.get("/admin", &moderator).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(r.body.contains("Admins only."));

    assert_eq!(w.get("/mod", &admin).await.status, StatusCode::OK);
    assert_eq!(w.get("/admin", &admin).await.status, StatusCode::OK);
}

#[tokio::test]
async fn header_links_follow_the_role() {
    let w = world().await;
    let user = w.user("user.example", &base(), 100, 0).await;
    let moderator = w.user("mod.example", &base(), 100, 0).await;
    let admin = w.user("admin.example", &base(), 100, 0).await;
    w.role("mod.example", "moderator").await;
    w.role("admin.example", "admin").await;

    let page = w.get("/", &user).await.body;
    assert!(!page.contains(r#"href="/mod""#) && !page.contains(r#"href="/admin""#));
    let page = w.get("/", &moderator).await.body;
    assert!(page.contains(r#"href="/mod""#) && !page.contains(r#"href="/admin""#));
    let page = w.get("/", &admin).await.body;
    assert!(page.contains(r#"href="/mod""#) && page.contains(r#"href="/admin""#));
}

#[tokio::test]
async fn a_revoked_or_banned_moderator_loses_access_at_once() {
    let w = world().await;
    let moderator = w.user("mod.example", &base(), 100, 0).await;
    w.role("mod.example", "moderator").await;
    assert_eq!(w.get("/mod", &moderator).await.status, StatusCode::OK);

    // The same session, after the role is revoked.
    w.role("mod.example", "user").await;
    assert_eq!(
        w.get("/mod", &moderator).await.status,
        StatusCode::FORBIDDEN
    );

    w.role("mod.example", "moderator").await;
    sqlx::query("UPDATE users SET banned_at = 1 WHERE handle = 'mod.example'")
        .execute(w.store.pool())
        .await
        .unwrap();
    assert_eq!(
        w.get("/mod", &moderator).await.status,
        StatusCode::FORBIDDEN
    );
}

/// A world with an author, a reader, and a moderator, and one story.
async fn setup() -> (World, String, String, String, String) {
    let w = world().await;
    let author = w.user("alice.example", &base(), 100, 0).await;
    let reader = w.user("bob.example", &base(), 100, 60).await;
    let moderator = w.user("mod.example", &base(), 100, 0).await;
    w.role("mod.example", "moderator").await;
    let id = w
        .submit(&author, "A story", "https://e.example/story", &["rust"])
        .await;
    (w, author, reader, moderator, id)
}

#[tokio::test]
async fn removal_hides_a_story_from_everyone_but_its_author_and_moderators() {
    let (w, author, reader, moderator, id) = setup().await;
    let url = format!("/mod/s/{id}");

    let r = w
        .post(&url, &moderator, &[("action", "remove"), ("reason", " ")])
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.body.contains("Give a reason."), "{}", r.body);
    let r = w
        .post(&url, &reader, &[("action", "remove"), ("reason", "x")])
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "readers cannot act");

    let r = w
        .post(
            &url,
            &moderator,
            &[("action", "remove"), ("reason", "Spam")],
        )
        .await;
    assert_eq!(
        r.location.as_deref(),
        Some(format!("{url}?done=1").as_str())
    );

    assert_eq!(
        w.get(&format!("/s/{id}"), &reader).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        w.get(&format!("/s/{id}"), "").await.status,
        StatusCode::NOT_FOUND
    );
    assert!(!w.get("/", &reader).await.body.contains("A story"));
    let own = w.get(&format!("/s/{id}"), &author).await;
    assert!(
        own.body.contains("Removed by a moderator: Spam"),
        "{}",
        own.body
    );
    assert!(w
        .get(&format!("/s/{id}"), &moderator)
        .await
        .body
        .contains("Removed by a moderator: Spam"));

    let log = w.get("/moderations", "").await;
    assert!(
        log.body.contains("mod.example: remove story"),
        "{}",
        log.body
    );
    assert!(log.body.contains("Spam"));

    w.post(
        &url,
        &moderator,
        &[("action", "restore"), ("reason", "Appeal upheld")],
    )
    .await;
    assert_eq!(
        w.get(&format!("/s/{id}"), &reader).await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_removed_comment_with_replies_stays_as_a_placeholder() {
    let (w, author, reader, moderator, id) = setup().await;
    let r = w
        .post(
            "/comment",
            &reader,
            &[("story", &id), ("text", "Rude words")],
        )
        .await;
    let parent = r
        .location
        .unwrap()
        .rsplit("#c-")
        .next()
        .unwrap()
        .to_string();
    w.post(
        "/comment",
        &author,
        &[
            ("story", &id),
            ("parent", &parent),
            ("text", "A calm reply"),
        ],
    )
    .await;
    w.post(
        &format!("/mod/c/{parent}"),
        &moderator,
        &[("action", "remove"), ("reason", "Unkind")],
    )
    .await;

    let page = w.get(&format!("/s/{id}"), &author).await.body;
    assert!(
        page.contains("[removed]") && !page.contains("Rude words"),
        "{page}"
    );
    assert!(page.contains("A calm reply"));
    let own = w.get(&format!("/s/{id}"), &reader).await.body;
    assert!(own.contains("Rude words") && own.contains("Removed by a moderator: Unkind"));
}

#[tokio::test]
async fn a_locked_story_takes_no_comments_votes_or_flags() {
    let (w, _author, reader, moderator, id) = setup().await;
    w.post(
        &format!("/mod/s/{id}"),
        &moderator,
        &[("action", "lock"), ("reason", "Flame war")],
    )
    .await;
    let page = w.get(&format!("/s/{id}"), &reader).await.body;
    assert!(page.contains("Locked: Flame war"));
    assert!(!page.contains("Add a comment"));
    let v = w
        .post("/vote", &reader, &[("kind", "story"), ("id", &id)])
        .await;
    assert!(v.body.contains("This story is locked."), "{}", v.body);
    let f = w
        .post(
            "/flag",
            &reader,
            &[("kind", "story"), ("id", &id), ("reason", "spam")],
        )
        .await;
    assert!(f.body.contains("This story is locked."));
    let c = w
        .post("/comment", &reader, &[("story", &id), ("text", "late")])
        .await;
    assert!(c.body.contains("This story is closed to new comments."));
}

#[tokio::test]
async fn merged_stories_redirect_and_unmerge() {
    let (w, author, _reader, moderator, a) = setup().await;
    let b = w
        .submit(&author, "Same thing", "https://e.example/other", &["rust"])
        .await;
    let r = w
        .post(
            &format!("/mod/s/{a}"),
            &moderator,
            &[
                ("action", "merge"),
                ("into", &format!("/s/{b}")),
                ("reason", "Duplicate"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    let r = w.get(&format!("/s/{a}"), "").await;
    assert_eq!(r.status, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(r.location.as_deref(), Some(format!("/s/{b}").as_str()));

    w.post(
        &format!("/mod/s/{a}"),
        &moderator,
        &[("action", "unmerge"), ("reason", "Different")],
    )
    .await;
    assert_eq!(w.get(&format!("/s/{a}"), "").await.status, StatusCode::OK);
}

#[tokio::test]
async fn a_moderator_edit_holds_against_the_authors_edit() {
    let (w, author, _reader, moderator, id) = setup().await;
    let r = w
        .post(
            &format!("/mod/s/{id}"),
            &moderator,
            &[
                ("action", "edit"),
                ("title", "A better title"),
                ("url", "https://e.example/story"),
                ("tags", "rust"),
                ("reason", "Editorialized"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    let page = w.get(&format!("/s/{id}"), "").await.body;
    assert!(page.contains("A better title") && page.contains("edited by a moderator"));

    let r = w
        .post(
            &format!("/s/{id}/edit"),
            &author,
            &[
                ("title", "MY TITLE"),
                ("url", "https://e.example/story"),
                ("text", "Notes."),
                ("tags", "rust"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    let page = w.get(&format!("/s/{id}"), "").await.body;
    assert!(page.contains("A better title") && !page.contains("MY TITLE"));
    assert!(page.contains("Notes."), "the author's text still changes");
    let put = w.writes().last().cloned().unwrap();
    assert_eq!(
        put.record.unwrap()["title"],
        "MY TITLE",
        "the author's record holds the author's title"
    );
}

#[tokio::test]
async fn bans_badges_notes_and_the_flagged_queue() {
    let (w, author, reader, moderator, id) = setup().await;
    let carol = w.user("carol.example", &base(), 100, 60).await;
    let dave = w.user("dave.example", &base(), 100, 60).await;
    for who in [&reader, &carol, &dave] {
        w.post(
            "/flag",
            who,
            &[("kind", "story"), ("id", &id), ("reason", "spam")],
        )
        .await;
    }
    let q = w.get("/mod", &moderator).await.body;
    assert!(q.contains("A story") && q.contains("Spam (3)"), "{q}");
    let review = w.get(&format!("/mod/s/{id}"), &moderator).await.body;
    assert!(
        review.contains("Spam by <a href=\"/mod/u/did:plc:bob\">bob.example</a>"),
        "moderators see flaggers"
    );

    let r = w
        .post(
            "/mod/u/did:plc:alice",
            &moderator,
            &[
                ("action", "badge"),
                ("label", "Maintainer"),
                ("link", "https://e.example/p"),
                ("reason", "Runs the project"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    w.post(
        "/mod/u/did:plc:alice",
        &moderator,
        &[("action", "note"), ("note", "Watch the next submission.")],
    )
    .await;
    let front = w.get("/", "").await.body;
    assert!(
        front.contains(r#"<a class="badge" href="https://e.example/p">Maintainer</a>"#),
        "{front}"
    );
    let review = w.get("/mod/u/did:plc:alice", &moderator).await.body;
    assert!(review.contains("Watch the next submission."));

    w.post(
        "/mod/u/did:plc:alice",
        &moderator,
        &[("action", "ban"), ("reason", "Spam account")],
    )
    .await;
    let r = w
        .post(
            "/submit",
            &author,
            &[
                ("title", "More"),
                ("url", "https://e.example/more"),
                ("tags", "rust"),
            ],
        )
        .await;
    assert!(
        r.body.contains("Your account is banned from posting."),
        "{}",
        r.body
    );
    let log = w.get("/moderations", "").await.body;
    assert!(log.contains("ban") && log.contains("grant badge") && !log.contains("Watch the next"));
}

#[tokio::test]
async fn moderator_forms_need_the_csrf_token() {
    let (w, _author, _reader, moderator, id) = setup().await;
    for uri in [format!("/mod/s/{id}"), "/mod/u/did:plc:alice".to_string()] {
        let r = w
            .send(
                axum::http::Request::post(uri.as_str())
                    .header(axum::http::header::COOKIE, &moderator)
                    .header(
                        axum::http::header::CONTENT_TYPE,
                        "application/x-www-form-urlencoded",
                    )
                    .body(axum::body::Body::from("csrf=wrong&action=remove&reason=x"))
                    .unwrap(),
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{uri}");
    }
}

#[tokio::test]
async fn suggestions_go_to_the_edits_tab_and_apply_on_accept() {
    let (w, author, reader, moderator, id) = setup().await;
    let low = w.user("low.example", &base(), 100, 0).await;
    let url = format!("/s/{id}/suggest");

    let r = w.get(&url, &low).await;
    assert!(
        r.body
            .contains("Suggesting edits needs karma of 10 or more."),
        "{}",
        r.body
    );
    assert!(w.get(&url, &author).await.body.contains("edit it instead"));
    assert!(w
        .get(&format!("/s/{id}"), &reader)
        .await
        .body
        .contains("Suggest an edit"));

    let r = w
        .post(&url, &reader, &[("title", "A story"), ("tags", "rust")])
        .await;
    assert!(r.body.contains("Nothing changed."), "{}", r.body);
    let r = w
        .post(
            &url,
            &reader,
            &[
                ("title", "A clearer title"),
                ("tags", "rust"),
                ("tags", "meta"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);

    let q = w.get("/mod?tab=edits", &moderator).await.body;
    assert!(
        q.contains("A clearer title") && q.contains("meta rust") && q.contains("bob.example"),
        "{q}"
    );
    let sugg_id = q
        .split("/mod/suggestions/")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_string();
    let r = w
        .post(
            &format!("/mod/suggestions/{sugg_id}"),
            &moderator,
            &[("decision", "accept"), ("reason", "Better")],
        )
        .await;
    assert_eq!(
        r.location.as_deref(),
        Some("/mod?tab=edits&done=1"),
        "{}",
        r.body
    );
    let page = w.get(&format!("/s/{id}"), "").await.body;
    assert!(page.contains("A clearer title") && page.contains("edited by a moderator"));
    assert!(w
        .get("/moderations", "")
        .await
        .body
        .contains("Better (suggested by bob.example)"));
    let again = w
        .post(
            &format!("/mod/suggestions/{sugg_id}"),
            &moderator,
            &[("decision", "dismiss"), ("reason", "x")],
        )
        .await;
    assert_eq!(
        again.status,
        StatusCode::NOT_FOUND,
        "a decided suggestion is no longer open"
    );
}

#[tokio::test]
async fn error_pages_keep_the_signed_in_header() {
    let w = world().await;
    let user = w.user("user.example", &base(), 100, 0).await;
    w.role("mod.example", "moderator").await;
    for (path, status) in [
        ("/mod", StatusCode::FORBIDDEN),
        ("/admin", StatusCode::FORBIDDEN),
        ("/s/nope00", StatusCode::NOT_FOUND),
    ] {
        let r = w.get(path, &user).await;
        assert_eq!(r.status, status, "{path}");
        assert!(
            r.body
                .contains(r#"<a class="who" href="/u/user.example">user.example</a>"#),
            "{path}: {}",
            r.body
        );
        assert!(
            !r.body.contains(r#"<a href="/login">Sign in</a>"#),
            "{path}"
        );
    }
    let signed_out = w.get("/s/nope00", "").await;
    assert!(signed_out.body.contains(r#"<a href="/login">Sign in</a>"#));
}

#[tokio::test]
async fn moderator_actions_name_the_account_by_did_not_by_handle() {
    let w = world().await;
    let moderator = w.user("mod.example", &base(), 100, 0).await;
    w.role("mod.example", "moderator").await;
    w.user("alice.example", &base(), 100, 0).await;

    // A handle in the address only finds the account, then moves to its DID.
    let r = w.get("/mod/u/alice.example", &moderator).await;
    assert_eq!(r.location.as_deref(), Some("/mod/u/did:plc:alice"));
    let page = w.get("/mod/u/did:plc:alice", &moderator).await.body;
    assert!(
        page.contains(r#"action="/mod/u/did:plc:alice""#),
        "forms post to the DID"
    );

    // An action addressed by handle is refused, whoever holds the handle now.
    let r = w
        .post(
            "/mod/u/alice.example",
            &moderator,
            &[("action", "ban"), ("reason", "Spam")],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.body.contains("by DID, not by handle"), "{}", r.body);
    let banned: Option<i64> =
        sqlx::query_scalar("SELECT banned_at FROM users WHERE did = 'did:plc:alice'")
            .fetch_one(w.store.pool())
            .await
            .unwrap();
    assert_eq!(banned, None);
}

#[tokio::test]
async fn a_handle_belongs_to_whichever_account_verified_it_last() {
    let w = world().await;
    let moderator = w.user("mod.example", &base(), 100, 0).await;
    w.role("mod.example", "moderator").await;
    w.user("alice.example", &base(), 100, 0).await;
    let id = w
        .submit(
            &w.user("alice.example", &base(), 100, 0).await,
            "Alice's story",
            "https://e.example/a",
            &["rust"],
        )
        .await;

    // alice.example moves to a different account.
    w.store
        .record_sign_in("did:plc:newowner", "alice.example", true, NOW)
        .await
        .unwrap();
    let old: (String, bool) =
        sqlx::query_as("SELECT handle, handle_verified FROM users WHERE did = 'did:plc:alice'")
            .fetch_one(w.store.pool())
            .await
            .unwrap();
    assert_eq!(
        old,
        ("alice.example".to_string(), false),
        "the old holder loses the verification"
    );

    // The handle now finds the new owner; the old account shows its DID.
    let r = w.get("/mod/u/alice.example", &moderator).await;
    assert_eq!(r.location.as_deref(), Some("/mod/u/did:plc:newowner"));
    let story = w.get(&format!("/s/{id}"), "").await.body;
    assert!(
        story.contains(r#"<a class="handle" href="/u/did:plc:alice">did:plc:alice</a>"#),
        "{story}"
    );
    let profile = w.get("/u/alice.example", "").await.body;
    assert!(
        !profile.contains("Alice&#39;s story") && !profile.contains("Alice's story"),
        "the new owner's profile does not show the old owner's posts"
    );
}

#[tokio::test]
async fn comment_review_removes_and_restores() {
    let (w, author, reader, moderator, id) = setup().await;
    let r = w
        .post(
            "/comment",
            &author,
            &[("story", &id), ("text", "Rude words")],
        )
        .await;
    let c = r
        .location
        .unwrap()
        .rsplit("#c-")
        .next()
        .unwrap()
        .to_string();
    let url = format!("/mod/c/{c}");

    assert_eq!(w.get(&url, &reader).await.status, StatusCode::FORBIDDEN);
    let page = w.get(&url, &moderator).await.body;
    assert!(
        page.contains("Review comment") && page.contains("Rude words"),
        "{page}"
    );
    assert!(page.contains(r#"value="remove""#) && !page.contains("Removed:"));
    assert_eq!(
        w.get("/mod/c/nope00", &moderator).await.status,
        StatusCode::NOT_FOUND
    );

    let r = w
        .post(
            &url,
            &moderator,
            &[("action", "remove"), ("reason", "Unkind")],
        )
        .await;
    assert_eq!(
        r.location.as_deref(),
        Some(format!("{url}?done=1").as_str())
    );
    let page = w.get(&url, &moderator).await.body;
    assert!(page.contains("Removed: Unkind") && page.contains(r#"value="restore""#));
    assert!(!w
        .get(&format!("/s/{id}"), &reader)
        .await
        .body
        .contains("Rude words"));
    let r = w
        .post(
            &url,
            &moderator,
            &[("action", "remove"), ("reason", "Again")],
        )
        .await;
    assert!(r.body.contains("It is already removed."), "{}", r.body);
    let r = w
        .post(&url, &moderator, &[("action", "lock"), ("reason", "x")])
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "comments have no lock");

    w.post(
        &url,
        &moderator,
        &[("action", "restore"), ("reason", "Fair")],
    )
    .await;
    assert!(w
        .get(&format!("/s/{id}"), &reader)
        .await
        .body
        .contains("Rude words"));
    let log = w.get("/moderations", "").await.body;
    assert!(
        log.contains("mod.example: remove comment") && log.contains("mod.example: restore comment"),
        "{log}"
    );
}

#[tokio::test]
async fn unban_and_badge_removal_are_logged() {
    let (w, author, _reader, moderator, _id) = setup().await;
    let user = "/mod/u/did:plc:alice";
    let r = w
        .post(user, &moderator, &[("action", "unban"), ("reason", "x")])
        .await;
    assert!(r.body.contains("That account is not banned."), "{}", r.body);
    w.post(user, &moderator, &[("action", "ban"), ("reason", "Spam")])
        .await;
    let r = w
        .post(
            user,
            &moderator,
            &[("action", "unban"), ("reason", "Appeal")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    let r = w
        .post(
            "/submit",
            &author,
            &[
                ("title", "Back"),
                ("url", "https://e.example/back"),
                ("tags", "rust"),
            ],
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::SEE_OTHER,
        "an unbanned account posts again"
    );

    w.post(
        user,
        &moderator,
        &[
            ("action", "badge"),
            ("label", "Maintainer"),
            ("reason", "Runs it"),
        ],
    )
    .await;
    let badge: i64 = sqlx::query_scalar("SELECT id FROM role_badges")
        .fetch_one(w.store.pool())
        .await
        .unwrap();
    // A badge is removed only from the account that holds it.
    w.user("carol.example", &base(), 100, 0).await;
    let r = w
        .post(
            "/mod/u/did:plc:carol",
            &moderator,
            &[
                ("action", "unbadge"),
                ("badge", &badge.to_string()),
                ("reason", "x"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = w
        .post(user, &moderator, &[("action", "unbadge"), ("reason", "x")])
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "no badge named");
    let r = w
        .post(
            user,
            &moderator,
            &[
                ("action", "unbadge"),
                ("badge", &badge.to_string()),
                ("reason", "Stepped down"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    assert!(!w.get("/", "").await.body.contains("Maintainer"));
    let r = w
        .post(user, &moderator, &[("action", "shout"), ("reason", "x")])
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let log = w.get("/moderations", "").await.body;
    assert!(
        log.contains("mod.example: unban") && log.contains("mod.example: remove badge"),
        "{log}"
    );
}

#[tokio::test]
async fn a_dismissed_suggestion_changes_nothing_and_is_logged() {
    let (w, _author, reader, moderator, id) = setup().await;
    let r = w
        .post(
            &format!("/s/{id}/suggest"),
            &reader,
            &[("title", "A worse title"), ("tags", "rust")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    let q = w.get("/mod?tab=edits", &moderator).await.body;
    let sugg = q
        .split("/mod/suggestions/")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_string();
    let url = format!("/mod/suggestions/{sugg}");
    let r = w
        .post(&url, &moderator, &[("decision", "shelve"), ("reason", "x")])
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = w
        .post(
            &url,
            &moderator,
            &[("decision", "dismiss"), ("reason", " ")],
        )
        .await;
    assert!(r.body.contains("Give a reason."), "{}", r.body);
    let r = w
        .post(
            &url,
            &moderator,
            &[("decision", "dismiss"), ("reason", "Less clear")],
        )
        .await;
    assert_eq!(r.location.as_deref(), Some("/mod?tab=edits&done=1"));
    let page = w.get(&format!("/s/{id}"), "").await.body;
    assert!(page.contains("A story") && !page.contains("A worse title"));
    assert!(w
        .get("/mod?tab=edits", &moderator)
        .await
        .body
        .contains("No suggested edits."));
    let log = w.get("/moderations", "").await.body;
    assert!(
        log.contains("dismiss suggestion") && log.contains("Less clear"),
        "{log}"
    );
}

#[tokio::test]
async fn merge_and_edit_refuse_what_they_cannot_do() {
    let (w, _author, _reader, moderator, id) = setup().await;
    let url = format!("/mod/s/{id}");
    let r = w
        .post(
            &url,
            &moderator,
            &[
                ("action", "merge"),
                ("into", "/s/nope00"),
                ("reason", "Dupe"),
            ],
        )
        .await;
    assert!(
        r.body.contains("There is no story with that ID."),
        "{}",
        r.body
    );

    let edit = |extra: Vec<(&'static str, &'static str)>| {
        let mut f = vec![("action", "edit"), ("reason", "Tidy")];
        f.extend(extra);
        f
    };
    for (form, want) in [
        (
            edit(vec![("title", " "), ("tags", "rust")]),
            "A title is 1 to 150 characters.",
        ),
        (
            edit(vec![
                ("title", "T"),
                ("url", "ftp://e.example/"),
                ("tags", "rust"),
            ]),
            "The link is not usable",
        ),
        (
            edit(vec![("title", "T"), ("url", ""), ("tags", "rust")]),
            "A story without text needs a link.",
        ),
        (
            edit(vec![
                ("title", "T"),
                ("url", "https://e.example/story"),
                ("tags", "nosuch"),
            ]),
            "There is no tag",
        ),
        (
            edit(vec![("title", "T"), ("url", "https://e.example/story")]),
            "Pick 1 to 5 tags.",
        ),
    ] {
        let r = w.post(&url, &moderator, &form).await;
        assert!(r.body.contains(want), "{want}: {}", r.body);
    }
    let r = w
        .post(&url, &moderator, &[("action", "explode"), ("reason", "x")])
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let (title, mod_edited): (String, bool) =
        sqlx::query_as("SELECT title, override_by IS NOT NULL FROM stories WHERE short_id = ?")
            .bind(&id)
            .fetch_one(w.store.pool())
            .await
            .unwrap();
    assert_eq!((title.as_str(), mod_edited), ("A story", false));
}
