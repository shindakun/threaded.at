mod common;

use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use threaded_auth::client::{scopes, Flow, Namespace};
use threaded_auth::records::MemoryRepo;
use threaded_store::Store;
use threaded_web::router;
use tower::ServiceExt;

const NOW: i64 = 1_790_000_000;
const DAY: i64 = 86_400;

struct World {
    app: axum::Router,
    store: Store,
    repo: Arc<MemoryRepo>,
    csrf: String,
}

struct Res {
    status: StatusCode,
    body: String,
    location: Option<String>,
}

async fn world_at(now: i64, store: Option<(Store, Arc<MemoryRepo>)>) -> World {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let (store, repo) = match store {
        Some(s) => s,
        None => {
            let store = Store::open("sqlite::memory:").await.unwrap();
            for (t, d, b) in [
                ("rust", "Rust", 0.0),
                ("show", "Show", 0.0),
                ("ask", "Ask", 0.0),
                ("meta", "Meta", -0.5),
            ] {
                store.upsert_tag(t, d, b).await.unwrap();
            }
            (store, Arc::new(MemoryRepo::default()))
        }
    };
    let app = router(common::state(
        &store,
        repo.clone(),
        now,
        Path::new("/nonexistent/blog"),
    ));
    let r = app
        .clone()
        .oneshot(Request::get("/login").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let c = r.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .to_string();
    let csrf = c["csrf=".len()..].split(';').next().unwrap().to_string();
    World {
        app,
        store,
        repo,
        csrf,
    }
}

async fn world() -> World {
    world_at(NOW, None).await
}

impl World {
    /// Signs `handle` in with `scope`; `age_days` sets the account's tenure.
    async fn user(&self, handle: &str, scope: &str, age_days: i64, karma: i64) -> String {
        let did = format!("did:plc:{}", handle.split('.').next().unwrap());
        self.store
            .record_sign_in(&did, handle, true, NOW)
            .await
            .unwrap();
        sqlx::query("UPDATE users SET created_at = ?, karma = ? WHERE did = ?")
            .bind(NOW - age_days * DAY)
            .bind(karma)
            .bind(&did)
            .execute(self.store.pool())
            .await
            .unwrap();
        let sid = format!("sid-{handle}");
        sqlx::query("INSERT OR REPLACE INTO oauth_sessions (did, session_id, data, updated_at) VALUES (?, ?, ?, 0)")
            .bind(&did)
            .bind(&sid)
            .bind(threaded_auth::testing::session_json(&did, &sid, scope))
            .execute(self.store.pool())
            .await
            .unwrap();
        let key = cookie::Key::from(&common::COOKIE_KEY);
        let mut jar = cookie::CookieJar::new();
        let flow = if scope.contains(".vote") {
            "votes"
        } else {
            "base"
        };
        jar.private_mut(&key).add(cookie::Cookie::new(
            "threaded",
            format!("{did} {sid} {flow}"),
        ));
        format!(
            "csrf={}; threaded={}",
            self.csrf,
            jar.get("threaded").unwrap().value()
        )
    }

    async fn send(&self, req: Request<Body>) -> Res {
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let location = res
            .headers()
            .get(header::LOCATION)
            .map(|v| v.to_str().unwrap().to_string());
        let body = String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec())
            .unwrap();
        Res {
            status,
            body,
            location,
        }
    }

    async fn get(&self, uri: &str, who: &str) -> Res {
        self.send(
            Request::get(uri)
                .header(header::COOKIE, who)
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    async fn post(&self, uri: &str, who: &str, form: &[(&str, &str)]) -> Res {
        let mut pairs: Vec<(&str, &str)> = vec![("csrf", &self.csrf)];
        pairs.extend_from_slice(form);
        let body = serde_urlencoded::to_string(&pairs).unwrap();
        self.send(
            Request::post(uri)
                .header(header::COOKIE, who)
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
    }

    /// Submits a story and returns its short ID.
    async fn submit(&self, who: &str, title: &str, url: &str, tags: &[&str]) -> String {
        let mut form = vec![("title", title), ("url", url)];
        for t in tags {
            form.push(("tags", t));
        }
        let r = self.post("/submit", who, &form).await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
        r.location.unwrap().trim_start_matches("/s/").to_string()
    }

    fn writes(&self) -> Vec<threaded_auth::records::Written> {
        self.repo.writes.lock().unwrap().clone()
    }
}

fn base() -> String {
    scopes(Namespace::DEV, Flow::Base)
}

fn votes() -> String {
    scopes(Namespace::DEV, Flow::Votes)
}

#[tokio::test]
async fn submitting_writes_the_record_then_indexes_it() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let id = w
        .submit(
            &alice,
            "  Writing a B-tree  ",
            "https://example.com/btree",
            &["rust"],
        )
        .await;

    let writes = w.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].op, "create");
    assert_eq!(writes[0].collection, "at.threaded.dev.submission");
    let rec = writes[0].record.as_ref().unwrap();
    assert_eq!(rec["$type"], "at.threaded.dev.submission");
    assert_eq!(rec["title"], "Writing a B-tree");
    assert_eq!(rec["url"], "https://example.com/btree");
    assert_eq!(rec["tags"], serde_json::json!(["rust"]));
    assert_eq!(rec["authored"], false);
    assert_eq!(rec["createdAt"], "2026-09-21T14:13:20Z");
    assert!(rec.get("text").is_none());

    let (uri,): (String,) = sqlx::query_as("SELECT at_uri FROM stories WHERE short_id = ?")
        .bind(&id)
        .fetch_one(w.store.pool())
        .await
        .unwrap();
    assert_eq!(uri, "at://did:plc:alice/at.threaded.dev.submission/m0");

    let page = w.get(&format!("/s/{id}"), &alice).await;
    assert!(page.body.contains("Writing a B-tree"));
    assert!(
        page.body
            .contains(r#"href="https://example.com/btree" target="_blank" rel="noopener""#),
        "a story's link opens in a new tab"
    );
    assert!(page.body.contains("(opens in a new tab)"));
    assert!(
        page.body.contains(&format!(r#"href="/s/{id}/edit""#)),
        "the author can edit"
    );
}

#[tokio::test]
async fn submit_refusals_explain_themselves() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let cases: &[(&[(&str, &str)], &str)] = &[
        (
            &[("title", "T"), ("url", "https://e.example/a")],
            "Pick at least one tag.",
        ),
        (
            &[
                ("title", "T"),
                ("url", "https://e.example/a"),
                ("tags", "nope"),
            ],
            "There is no tag",
        ),
        (
            &[
                ("title", ""),
                ("url", "https://e.example/a"),
                ("tags", "rust"),
            ],
            "A title is 1 to 150 characters.",
        ),
        (
            &[("title", "T"), ("tags", "rust")],
            "Add a link, some text, or both.",
        ),
        (
            &[
                ("title", "T"),
                ("url", "javascript:alert(1)"),
                ("tags", "rust"),
            ],
            "The link is not usable",
        ),
        (
            &[
                ("title", "T"),
                ("url", "https://e.example/a"),
                ("tags", "show"),
            ],
            "is something you made",
        ),
        (
            &[
                ("title", "T"),
                ("url", "https://e.example/a"),
                ("tags", "ask"),
            ],
            "is text only",
        ),
    ];
    for (form, msg) in cases {
        let r = w.post("/submit", &alice, form).await;
        assert_eq!(r.status, StatusCode::OK, "{msg}");
        assert!(r.body.contains(msg), "{msg}: {}", r.body);
        assert!(r.body.contains(r#"role="alert""#));
    }
    assert!(
        w.writes().is_empty(),
        "no refused submission reaches the PDS"
    );
}

#[tokio::test]
async fn duplicates_rate_limits_and_permissions() {
    let w = world().await;
    let newbie = w.user("new.example", &base(), 1, 0).await;
    let first = w
        .submit(&newbie, "One", "https://e.example/one", &["rust"])
        .await;

    let dup = w
        .post(
            "/submit",
            &newbie,
            &[
                ("title", "Again"),
                ("url", "http://www.e.example/one/?utm_source=x"),
                ("tags", "rust"),
            ],
        )
        .await;
    assert_eq!(
        dup.location.as_deref(),
        Some(format!("/s/{first}?duplicate=1").as_str())
    );

    w.submit(&newbie, "Two", "https://e.example/two", &["rust"])
        .await;
    let third = w
        .post(
            "/submit",
            &newbie,
            &[
                ("title", "Three"),
                ("url", "https://e.example/3"),
                ("tags", "rust"),
            ],
        )
        .await;
    assert!(
        third.body.contains("You can submit 2 stories a day."),
        "{}",
        third.body
    );

    let signed_out = w
        .post("/submit", &format!("csrf={}", w.csrf), &[("title", "T")])
        .await;
    assert_eq!(signed_out.location.as_deref(), Some("/login?next=/submit"));

    let m1 = w.user("old.example", "atproto", 100, 0).await;
    let r = w
        .post(
            "/submit",
            &m1,
            &[
                ("title", "T"),
                ("url", "https://e.example/m1"),
                ("tags", "rust"),
            ],
        )
        .await;
    assert_eq!(
        r.location.as_deref(),
        Some("/login?next=/submit"),
        "a session without write scopes signs in again"
    );
}

#[tokio::test]
async fn comments_and_replies_reference_their_records() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let bob = w.user("bob.example", &base(), 100, 0).await;
    let id = w
        .submit(&alice, "Story", "https://e.example/s", &["rust"])
        .await;

    let r = w
        .post(
            "/comment",
            &bob,
            &[
                ("story", &id),
                ("text", "First *point*.\n\n![x](https://i.example/p.png)"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let first = r
        .location
        .unwrap()
        .rsplit("#c-")
        .next()
        .unwrap()
        .to_string();
    let r = w
        .post(
            "/comment",
            &alice,
            &[("story", &id), ("parent", &first), ("text", "A reply.")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);

    let writes = w.writes();
    let story_uri = "at://did:plc:alice/at.threaded.dev.submission/m0";
    let c1 = writes[1].record.as_ref().unwrap();
    assert_eq!(c1["$type"], "at.threaded.dev.comment");
    assert_eq!(c1["subject"]["uri"], story_uri);
    assert_eq!(c1["subject"]["cid"], "bafymem0");
    assert!(c1.get("parent").is_none());
    let c2 = writes[2].record.as_ref().unwrap();
    assert_eq!(
        c2["parent"]["uri"],
        "at://did:plc:bob/at.threaded.dev.comment/m1"
    );

    let page = w.get(&format!("/s/{id}"), &bob).await;
    assert_eq!(
        page.body.matches(r#"<ol class="thread">"#).count(),
        2,
        "a reply nests one level down"
    );
    assert!(page.body.contains("<em>point</em>"));
    assert!(
        !page.body.contains("<img"),
        "user Markdown renders no images"
    );
    assert!(page.body.contains(r#"aria-label="Comment by bob.example"#));
    assert!(page.body.contains("2 comments"));
}

#[tokio::test]
async fn votes_move_score_rank_and_karma() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let bob = w.user("bob.example", &base(), 100, 0).await;
    let id = w
        .submit(&alice, "Story", "https://e.example/v", &["rust"])
        .await;

    let own = w
        .post("/vote", &alice, &[("kind", "story"), ("id", &id)])
        .await;
    assert!(own.body.contains("You cannot vote on your own post."));

    let r = w
        .send(
            Request::post("/vote")
                .header(header::COOKIE, &bob)
                .header(header::ACCEPT, "application/json")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!("csrf={}&kind=story&id={id}", w.csrf)))
                .unwrap(),
        )
        .await;
    assert_eq!(r.body, r#"{"score":2,"voted":true}"#);
    let karma: i64 = sqlx::query_scalar("SELECT karma FROM users WHERE did = 'did:plc:alice'")
        .fetch_one(w.store.pool())
        .await
        .unwrap();
    assert_eq!(karma, 1);

    let page = w.get("/", &bob).await;
    assert!(page.body.contains(r#"aria-pressed="true""#));

    let r = w
        .post(
            "/vote",
            &bob,
            &[("kind", "story"), ("id", &id), ("next", "/")],
        )
        .await;
    assert_eq!(r.location.as_deref(), Some("/"));
    let score: i64 = sqlx::query_scalar("SELECT score FROM stories WHERE short_id = ?")
        .bind(&id)
        .fetch_one(w.store.pool())
        .await
        .unwrap();
    assert_eq!(score, 1, "a second vote removes the upvote");
    assert_eq!(w.writes().len(), 1, "private votes write nothing");
}

#[tokio::test]
async fn flags_need_standing_and_a_reason() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let member = w.user("bob.example", &base(), 100, 0).await;
    let newbie = w.user("new.example", &base(), 3, 0).await;
    let id = w
        .submit(&alice, "Story", "https://e.example/f", &["rust"])
        .await;

    let r = w
        .post(
            "/flag",
            &newbie,
            &[("kind", "story"), ("id", &id), ("reason", "spam")],
        )
        .await;
    assert!(
        r.body.contains("You cannot flag this."),
        "new users cannot flag"
    );
    let r = w
        .post(
            "/flag",
            &member,
            &[("kind", "story"), ("id", &id), ("reason", "troll")],
        )
        .await;
    assert!(
        r.body.contains("Pick one of the listed reasons."),
        "troll is a comment reason"
    );

    let r = w
        .post(
            "/flag",
            &member,
            &[("kind", "story"), ("id", &id), ("reason", "spam")],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let (score, flags): (i64, i64) =
        sqlx::query_as("SELECT score, flags FROM stories WHERE short_id = ?")
            .bind(&id)
            .fetch_one(w.store.pool())
            .await
            .unwrap();
    assert_eq!((score, flags), (0, 1));

    let author_view = w.get(&format!("/s/{id}"), &alice).await;
    assert!(author_view.body.contains("Flagged: Spam (1)"));
    let flagger_view = w.get(&format!("/s/{id}"), &member).await;
    assert!(flagger_view.body.contains("Unflag (Spam)"));
    assert!(
        !w.get(&format!("/s/{id}"), &newbie)
            .await
            .body
            .contains("Flagged:"),
        "only the author sees flag reasons"
    );

    w.post(
        "/flag",
        &member,
        &[("kind", "story"), ("id", &id), ("reason", "")],
    )
    .await;
    let flags: i64 = sqlx::query_scalar("SELECT flags FROM stories WHERE short_id = ?")
        .bind(&id)
        .fetch_one(w.store.pool())
        .await
        .unwrap();
    assert_eq!(flags, 0);
}

#[tokio::test]
async fn edits_keep_history_and_deletes_reach_the_pds() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let bob = w.user("bob.example", &base(), 100, 0).await;
    let id = w
        .submit(&alice, "Old title", "https://e.example/e", &["rust"])
        .await;

    let r = w
        .post(
            &format!("/s/{id}/edit"),
            &bob,
            &[("title", "Hijack"), ("tags", "rust")],
        )
        .await;
    assert!(r.body.contains("Only the author can change this story."));

    let r = w
        .post(
            &format!("/s/{id}/edit"),
            &alice,
            &[
                ("title", "New title"),
                ("url", "https://e.example/e"),
                ("text", "More."),
                ("tags", "rust"),
                ("tags", "meta"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    let put = w.writes().last().cloned().unwrap();
    assert_eq!((put.op, put.rkey.as_str()), ("put", "m0"));
    assert_eq!(put.record.unwrap()["title"], "New title");
    let revisions: i64 = sqlx::query_scalar("SELECT count(*) FROM story_revisions")
        .fetch_one(w.store.pool())
        .await
        .unwrap();
    assert_eq!(revisions, 1);
    assert!(w
        .get(&format!("/s/{id}"), &bob)
        .await
        .body
        .contains("edited"));

    // Two hours later the link no longer changes.
    let later = world_at(NOW + 7_200, Some((w.store.clone(), w.repo.clone()))).await;
    let alice_later = later.user("alice.example", &base(), 100, 0).await;
    later
        .post(
            &format!("/s/{id}/edit"),
            &alice_later,
            &[
                ("title", "New title"),
                ("url", "https://evil.example/"),
                ("tags", "rust"),
            ],
        )
        .await;
    let url: String = sqlx::query_scalar("SELECT url FROM stories WHERE short_id = ?")
        .bind(&id)
        .fetch_one(w.store.pool())
        .await
        .unwrap();
    assert_eq!(url, "https://e.example/e");

    let r = w
        .post("/comment", &bob, &[("story", &id), ("text", "Parent")])
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
        &alice,
        &[("story", &id), ("parent", &parent), ("text", "Child")],
    )
    .await;
    let r = w.post(&format!("/c/{parent}/delete"), &bob, &[]).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(w.writes().last().unwrap().op, "delete");
    let page = w.get(&format!("/s/{id}"), &bob).await;
    assert!(
        page.body.contains("[deleted]") && page.body.contains("Child"),
        "a deleted comment with a reply stays as [deleted]"
    );

    let r = w.post(&format!("/s/{id}/delete"), &alice, &[]).await;
    assert_eq!(r.location.as_deref(), Some("/"));
    assert_eq!(
        w.writes().last().unwrap().collection,
        "at.threaded.dev.submission"
    );
    assert_eq!(
        w.get(&format!("/s/{id}"), &bob).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn public_upvotes_publish_and_withdraw_records() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let bob_base = w.user("bob.example", &base(), 100, 0).await;
    let one = w
        .submit(&alice, "One", "https://e.example/p1", &["rust"])
        .await;
    let two = w
        .submit(&alice, "Two", "https://e.example/p2", &["rust"])
        .await;
    w.post("/vote", &bob_base, &[("kind", "story"), ("id", &one)])
        .await;

    let s = w.get("/settings", &bob_base).await;
    assert!(s.body.contains("Grant permission") && s.body.contains(r#"name="flow" value="votes""#));
    let r = w
        .post("/settings", &bob_base, &[("public_votes", "1")])
        .await;
    assert!(r
        .body
        .contains("Grant Threaded permission to publish upvotes first."));

    let bob = w.user("bob.example", &votes(), 100, 0).await;
    let r = w
        .post("/settings", &bob, &[("public_votes", "1"), ("past", "1")])
        .await;
    assert_eq!(r.location.as_deref(), Some("/settings?notice=saved"));
    let vote_writes: Vec<_> = w
        .writes()
        .into_iter()
        .filter(|x| x.collection == "at.threaded.dev.vote")
        .collect();
    assert_eq!(vote_writes.len(), 1, "the past upvote is published");
    let rec = vote_writes[0].record.as_ref().unwrap();
    assert_eq!(
        rec["subject"]["uri"],
        "at://did:plc:alice/at.threaded.dev.submission/m0"
    );

    w.post("/vote", &bob, &[("kind", "story"), ("id", &two)])
        .await;
    let profile = w.get("/u/bob.example", &alice).await;
    assert!(
        profile.body.contains("Liked")
            && profile.body.contains(">One<")
            && profile.body.contains(">Two<")
    );

    let r = w.post("/settings", &bob, &[]).await;
    assert_eq!(r.location.as_deref(), Some("/settings?notice=saved"));
    let deletes = w
        .writes()
        .into_iter()
        .filter(|x| x.op == "delete" && x.collection == "at.threaded.dev.vote")
        .count();
    assert_eq!(deletes, 2, "turning it off deletes both records");
    assert!(!w.get("/u/bob.example", &alice).await.body.contains("Liked"));
}

#[tokio::test]
async fn a_refusing_pds_indexes_nothing() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    *w.repo.fail.lock().unwrap() = Some("repo is read-only".into());
    let r = w
        .post(
            "/submit",
            &alice,
            &[
                ("title", "T"),
                ("url", "https://e.example/x"),
                ("tags", "rust"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert!(
        r.body
            .contains("Your server refused the change: repo is read-only"),
        "{}",
        r.body
    );
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM stories")
        .fetch_one(w.store.pool())
        .await
        .unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn profiles_list_a_users_posts() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    let id = w
        .submit(&alice, "Alice's story", "https://e.example/pr", &["rust"])
        .await;
    w.post(
        "/comment",
        &alice,
        &[("story", &id), ("text", "Note to self")],
    )
    .await;
    let text = w
        .post(
            "/submit",
            &alice,
            &[("title", "Just text"), ("text", "Body."), ("tags", "rust")],
        )
        .await;
    let text_id = text.location.unwrap().trim_start_matches("/s/").to_string();
    let page = w.get(&format!("/s/{text_id}"), "").await;
    assert!(
        page.body
            .contains(&format!(r#"<a class="title" href="/s/{text_id}">"#)),
        "a text post's title stays in this tab"
    );
    let p = w.get("/u/alice.example", "").await;
    assert_eq!(p.status, StatusCode::OK);
    assert!(p.body.contains("<h1>alice.example</h1>"));
    assert!(
        p.body.contains("Alice&#39;s story") || p.body.contains("Alice's story"),
        "{}",
        p.body
    );
    assert!(p.body.contains("Note to self"));
    assert!(!p.body.contains("Liked"), "upvotes are private by default");
    assert_eq!(
        w.get("/u/nobody.example", "").await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn every_write_needs_the_csrf_token() {
    let w = world().await;
    let alice = w.user("alice.example", &votes(), 100, 0).await;
    let id = w
        .submit(&alice, "Story", "https://e.example/c", &["rust"])
        .await;
    let r = w
        .post("/comment", &alice, &[("story", &id), ("text", "Hi")])
        .await;
    let c = r
        .location
        .unwrap()
        .rsplit("#c-")
        .next()
        .unwrap()
        .to_string();
    let before = w.writes().len();
    for uri in [
        "/submit".to_string(),
        "/comment".into(),
        "/vote".into(),
        "/flag".into(),
        "/settings".into(),
        format!("/s/{id}/edit"),
        format!("/s/{id}/delete"),
        format!("/c/{c}/edit"),
        format!("/c/{c}/delete"),
    ] {
        let r = w
            .send(
                Request::post(uri.as_str())
                    .header(header::COOKIE, &alice)
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(format!(
                        "csrf=wrong&story={id}&kind=story&id={id}&title=T&text=T&tags=rust"
                    )))
                    .unwrap(),
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{uri}");
    }
    assert_eq!(w.writes().len(), before, "a refused form writes nothing");
}

#[tokio::test]
async fn a_failed_index_removes_the_new_record() {
    let w = world().await;
    let alice = w.user("alice.example", &base(), 100, 0).await;
    sqlx::query("DROP TABLE taggings")
        .execute(w.store.pool())
        .await
        .unwrap();
    let r = w
        .post(
            "/submit",
            &alice,
            &[
                ("title", "T"),
                ("url", "https://e.example/x"),
                ("tags", "rust"),
            ],
        )
        .await;
    assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR);
    let ops: Vec<_> = w.writes().into_iter().map(|x| (x.op, x.rkey)).collect();
    assert_eq!(
        ops,
        vec![("create", "m0".to_string()), ("delete", "m0".to_string())]
    );
}
