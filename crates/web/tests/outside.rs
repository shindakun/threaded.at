//! The outside services Threaded asks over HTTP, each played by a local
//! server: the PLC directory on user review, and the AppView behind the
//! sign-in typeahead.

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use common::world::*;
use serde_json::json;
use threaded_web::identity::Plc;

const GOOD: &str = "did:plc:aaaaaaaaaaaaaaaaaaaaaaaa";
const EMPTY: &str = "did:plc:bbbbbbbbbbbbbbbbbbbbbbbb";
const UNKNOWN: &str = "did:plc:cccccccccccccccccccccccc";
const BROKEN: &str = "did:plc:dddddddddddddddddddddddd";
const GARBLED: &str = "did:plc:eeeeeeeeeeeeeeeeeeeeeeee";
const SLOW: &str = "did:plc:ffffffffffffffffffffffff";

/// A PLC directory that answers each test DID its own way, and notes the
/// paths it was asked for.
async fn fake_plc(asked: Arc<Mutex<Vec<String>>>) -> threaded_web::origin::Origin {
    let app = Router::new().route(
        "/{did}/log/audit",
        get(move |Path(did): Path<String>| {
            let asked = asked.clone();
            async move {
                asked.lock().unwrap().push(format!("/{did}/log/audit"));
                match did.as_str() {
                    GOOD => Json(json!([
                        {"did": GOOD, "createdAt": "2023-11-09T21:49:10.000Z", "operation": {}},
                        {"did": GOOD, "createdAt": "2025-01-02T03:04:05.000Z", "operation": {}},
                    ]))
                    .into_response(),
                    EMPTY => Json(json!([])).into_response(),
                    BROKEN => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                    GARBLED => "not json".into_response(),
                    SLOW => {
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        Json(json!([])).into_response()
                    }
                    _ => (StatusCode::NOT_FOUND, "DID not registered").into_response(),
                }
            }
        }),
    );
    common::serve_fake(app).await
}

#[tokio::test]
async fn user_review_asks_the_plc_directory_it_is_given() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let plc = fake_plc(asked.clone()).await;
    let w = world_custom(NOW, None, move |s| s.plc = Plc::directory_at(plc)).await;
    let moderator = w.user("mod.example", &base(), 100, 0).await;
    w.role("mod.example", "moderator").await;
    for did in [GOOD, EMPTY, UNKNOWN, BROKEN, GARBLED, SLOW] {
        w.store
            .record_sign_in(did, &format!("{}.example", &did[8..12]), true, NOW)
            .await
            .unwrap();
    }
    let created = |page: &str| {
        let at = page.find("<dt>Account created</dt><dd>").unwrap();
        let rest = &page[at + "<dt>Account created</dt><dd>".len()..];
        rest[..rest.find("</dd>").unwrap()].to_string()
    };
    let page = |did: &'static str| {
        let w = &w;
        let moderator = moderator.clone();
        async move { created(&w.get(&format!("/mod/u/{did}"), &moderator).await.body) }
    };

    let good = page(GOOD).await;
    assert!(
        good.starts_with("2023-11-09 (") && good.ends_with("), from the PLC directory"),
        "the first entry is the genesis operation: {good}"
    );
    assert_eq!(
        page(EMPTY).await,
        "the PLC directory does not know this DID"
    );
    assert_eq!(
        page(UNKNOWN).await,
        "the PLC directory does not know this DID"
    );
    assert_eq!(
        page(BROKEN).await,
        "The PLC directory did not answer: status 500 Internal Server Error."
    );
    assert!(
        page(GARBLED)
            .await
            .starts_with("The PLC directory did not answer: "),
        "an unreadable answer is a failure, not an unknown DID"
    );
    let started = std::time::Instant::now();
    let slow = page(SLOW).await;
    assert!(
        slow.starts_with("The PLC directory did not answer: "),
        "{slow}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "the page waits 3 seconds, not for the directory"
    );
    assert_eq!(
        asked.lock().unwrap()[0],
        format!("/{GOOD}/log/audit"),
        "the directory's own path for an audit log"
    );
}

/// An AppView whose typeahead answers by query, counting each request and
/// noting its parameters.
struct FakeAppView {
    url: threaded_web::origin::Origin,
    asked: Arc<Mutex<Vec<HashMap<String, String>>>>,
}

