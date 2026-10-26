use std::sync::Arc;

use threaded_core::policy::Policy;
use threaded_store::{By, Context, Source, Store, StoryInput, Target, UserRow};
use threaded_triage::{step, Fake, Step, Triager};

const NOW: i64 = 1_790_000_000;

async fn setup() -> (Store, UserRow, UserRow, Arc<Fake>, Triager) {
    let s = Store::open("sqlite::memory:").await.unwrap();
    s.upsert_tag("rust", "", 0.0).await.unwrap();
    s.record_sign_in("did:plc:alice", "alice.example", true, NOW - 100 * 86_400)
        .await
        .unwrap();
    s.record_sign_in("did:plc:mod", "mod.example", true, NOW)
        .await
        .unwrap();
    let alice = s.user_by_did("did:plc:alice").await.unwrap().unwrap();
    let m = s.user_by_did("did:plc:mod").await.unwrap().unwrap();
    let fake = Arc::new(Fake::default());
    (s, alice, m, fake.clone(), Triager::Fake(fake))
}

async fn post(s: &Store, u: &UserRow, n: usize, triage: bool) -> (i64, String) {
    // The breaker test posts more than a day's limit.
    let p = Policy {
        member_limits: (1_000, 1_000),
        ..Policy::default()
    };
    let tags = vec!["rust".to_string()];
    let url = format!("https://e.example/{n}");
    let a = s
        .accept_story(
            u,
            &StoryInput {
                title: "Story",
                url: &url,
                text: "",
                tags: &tags,
                authored: false,
            },
            (&format!("at://a/s/{n}"), "cid"),
            &Context {
                policy: &p,
                source: Source::Web,
                triage,
                now: NOW,
            },
        )
        .await
        .unwrap()
        .unwrap();
    (a.id, a.short_id)
}

