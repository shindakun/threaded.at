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
        review.contains("Spam by <a href=\"/mod/u/bob.example\">bob.example</a>"),
        "moderators see flaggers"
    );

    let r = w
        .post(
            "/mod/u/alice.example",
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
        "/mod/u/alice.example",
        &moderator,
        &[("action", "note"), ("note", "Watch the next submission.")],
    )
    .await;
    let front = w.get("/", "").await.body;
    assert!(
        front.contains(r#"<a class="badge" href="https://e.example/p">Maintainer</a>"#),
        "{front}"
    );
    let review = w.get("/mod/u/alice.example", &moderator).await.body;
    assert!(review.contains("Watch the next submission."));

    w.post(
        "/mod/u/alice.example",
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
    for uri in [format!("/mod/s/{id}"), "/mod/u/alice.example".to_string()] {
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
