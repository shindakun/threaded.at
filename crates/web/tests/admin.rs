mod common;

use axum::http::StatusCode;
use common::world::*;

async fn setup() -> (World, String, String) {
    let w = world().await;
    let admin = w.user("admin.example", &base(), 100, 0).await;
    w.role("admin.example", "admin").await;
    let moderator = w.user("mod.example", &base(), 100, 0).await;
    w.role("mod.example", "moderator").await;
    (w, admin, moderator)
}

#[tokio::test]
async fn admin_pages_are_for_admins() {
    let (w, admin, moderator) = setup().await;
    for page in ["/admin", "/admin/settings", "/admin/tags", "/admin/people"] {
        assert_eq!(w.get(page, &admin).await.status, StatusCode::OK, "{page}");
        assert_eq!(
            w.get(page, &moderator).await.status,
            StatusCode::FORBIDDEN,
            "{page}"
        );
    }
    let r = w
        .post(
            "/admin/settings",
            &moderator,
            &[("lock_days", "1"), ("reason", "x")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(w
        .get("/admin", &admin)
        .await
        .body
        .contains("Off: no API key is configured."));
}

#[tokio::test]
async fn settings_take_effect_at_once_and_are_logged() {
    let (w, admin, _) = setup().await;
    let newbie = w.user("new.example", &base(), 1, 0).await;

    let r = w
        .post(
            "/admin/settings",
            &admin,
            &[("new_stories_per_day", "ten"), ("reason", "x")],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.body.contains("is a number from 0 to 100"), "{}", r.body);
    let r = w
        .post("/admin/settings", &admin, &[("new_stories_per_day", "1")])
        .await;
    assert!(r.body.contains("Give a reason."), "{}", r.body);

    let r = w
        .post(
            "/admin/settings",
            &admin,
            &[("new_stories_per_day", "1"), ("reason", "Spam wave")],
        )
        .await;
    assert_eq!(r.location.as_deref(), Some("/admin/settings?done=1"));
    let page = w.get("/admin/settings", &admin).await.body;
    assert!(
        page.contains(r#"name="new_stories_per_day" type="text" value="1""#),
        "{page}"
    );

    w.submit(&newbie, "First", "https://e.example/1", &["rust"])
        .await;
    let r = w
        .post(
            "/submit",
            &newbie,
            &[
                ("title", "Second"),
                ("url", "https://e.example/2"),
                ("tags", "rust"),
            ],
        )
        .await;
    assert!(
        r.body.contains("You can submit 1 story a day."),
        "{}",
        r.body
    );

    let log = w.get("/moderations", "").await.body;
    assert!(
        log.contains("change setting") && log.contains("new_stories_per_day: 2 to 1"),
        "{log}"
    );
    let stored = w.store.settings().await.unwrap();
    assert_eq!(
        stored,
        vec![("new_stories_per_day".to_string(), "1".to_string())]
    );
}

#[tokio::test]
async fn tags_are_created_and_edited() {
    let (w, admin, _) = setup().await;
    let user = w.user("user.example", &base(), 100, 0).await;
    let r = w
        .post(
            "/admin/tags",
            &admin,
            &[
                ("name", "announce"),
                ("description", "Site news"),
                ("rank_boost", "0.5"),
                ("privileged", "1"),
                ("new_users_ok", "1"),
                ("active", "1"),
                ("reason", "For site news"),
            ],
        )
        .await;
    assert_eq!(
        r.location.as_deref(),
        Some("/admin/tags?done=1"),
        "{}",
        r.body
    );
    let r = w
        .post(
            "/submit",
            &user,
            &[
                ("title", "T"),
                ("url", "https://e.example/a"),
                ("tags", "announce"),
            ],
        )
        .await;
    assert!(r.body.contains("Only moderators can use"), "{}", r.body);

    let r = w
        .post(
            "/admin/tags",
            &admin,
            &[
                ("name", "announce"),
                ("description", "Site news"),
                ("rank_boost", "0.5"),
                ("privileged", "1"),
                ("new_users_ok", "1"),
                ("active", "1"),
                ("reason", "x"),
            ],
        )
        .await;
    assert!(r.body.contains("Nothing changed."));
    let r = w
        .post(
            "/admin/tags",
            &admin,
            &[("name", "Bad Name"), ("reason", "x")],
        )
        .await;
    assert!(r.body.contains("A tag name is lowercase"));

    w.post(
        "/admin/tags",
        &admin,
        &[
            ("name", "announce"),
            ("description", "Site news"),
            ("rank_boost", "0.5"),
            ("new_users_ok", "1"),
            ("active", "1"),
            ("reason", "Open it up"),
        ],
    )
    .await;
    w.submit(&user, "Now allowed", "https://e.example/b", &["announce"])
        .await;
    let log = w.get("/moderations", "").await.body;
    assert!(
        log.contains("create tag") && log.contains("privileged: true to false"),
        "{log}"
    );
}

#[tokio::test]
async fn roles_are_granted_by_did_and_never_to_yourself() {
    let (w, admin, _) = setup().await;
    let carol = w.user("carol.example", &base(), 100, 0).await;
    assert_eq!(w.get("/mod", &carol).await.status, StatusCode::FORBIDDEN);

    let r = w
        .post(
            "/admin/people",
            &admin,
            &[
                ("did", "carol.example"),
                ("role", "moderator"),
                ("reason", "x"),
            ],
        )
        .await;
    assert!(r.body.contains("Roles take a DID"));
    let r = w
        .post(
            "/admin/people",
            &admin,
            &[("did", "did:plc:admin"), ("role", "user"), ("reason", "x")],
        )
        .await;
    assert!(r.body.contains("You cannot change your own role."));
    let r = w
        .post(
            "/admin/people",
            &admin,
            &[
                ("did", "did:plc:nobody"),
                ("role", "moderator"),
                ("reason", "x"),
            ],
        )
        .await;
    assert!(r.body.contains("Nobody with that DID has signed in yet."));

    let r = w
        .post(
            "/admin/people",
            &admin,
            &[
                ("did", "did:plc:carol"),
                ("role", "moderator"),
                ("reason", "Trusted regular"),
            ],
        )
        .await;
    assert_eq!(r.location.as_deref(), Some("/admin/people?done=1"));
    assert_eq!(w.get("/mod", &carol).await.status, StatusCode::OK);
    assert!(w
        .get("/admin/people", &admin)
        .await
        .body
        .contains("carol.example"));

    w.post(
        "/admin/people",
        &admin,
        &[
            ("did", "did:plc:carol"),
            ("role", "user"),
            ("reason", "Stepped down"),
        ],
    )
    .await;
    assert_eq!(w.get("/mod", &carol).await.status, StatusCode::FORBIDDEN);
    let log = w.get("/moderations", "").await.body;
    assert!(
        log.contains("grant role") && log.contains("revoke role") && log.contains("admin.example"),
        "{log}"
    );
}

#[tokio::test]
async fn the_triage_breaker_opens_and_an_admin_resets_it() {
    let w = world_with_triage().await;
    let admin = w.user("admin.example", &base(), 100, 0).await;
    w.role("admin.example", "admin").await;
    let moderator = w.user("mod.example", &base(), 100, 0).await;
    w.role("mod.example", "moderator").await;
    w.user("alice.example", &base(), 100, 0).await;
    let author: i64 = sqlx::query_scalar("SELECT id FROM users WHERE did = 'did:plc:alice'")
        .fetch_one(w.store.pool())
        .await
        .unwrap();
    // Twenty holds that moderators released: precision 0, under the floor.
    for i in 0..threaded_store::BREAKER_MIN_DECIDED {
        let link = threaded_core::link::parse(&format!("https://e.example/b{i}")).unwrap();
        let short = w
            .store
            .insert_story(&threaded_store::NewStory {
                user_id: author,
                title: "Held",
                link: Some(&link),
                body: "",
                user_is_author: false,
                tags: &["rust"],
                created_at: NOW - 100,
                record: None,
            })
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO triage_verdicts (story_id, verdict, held, decision, decided_at, created_at) \
             SELECT id, 'spam', 1, 'released', ?, ? FROM stories WHERE short_id = ?",
        )
        .bind(NOW - 10)
        .bind(NOW - 50)
        .bind(&short)
        .execute(w.store.pool())
        .await
        .unwrap();
    }
    let page = w.get("/admin", &admin).await.body;
    assert!(
        page.contains("Off: moderators released too many holds."),
        "{page}"
    );
    assert!(page.contains(r#"action="/admin/breaker""#));

    let r = w
        .post("/admin/breaker", &moderator, &[("reason", "x")])
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "admins only");
    let r = w.post("/admin/breaker", &admin, &[("reason", " ")]).await;
    assert!(r.body.contains("Give a reason."), "{}", r.body);
    let r = w
        .post("/admin/breaker", &admin, &[("reason", "Rules rewritten")])
        .await;
    assert_eq!(r.location.as_deref(), Some("/admin?done=1"));
    let page = w.get("/admin", &admin).await.body;
    assert!(
        page.contains("<dt>Automatic holds</dt><dd>On</dd>"),
        "{page}"
    );
    assert!(!page.contains(r#"action="/admin/breaker""#));
    let log = w.get("/moderations", "").await.body;
    assert!(log.contains("admin.example: reset triage breaker"), "{log}");
}

#[tokio::test]
async fn a_role_already_held_or_unknown_is_refused() {
    let (w, admin, _) = setup().await;
    w.user("carol.example", &base(), 100, 0).await;
    let r = w
        .post(
            "/admin/people",
            &admin,
            &[
                ("did", "did:plc:mod"),
                ("role", "moderator"),
                ("reason", "x"),
            ],
        )
        .await;
    assert!(
        r.body.contains("They already hold that role."),
        "{}",
        r.body
    );
    let r = w
        .post(
            "/admin/people",
            &admin,
            &[
                ("did", "did:plc:carol"),
                ("role", "emperor"),
                ("reason", "x"),
            ],
        )
        .await;
    assert!(r.body.contains("Pick a role."), "{}", r.body);
    let r = w
        .post(
            "/admin/people",
            &admin,
            &[
                ("did", "did:plc:carol"),
                ("role", "moderator"),
                ("reason", " "),
            ],
        )
        .await;
    assert!(r.body.contains("Give a reason."), "{}", r.body);
}