async fn rules(s: &Store, m: &UserRow) {
    s.save_rules("No spam.", By::Moderator(m.id), "First rules", NOW)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn nothing_is_queued_while_triage_is_off_and_nothing_runs_without_rules() {
    let (s, alice, m, fake, t) = setup().await;
    let p = Policy::default();
    post(&s, &alice, 1, true).await;
    assert_eq!(step(&s, &t, &p, NOW).await.unwrap(), Step::NoRules);
    assert!(fake.requests.lock().unwrap().is_empty());
    rules(&s, &m).await;
    assert!(matches!(
        step(&s, &t, &p, NOW).await.unwrap(),
        Step::Judged { .. }
    ));
    assert_eq!(fake.requests.lock().unwrap().len(), 1);

    post(&s, &alice, 2, false).await;
    assert_eq!(
        step(&s, &t, &p, NOW).await.unwrap(),
        Step::Idle,
        "triage off queued nothing"
    );
}

#[tokio::test]
async fn high_confidence_spam_is_held_and_logged_by_triage() {
    let (s, alice, m, fake, t) = setup().await;
    let p = Policy::default();
    rules(&s, &m).await;
    let (id, sid) = post(&s, &alice, 1, true).await;
    fake.reply("spam", "high");
    assert_eq!(
        step(&s, &t, &p, NOW).await.unwrap(),
        Step::Judged {
            verdict: "spam".into(),
            held: true
        }
    );
    assert!(
        s.story(&sid).await.unwrap().is_none(),
        "held items are hidden"
    );
    let log = s.mod_log(10, 0).await.unwrap();
    assert_eq!(
        (log[0].actor.as_str(), log[0].action.as_str()),
        ("triage", "hold story")
    );
    assert_eq!(log[0].reason, "No spam");
    let v = s.verdict_for(Target::Story(id)).await.unwrap().unwrap();
    assert_eq!(v.model, "claude-opus-5-5");
    assert!(v.rules_version_id.is_some());

    // A moderator releases it; it is visible again.
    let holds = s.holds().await.unwrap();
    assert_eq!(holds.len(), 1);
    s.decide_verdict(
        holds[0].verdict.id,
        "released",
        By::Moderator(m.id),
        "Not spam",
        NOW,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(s.story(&sid).await.unwrap().is_some());
    assert!(s.holds().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_confirmed_hold_becomes_a_removal() {
    let (s, alice, m, fake, t) = setup().await;
    let p = Policy::default();
    rules(&s, &m).await;
    let (_, sid) = post(&s, &alice, 1, true).await;
    fake.reply("spam", "high");
    step(&s, &t, &p, NOW).await.unwrap();
    let h = s.holds().await.unwrap();
    s.decide_verdict(
        h[0].verdict.id,
        "confirmed",
        By::Moderator(m.id),
        "Spam",
        NOW,
    )
    .await
    .unwrap()
    .unwrap();
    let row = s.story_any(&sid).await.unwrap().unwrap();
    assert!(row.held_at.is_none() && row.removed_at.is_some());
    assert_eq!(
        s.decide_verdict(h[0].verdict.id, "released", By::Moderator(m.id), "x", NOW)
            .await
            .unwrap(),
        Err(threaded_store::ModError::NotFound),
        "a decided verdict takes no second decision"
    );
}

#[tokio::test]
async fn other_verdicts_suggest_and_hold_nothing() {
    let (s, alice, m, fake, t) = setup().await;
    let p = Policy::default();
    rules(&s, &m).await;
    let (_, sid) = post(&s, &alice, 1, true).await;
    fake.reply("spam", "medium");
    step(&s, &t, &p, NOW).await.unwrap();
    assert!(s.story(&sid).await.unwrap().is_some());
    let sugg = s.triage_suggestions().await.unwrap();
    assert_eq!(sugg.len(), 1);
    s.decide_verdict(
        sugg[0].verdict.id,
        "removed",
        By::Moderator(m.id),
        "Spam",
        NOW,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        s.story(&sid).await.unwrap().is_none(),
        "acting on a suggestion removes the item"
    );
    let stats = s.triage_stats(NOW).await.unwrap();
    assert_eq!(
        (stats.suggestions_decided, stats.suggestions_agreed),
        (1, 1)
    );
}

#[tokio::test]
async fn opted_out_authors_skip_the_model() {
    let (s, alice, m, fake, t) = setup().await;
    let p = Policy::default();
    rules(&s, &m).await;
    s.set_triage_opt_out(alice.id, true).await.unwrap();
    post(&s, &alice, 1, true).await;
    fake.reply("spam", "high");
    assert_eq!(
        step(&s, &t, &p, NOW).await.unwrap(),
        Step::Judged {
            verdict: "needs_human".into(),
            held: false
        }
    );
    assert!(fake.requests.lock().unwrap().is_empty(), "no model call");
    assert_eq!(
        s.triage_suggestions().await.unwrap().len(),
        1,
        "a moderator sees it"
    );
}

#[tokio::test]
async fn the_breaker_stops_holds_when_moderators_keep_releasing_them() {
    let (s, alice, m, fake, t) = setup().await;
    let p = Policy::default();
    rules(&s, &m).await;
    for n in 0..20 {
        post(&s, &alice, n, true).await;
        fake.reply("spam", "high");
        step(&s, &t, &p, NOW).await.unwrap();
        let h = s.holds().await.unwrap();
        s.decide_verdict(
            h[0].verdict.id,
            "released",
            By::Moderator(m.id),
            "Fine",
            NOW,
        )
        .await
        .unwrap()
        .unwrap();
    }
    let stats = s.triage_stats(NOW).await.unwrap();
    assert!(stats.breaker_open(p.hold_precision_floor));

    let (_, sid) = post(&s, &alice, 99, true).await;
    fake.reply("spam", "high");
    assert_eq!(
        step(&s, &t, &p, NOW).await.unwrap(),
        Step::Judged {
            verdict: "spam".into(),
            held: false
        }
    );
    assert!(
        s.story(&sid).await.unwrap().is_some(),
        "with the breaker open, spam is only suggested"
    );

    s.reset_breaker(By::Moderator(m.id), "Rules fixed", NOW + 1)
        .await
        .unwrap()
        .unwrap();
    assert!(!s
        .triage_stats(NOW + 1)
        .await
        .unwrap()
        .breaker_open(p.hold_precision_floor));
}
