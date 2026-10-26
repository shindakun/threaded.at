//! A signed-in test harness: an app on an in-memory database, the in-memory
//! record writer, and users with sessions.
#![allow(dead_code)]

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

pub const NOW: i64 = 1_790_000_000;
pub const DAY: i64 = 86_400;

pub struct World {
    pub app: axum::Router,
    pub store: Store,
    pub repo: Arc<MemoryRepo>,
    pub csrf: String,
}

pub struct Res {
    pub status: StatusCode,
    pub body: String,
    pub location: Option<String>,
}

pub async fn world_at(now: i64, store: Option<(Store, Arc<MemoryRepo>)>) -> World {
    world_with(now, store, false).await
}

/// A world with an Anthropic key configured, so triage turns on once rules exist.
pub async fn world_with_triage() -> World {
    world_with(NOW, None, true).await
}

pub async fn world_with(
    now: i64,
    store: Option<(Store, Arc<MemoryRepo>)>,
    triage_key: bool,
) -> World {
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
    let mut state = super::state(&store, repo.clone(), now, Path::new("/nonexistent/blog"));
    state.triage_key = triage_key;
    let app = router(state);
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

pub async fn world() -> World {
    world_at(NOW, None).await
}

impl World {
    /// Signs `handle` in with `scope`; `age_days` sets the account's tenure.
    pub async fn user(&self, handle: &str, scope: &str, age_days: i64, karma: i64) -> String {
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
        let key = cookie::Key::from(&super::COOKIE_KEY);
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

    pub async fn send(&self, req: Request<Body>) -> Res {
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

    pub async fn get(&self, uri: &str, who: &str) -> Res {
        self.send(
            Request::get(uri)
                .header(header::COOKIE, who)
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    pub async fn post(&self, uri: &str, who: &str, form: &[(&str, &str)]) -> Res {
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
    pub async fn submit(&self, who: &str, title: &str, url: &str, tags: &[&str]) -> String {
        let mut form = vec![("title", title), ("url", url)];
        for t in tags {
            form.push(("tags", t));
        }
        let r = self.post("/submit", who, &form).await;
        assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
        r.location.unwrap().trim_start_matches("/s/").to_string()
    }

    pub fn writes(&self) -> Vec<threaded_auth::records::Written> {
        self.repo.writes.lock().unwrap().clone()
    }

    /// Sets a signed-in user's role directly, as `threaded role` would.
    pub async fn role(&self, handle: &str, role: &str) {
        sqlx::query("UPDATE users SET role = ? WHERE handle = ?")
            .bind(role)
            .bind(handle)
            .execute(self.store.pool())
            .await
            .unwrap();
    }
}

pub fn base() -> String {
    scopes(Namespace::DEV, Flow::Base)
}

pub fn votes() -> String {
    scopes(Namespace::DEV, Flow::Votes)
}
