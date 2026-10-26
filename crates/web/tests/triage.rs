mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use common::world::*;
use threaded_core::policy::Policy;
use threaded_triage::{step, Fake, Step, Triager};

#[tokio::test]
async fn without_a_key_nothing_is_queued() {
    let w = world().await;
    let admin = w.user("admin.example", &base(), 100, 0).await;
    w.role("admin.example", "admin").await;
    w.post(
        "/admin/rules",
        &admin,
        &[("body", "No spam."), ("reason", "First")],
    )
    .await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    w.submit(&alice, "A story", "https://e.example/a", &["rust"])
        .await;
    let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM triage_queue")
        .fetch_one(w.store.pool())
        .await
        .unwrap();
    assert_eq!(queued, 0);
    assert!(w
        .get("/admin", &admin)
        .await
        .body
        .contains("Off: no API key is configured."));
}

#[tokio::test]
async fn a_held_story_waits_for_a_moderator() {
    let w = world_with_triage().await;
    let admin = w.user("admin.example", &base(), 100, 0).await;
    w.role("admin.example", "admin").await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let reader = w.user("bob.example", &base(), 100, 0).await;

    assert!(w
        .get("/admin", &admin)
        .await
        .body
        .contains("save the site rules to turn triage on"));
    w.submit(&alice, "Before rules", "https://e.example/0", &["rust"])
        .await;
    let r = w
        .post(
            "/admin/rules",
            &admin,
            &[
                ("body", "No spam: no storefront links."),
                ("reason", "First rules"),
            ],
        )
        .await;
    assert_eq!(r.location.as_deref(), Some("/admin/rules?done=1"));
    assert!(w.get("/admin", &admin).await.body.contains("On."));

    let id = w
        .submit(&alice, "BUY WATCHES", "https://shop.example/w", &["rust"])
        .await;
    let fake = Arc::new(Fake::default());
    let t = Triager::Fake(fake.clone());
    fake.reply("spam", "high");
    assert_eq!(
        step(&w.store, &t, &Policy::default(), NOW).await.unwrap(),
        Step::Judged {
            verdict: "spam".into(),
            held: true
        }
    );
    assert_eq!(
        fake.requests.lock().unwrap().len(),
        1,
        "only the story posted after the rules was queued"
    );

    assert_eq!(
        w.get(&format!("/s/{id}"), &reader).await.status,
        StatusCode::NOT_FOUND
    );
    assert!(w
        .get(&format!("/s/{id}"), &alice)
        .await
        .body
        .contains("Held for review: spam (automated)"));
    let holds = w.get("/mod", &admin).await.body;
    assert!(
        holds.contains("BUY WATCHES")
            && holds.contains("Triage: spam")
            && holds.contains("Confirm hold"),
        "{holds}"
    );
    let review = w.get(&format!("/mod/s/{id}"), &admin).await.body;
    assert!(
        review.contains("Model claude-opus-5-5, rules version 1"),
        "{review}"
    );

    let vid = holds
        .split("/mod/verdicts/")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_string();
    let r = w
        .post(
            &format!("/mod/verdicts/{vid}"),
            &admin,
            &[
                ("decision", "released"),
                ("tab", "holds"),
                ("reason", "A real shop review"),
            ],
        )
        .await;
    assert_eq!(
        r.location.as_deref(),
        Some("/mod?tab=holds&done=1"),
        "{}",
        r.body
    );
    assert_eq!(
        w.get(&format!("/s/{id}"), &reader).await.status,
        StatusCode::OK
    );

    let log = w.get("/moderations", "").await.body;
    assert!(
        log.contains("triage: hold story") && log.contains("admin.example: release story"),
        "{log}"
    );
    let stats = w.get("/moderations/stats", "").await.body;
    assert!(stats.contains("0% of the last 1 decided holds"), "{stats}");
}

#[tokio::test]
async fn authors_can_keep_their_posts_out_of_triage() {
    let w = world_with_triage().await;
    let admin = w.user("admin.example", &base(), 100, 0).await;
    w.role("admin.example", "admin").await;
    w.post(
        "/admin/rules",
        &admin,
        &[("body", "No spam."), ("reason", "First")],
    )
    .await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let r = w
        .post("/settings/triage", &alice, &[("opt_out", "1")])
        .await;
    assert_eq!(r.location.as_deref(), Some("/settings?notice=saved"));
    assert!(w
        .get("/settings", &alice)
        .await
        .body
        .contains(r#"name="opt_out" value="1" checked"#));

    w.submit(&alice, "Mine", "https://e.example/m", &["rust"])
        .await;
    let fake = Arc::new(Fake::default());
    step(
        &w.store,
        &Triager::Fake(fake.clone()),
        &Policy::default(),
        NOW,
    )
    .await
    .unwrap();
    assert!(fake.requests.lock().unwrap().is_empty(), "no model call");
    let q = w.get("/mod?tab=suggestions", &admin).await.body;
    assert!(
        q.contains("Mine")
            && q.contains("keeps their posts out of AI triage")
            && q.contains("No model call"),
        "{q}"
    );
}