async fn fake_appview() -> FakeAppView {
    let asked: Arc<Mutex<Vec<HashMap<String, String>>>> = Arc::default();
    let seen = asked.clone();
    let app = Router::new().route(
        "/xrpc/app.bsky.actor.searchActorsTypeahead",
        get(move |Query(q): Query<HashMap<String, String>>| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(q.clone());
                match q.get("q").map(String::as_str) {
                    // An XRPC error: JSON, so only its status says it failed.
                    Some("down") => (
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(json!({"error": "Unavailable", "message": "try later"})),
                    )
                        .into_response(),
                    Some("garbled") => "not json".into_response(),
                    _ => Json(json!({"actors": [
                        {
                            "did": "did:plc:alice",
                            "handle": "alice.example",
                            "displayName": "Alice",
                            "avatar": "https://cdn.bsky.app/img/avatar/alice",
                            "viewer": {"muted": false},
                            "labels": [],
                            "description": "not passed on",
                        },
                        {"did": "did:plc:alfie", "handle": "alfie.example"},
                    ]}))
                    .into_response(),
                }
            }
        }),
    );
    FakeAppView {
        url: common::serve_fake(app).await,
        asked,
    }
}

async fn typeahead_world(appview: &FakeAppView) -> World {
    let url = appview.url.clone();
    world_custom(NOW, None, move |s| {
        let auth = Arc::try_unwrap(std::mem::replace(&mut s.auth, common::auth(&s.store)))
            .ok()
            .expect("the state holds the only copy");
        s.auth = Arc::new(auth.with_typeahead(url));
    })
    .await
}

#[tokio::test]
async fn the_typeahead_passes_on_four_fields_and_caches_answers() {
    let appview = fake_appview().await;
    let w = typeahead_world(&appview).await;

    let r = w.get("/login/typeahead?q=%40Al", "").await;
    assert_eq!(r.status, StatusCode::OK);
    let body: serde_json::Value = serde_json::from_str(&r.body).unwrap();
    assert_eq!(
        body,
        json!({"actors": [
            {
                "did": "did:plc:alice",
                "handle": "alice.example",
                "displayName": "Alice",
                "avatar": "https://cdn.bsky.app/img/avatar/alice",
            },
            {"did": "did:plc:alfie", "handle": "alfie.example", "displayName": null, "avatar": null},
        ]})
    );
    {
        let asked = appview.asked.lock().unwrap();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0]["q"], "al", "trimmed of @ and lowercased");
        assert_eq!(asked[0]["limit"], "6");
    }

    let again = w.get("/login/typeahead?q=al", "").await;
    assert_eq!(again.body, r.body);
    assert_eq!(
        appview.asked.lock().unwrap().len(),
        1,
        "a repeat comes from the cache"
    );
}

#[tokio::test]
async fn a_failed_answer_is_empty_and_not_cached() {
    let appview = fake_appview().await;
    let w = typeahead_world(&appview).await;
    for q in ["down", "garbled"] {
        for _ in 0..2 {
            let r = w.get(&format!("/login/typeahead?q={q}"), "").await;
            assert_eq!(r.status, StatusCode::OK);
            assert_eq!(r.body, r#"{"actors":[]}"#, "{q}");
        }
    }
    let asked = appview.asked.lock().unwrap();
    assert_eq!(
        asked.iter().filter(|q| q["q"] == "down").count(),
        2,
        "an error from the AppView is asked again"
    );
}

#[tokio::test]
async fn the_typeahead_limits_each_address() {
    let appview = fake_appview().await;
    let w = typeahead_world(&appview).await;
    let ask = |ip: &'static str, n: usize| {
        let w = &w;
        async move {
            w.send(
                axum::http::Request::get(format!("/login/typeahead?q=al{n}"))
                    .header("x-forwarded-for", ip)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .status
        }
    };
    for n in 0..30 {
        assert_eq!(ask("203.0.113.7", n).await, StatusCode::OK, "{n}");
    }
    assert_eq!(ask("203.0.113.7", 30).await, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        ask("203.0.113.8", 31).await,
        StatusCode::OK,
        "another address has its own allowance"
    );
}
