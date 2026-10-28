mod common;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use threaded_store::Store;
use threaded_web::router;
use tower::ServiceExt;

async fn setup() -> (axum::Router, Store) {
    let store = Store::open("sqlite::memory:").await.unwrap();
    let app = router(common::state(
        &store,
        Default::default(),
        1_790_000_000,
        std::path::Path::new("/nonexistent/blog"),
    ));
    (app, store)
}

struct Res {
    status: StatusCode,
    body: String,
    set_cookies: Vec<String>,
    location: Option<String>,
}

async fn send(app: &axum::Router, req: Request<Body>) -> Res {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let set_cookies = res
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect();
    let location = res
        .headers()
        .get(header::LOCATION)
        .map(|v| v.to_str().unwrap().to_string());
    let body =
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    Res {
        status,
        body,
        set_cookies,
        location,
    }
}

fn get(uri: &str, cookie: &str) -> Request<Body> {
    Request::get(uri)
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap()
}

fn post(uri: &str, cookie: &str, form: &str) -> Request<Body> {
    Request::post(uri)
        .header(header::COOKIE, cookie)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(form.to_string()))
        .unwrap()
}

/// A CSRF token issued by the server, as cookie header and form value.
async fn csrf(app: &axum::Router) -> (String, String) {
    let r = send(app, get("/login", "")).await;
    let c = r
        .set_cookies
        .iter()
        .find(|c| c.starts_with("csrf="))
        .expect("csrf cookie");
    assert!(
        c.contains("SameSite=Strict") && c.contains("HttpOnly"),
        "{c}"
    );
    let token = c["csrf=".len()..].split(';').next().unwrap().to_string();
    assert!(r.body.contains(&format!(r#"name="csrf" value="{token}""#)));
    (format!("csrf={token}"), token)
}

fn session_cookie(value: &str) -> String {
    let key = cookie::Key::from(&common::COOKIE_KEY);
    let mut jar = cookie::CookieJar::new();
    jar.private_mut(&key)
        .add(cookie::Cookie::new("threaded", value.to_string()));
    format!("threaded={}", jar.get("threaded").unwrap().value())
}

#[tokio::test]
async fn login_page_and_header_when_signed_out() {
    let (app, _) = setup().await;
    let r = send(&app, get("/login", "")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("<title>Sign in | Threaded</title>"));
    assert!(r
        .body
        .contains("You sign in on the server that hosts your account, not on Threaded."));
    assert!(r.body.contains(r#"<a href="/login">Sign in</a>"#));
    assert!(r.body.contains(r#"value="https://bsky.social""#));
}

#[tokio::test]
async fn posts_without_a_matching_token_are_refused() {
    let (app, _) = setup().await;
    let (cookie, token) = csrf(&app).await;
    for (c, form) in [
        ("", format!("csrf={token}&identifier=alice.example")),
        (
            cookie.as_str(),
            "csrf=wrong&identifier=alice.example".to_string(),
        ),
        (cookie.as_str(), "identifier=alice.example".to_string()),
    ] {
        let r = send(&app, post("/login", c, &form)).await;
        assert!(
            matches!(
                r.status,
                StatusCode::FORBIDDEN | StatusCode::UNPROCESSABLE_ENTITY
            ),
            "{:?}",
            r.status
        );
    }
    let r = send(&app, post("/logout", &cookie, "csrf=wrong")).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn empty_identifier_is_explained() {
    let (app, _) = setup().await;
    let (cookie, token) = csrf(&app).await;
    let r = send(
        &app,
        post(
            "/login",
            &cookie,
            &format!("csrf={token}&identifier=+%40+&next=%2Fs%2Fabc"),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.body
            .contains("Enter your handle or your server&#39;s address.")
            || r.body
                .contains("Enter your handle or your server's address."),
        "{}",
        r.body
    );
    assert!(r
        .body
        .contains(r#"aria-describedby="identifier-hint login-error""#));
    assert!(r.body.contains(r#"name="next" value="/s/abc""#));
}

#[tokio::test]
async fn localhost_client_metadata() {
    let (app, _) = setup().await;
    let r = send(&app, get("/oauth-client-metadata.json", "")).await;
    assert_eq!(r.status, StatusCode::OK);
    let m: serde_json::Value = serde_json::from_str(&r.body).unwrap();
    assert!(m["client_id"].as_str().unwrap().starts_with(
        "http://localhost/?redirect_uri=http%3A%2F%2F127.0.0.1%3A8080%2Foauth%2Fcallback"
    ));
    assert_eq!(
        m["scope"],
        "atproto repo:app.bsky.feed.post?action=create repo:at.threaded.dev.comment repo:at.threaded.dev.submission repo:at.threaded.dev.vote?action=create&action=delete",
        "every scope any sign-in may ask for"
    );
    assert_eq!(m["token_endpoint_auth_method"], "none");
    assert_eq!(m["dpop_bound_access_tokens"], true);
    assert_eq!(
        send(&app, get("/oauth/jwks.json", "")).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn callback_failures_render_a_reason() {
    let (app, _) = setup().await;
    let r = send(
        &app,
        get(
            "/oauth/callback?error=access_denied&iss=https%3A%2F%2Fbsky.social",
            "",
        ),
    )
    .await;
    assert!(r.body.contains("You chose not to allow Threaded."));
    assert!(r.body.contains("Server: bsky.social"));

    let r = send(
        &app,
        get(
            "/oauth/callback?code=x&state=forged&iss=https%3A%2F%2Fbsky.social",
            "",
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.body.contains("bsky.social declined the sign-in."),
        "{}",
        r.body
    );
    assert!(r.set_cookies.iter().all(|c| !c.starts_with("threaded=")));

    let r = send(&app, get("/oauth/callback?state=x", "")).await;
    assert!(r.body.contains("missing its code"));
}

#[tokio::test]
async fn signed_in_header_and_sign_out() {
    let (app, store) = setup().await;
    store
        .record_sign_in("did:plc:alice", "alice.example", true, 1)
        .await
        .unwrap();
    sqlx::query("INSERT INTO oauth_sessions (did, session_id, data, updated_at) VALUES ('did:plc:alice', 'sid1', '{}', 1)")
        .execute(store.pool())
        .await
        .unwrap();
    let (csrf_cookie, token) = csrf(&app).await;
    let cookies = format!("{csrf_cookie}; {}", session_cookie("did:plc:alice sid1"));

    let r = send(&app, get("/", &cookies)).await;
    assert!(
        r.body
            .contains(r#"<a class="who" href="/u/alice.example">alice.example</a>"#),
        "{}",
        r.body
    );
    assert!(r.body.contains("Sign out"));
    let renewed = r
        .set_cookies
        .iter()
        .find(|c| c.starts_with("threaded="))
        .expect("session renewed");
    assert!(
        renewed.contains("Max-Age=2592000")
            && renewed.contains("HttpOnly")
            && renewed.contains("SameSite=Lax"),
        "{renewed}"
    );

    let r = send(&app, post("/logout", &cookies, &format!("csrf={token}"))).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(r.location.as_deref(), Some("/"));
    assert!(
        r.set_cookies
            .iter()
            .any(|c| c.starts_with("threaded=;") || c.contains("threaded=; ")),
        "{:?}",
        r.set_cookies
    );
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM oauth_sessions")
        .fetch_one(store.pool())
        .await
        .unwrap();
    assert_eq!(left, 0);

    let r = send(&app, get("/", &cookies)).await;
    assert!(
        r.body.contains(r#"<a href="/login">Sign in</a>"#),
        "a deleted session must not sign anyone in"
    );
}

#[tokio::test]
async fn forged_session_cookie_is_ignored() {
    let (app, store) = setup().await;
    store
        .record_sign_in("did:plc:alice", "alice.example", true, 1)
        .await
        .unwrap();
    let r = send(&app, get("/", "threaded=did:plc:alice%20sid1")).await;
    assert!(r.body.contains(r#"<a href="/login">Sign in</a>"#));
}

#[tokio::test]
async fn typeahead_ignores_short_and_non_handle_queries() {
    let (app, _) = setup().await;
    for q in ["a", "did:plc:x", "https%3A%2F%2Fpds.example"] {
        let r = send(&app, get(&format!("/login/typeahead?q={q}"), "")).await;
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(r.body, r#"{"actors":[]}"#, "{q}");
    }
}
