//! What moderators see of the indexer: the Detached tab, stream lag on
//! `/admin`, and an account's identity on the user review page.

mod common;

use axum::extract::Path;
use axum::routing::get;
use axum::{Json, Router};
use common::world::*;
use threaded_store::StoryEdit;

/// Detaches story `id` as the indexer would when its author moves the link.
async fn detach(w: &World, id: &str, to: &str) {
    let story = w.store.story_any(id).await.unwrap().unwrap();
    let link = threaded_core::link::parse(to).unwrap();
    w.store
        .detach_story(
            &story,
            &StoryEdit {
                title: &story.title,
                link: Some(&link),
                body: &story.body,
                tags: &story.tag_list(),
                cid: None,
            },
            NOW,
        )
        .await
        .unwrap();
}

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
async fn a_detached_story_waits_in_its_tab_until_reattached() {
    let (w, author, reader, moderator, id) = setup().await;
    detach(&w, &id, "https://e.example/moved").await;

    let page = format!("/s/{id}");
    assert_eq!(w.get(&page, &reader).await.status.as_u16(), 404);
    assert!(!w.get("/", &reader).await.body.contains("A story"));
    let own = w.get(&page, &author).await.body;
    assert!(
        own.contains("Hidden for review: the link changed after the edit window"),
        "{own}"
    );

    let log = w.get("/moderations", "").await.body;
    assert!(log.contains("indexer: detach story"), "{log}");
    assert!(log.contains("from https://e.example/story to https://e.example/moved"));

    let tab = w.get("/mod?tab=detached", &moderator).await.body;
    assert!(tab.contains("A story"), "{tab}");
    assert!(tab.contains("Link changed from https://e.example/story to https://e.example/moved"));
    assert!(
        tab.contains(r#"Detached <span class="count">1</span>"#),
        "{tab}"
    );
    let item = w.get(&format!("/mod/s/{id}"), &moderator).await.body;
    assert!(
        item.contains("Detached: the author changed the link"),
        "{item}"
    );
    assert!(item.contains(r#"value="reattach""#));

    // Readers cannot reattach; a moderator needs a reason.
    let action = format!("/mod/s/{id}");
    let r = w
        .post(&action, &reader, &[("action", "reattach"), ("reason", "x")])
        .await;
    assert_eq!(r.status.as_u16(), 403);
    let r = w
        .post(
            &action,
            &moderator,
            &[("action", "reattach"), ("reason", " ")],
        )
        .await;
    assert!(r.body.contains("Give a reason."), "{}", r.body);

    let r = w
        .post(
            &action,
            &moderator,
            &[
                ("action", "reattach"),
                ("reason", "Same article, new address"),
                ("from", "detached"),
            ],
        )
        .await;
    assert_eq!(r.location.as_deref(), Some("/mod?tab=detached&done=1"));
    let shown = w.get(&page, &reader).await;
    assert_eq!(shown.status.as_u16(), 200);
    assert!(shown.body.contains("https://e.example/moved"));
    let log = w.get("/moderations", "").await.body;
    assert!(log.contains("mod.example: reattach story"), "{log}");
    assert!(w
        .get("/mod?tab=detached", &moderator)
        .await
        .body
        .contains("No detached stories."));

    let r = w
        .post(
            &action,
            &moderator,
            &[("action", "reattach"), ("reason", "again")],
        )
        .await;
    assert!(r.body.contains("It is not detached."), "{}", r.body);
}

#[tokio::test]
async fn removing_a_detached_story_takes_it_off_the_tab_until_restored() {
    let (w, _author, _reader, moderator, id) = setup().await;
    detach(&w, &id, "https://e.example/moved").await;
    let action = format!("/mod/s/{id}");
    let r = w
        .post(
            &action,
            &moderator,
            &[
                ("action", "remove"),
                ("reason", "Bait"),
                ("from", "detached"),
            ],
        )
        .await;
    assert_eq!(r.location.as_deref(), Some("/mod?tab=detached&done=1"));
    assert!(w
        .get("/mod?tab=detached", &moderator)
        .await
        .body
        .contains("No detached stories."));

    // Restoring undoes the removal only; the story is still detached.
    w.post(
        &action,
        &moderator,
        &[("action", "restore"), ("reason", "Oops")],
    )
    .await;
    assert!(w
        .get("/mod?tab=detached", &moderator)
        .await
        .body
        .contains("A story"));
}

/// A hydrant on a Unix socket that answers `/stream/head` and one repo.
async fn fake_hydrant(dir: &std::path::Path, head: Option<u64>) -> std::path::PathBuf {
    let socket = dir.join("api.sock");
    let app = Router::new()
        .route(
            "/stream/head",
            get(move || async move { Json(serde_json::json!({ "id": head })) }),
        )
        .route(
            "/repos/{did}",
            get(|Path(did): Path<String>| async move {
                if did == DID {
                    Ok(Json(serde_json::json!({
                        "did": did,
                        "status": "active",
                        "handle": "carol.example",
                        "pds": "https://pds.example.com/",
                        "tracked": true,
                    })))
                } else {
                    Err(axum::http::StatusCode::NOT_FOUND)
                }
            }),
        );
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    socket
}

/// A DID in the PLC directory's form.
const DID: &str = "did:plc:z72i7hdynmk6r22z27h6tvur";

#[tokio::test]
async fn admin_shows_the_indexer_off_without_hydrant() {
    let w = world().await;
    let admin = w.user("admin.example", &base(), 100, 0).await;
    w.role("admin.example", "admin").await;
    let page = w.get("/admin", &admin).await.body;
    assert!(
        page.contains("Off: THREADED_HYDRANT_SOCKET is not set"),
        "{page}"
    );
    assert!(page.contains("none read yet"));
}

#[tokio::test]
async fn admin_shows_how_far_the_indexer_is_behind() {
    let dir = tempfile::tempdir().unwrap();
    let socket = fake_hydrant(dir.path(), Some(120)).await;
    let w = world_custom(NOW, None, |s| {
        s.hydrant = Some(threaded_indexer::client::Hydrant::new(&socket))
    })
    .await;
    let admin = w.user("admin.example", &base(), 100, 0).await;
    w.role("admin.example", "admin").await;
    w.store.set_stream_cursor(100, NOW - 120).await.unwrap();
    let page = w.get("/admin", &admin).await.body;
    assert!(page.contains("<dt>State</dt><dd>On.</dd>"), "{page}");
    assert!(page.contains("<dd>120</dd>"));
    assert!(page.contains("<dd>100</dd>"));
    assert!(page.contains("Up to 21 events"), "{page}");
    assert!(page.contains("2 minutes ago"), "{page}");

    w.store.set_stream_cursor(121, NOW).await.unwrap();
    let page = w.get("/admin", &admin).await.body;
    assert!(page.contains("Caught up"), "{page}");
}

#[tokio::test]
async fn admin_says_when_hydrant_does_not_answer() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("missing.sock");
    let w = world_custom(NOW, None, |s| {
        s.hydrant = Some(threaded_indexer::client::Hydrant::new(&socket))
    })
    .await;
    let admin = w.user("admin.example", &base(), 100, 0).await;
    w.role("admin.example", "admin").await;
    let page = w.get("/admin", &admin).await.body;
    assert!(page.contains("hydrant did not answer"), "{page}");
}

#[tokio::test]
async fn user_review_shows_identity_from_hydrant_and_the_plc_directory() {
    let dir = tempfile::tempdir().unwrap();
    let socket = fake_hydrant(dir.path(), None).await;
    let w = world_custom(NOW, None, |s| {
        s.hydrant = Some(threaded_indexer::client::Hydrant::new(&socket))
    })
    .await;
    let moderator = w.user("mod.example", &base(), 100, 0).await;
    w.role("mod.example", "moderator").await;
    w.store
        .record_sign_in(DID, "carol.example", true, NOW - 30 * DAY)
        .await
        .unwrap();
    w.store
        .record_sign_in(DID, "carol.example.net", true, NOW - 2 * DAY)
        .await
        .unwrap();
    // 2023-11-09T21:49:10Z.
    w.plc.lock().unwrap().insert(DID.into(), 1_699_566_550);

    let page = w.get(&format!("/mod/u/{DID}"), &moderator).await.body;
    assert!(page.contains("carol.example.net, verified"), "{page}");
    assert!(page.contains("<dd>pds.example.com</dd>"), "{page}");
    assert!(page.contains("2023-11-09"), "{page}");
    assert!(page.contains("from the PLC directory"));
    let newer = page.find("carol.example.net <span").unwrap();
    let older = page.find("carol.example <span").unwrap();
    assert!(newer < older, "newest handle first");

    // Unknown to both, and a DID that is not a did:plc.
    w.plc.lock().unwrap().clear();
    w.user("dave.example", &base(), 10, 0).await;
    let page = w.get("/mod/u/did:plc:dave", &moderator).await.body;
    assert!(
        page.contains("hydrant does not know this account"),
        "{page}"
    );
    assert!(
        page.contains("only a did:plc has a creation time"),
        "{page}"
    );
    let page = w.get(&format!("/mod/u/{DID}"), &moderator).await.body;
    assert!(
        page.contains("the PLC directory does not know this DID"),
        "{page}"
    );
}

#[tokio::test]
async fn user_review_works_with_the_indexer_off() {
    let w = world().await;
    let moderator = w.user("mod.example", &base(), 100, 0).await;
    w.role("mod.example", "moderator").await;
    let page = w.get("/mod/u/did:plc:mod", &moderator).await.body;
    assert!(page.contains("The indexer is off."), "{page}");
    assert!(page.contains("(100 days)"), "{page}");
    assert!(page.contains("mod.example <span"), "{page}");
    w.user("new.example", &base(), 1, 0).await;
    let page = w.get("/mod/u/did:plc:new", &moderator).await.body;
    assert!(page.contains("(1 day)"), "{page}");
}

#[tokio::test]
async fn the_author_finds_hidden_posts_on_their_profile() {
    let (w, author, reader, moderator, id) = setup().await;
    let r = w
        .post(
            "/comment",
            &author,
            &[("story", &id), ("text", "Off the rails")],
        )
        .await;
    let c = r
        .location
        .unwrap()
        .rsplit("#c-")
        .next()
        .unwrap()
        .to_string();
    w.post(
        &format!("/mod/c/{c}"),
        &moderator,
        &[("action", "remove"), ("reason", "Unkind")],
    )
    .await;
    detach(&w, &id, "https://e.example/moved").await;

    let own = w.get("/u/alice.example", &author).await.body;
    assert!(own.contains("Hidden from others"), "{own}");
    assert!(
        own.contains(&format!(r#"<a href="/s/{id}">A story</a>"#)),
        "{own}"
    );
    assert!(own.contains("Hidden for review: the link changed after the edit window"));
    assert!(own.contains(&format!(
        r#"<a href="/s/{id}#c-{c}">Comment on A story</a>"#
    )));
    assert!(own.contains("Removed by a moderator: Unkind"));

    for who in [&reader, &moderator, ""] {
        let page = w.get("/u/alice.example", who).await.body;
        assert!(
            !page.contains("Hidden from others"),
            "only the author sees it"
        );
    }

    // Reattached, the story leaves the list; the removed comment stays.
    w.post(
        &format!("/mod/s/{id}"),
        &moderator,
        &[("action", "reattach"), ("reason", "Fine")],
    )
    .await;
    let own = w.get("/u/alice.example", &author).await.body;
    assert!(!own.contains("the link changed after the edit window"));
    assert!(own.contains("Removed by a moderator: Unkind"));
}
