use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use askama::Template;
use axum::extract::{ConnectInfo, FromRequestParts, Query, Request, State};
use axum::http::{header, request::Parts, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::{Form, Json};
use axum_extra::extract::cookie::{Cookie, CookieJar, Key, PrivateCookieJar, SameSite};
use rand::RngCore;
use serde::Deserialize;
use std::sync::Arc;

use threaded_auth::client::{Clients, Flow};
use threaded_auth::flow::{self, FlowError};
use threaded_auth::hosts::{host_of, HostView};
use threaded_auth::records::SessionRef;
use threaded_auth::store::SqliteAuthStore;

use crate::{AppError, AppState, Page, SITE_NAME};

const SESSION_DAYS: i64 = 30;
const TYPEAHEAD_TTL: Duration = Duration::from_secs(30);
const TYPEAHEAD_LIMIT: u32 = 30;
const TYPEAHEAD_WINDOW: Duration = Duration::from_secs(10);
const TYPEAHEAD_HOST: &str = "https://public.api.bsky.app";

pub struct Auth {
    pub clients: Arc<Clients>,
    pub store: SqliteAuthStore,
    pub key: Key,
    /// Production: `Secure` cookies with the `__Host-` prefix.
    pub secure: bool,
    http: reqwest::Client,
    typeahead: Mutex<Typeahead>,
}

#[derive(Default)]
struct Typeahead {
    cache: HashMap<String, (Instant, serde_json::Value)>,
    hits: HashMap<IpAddr, (Instant, u32)>,
}

impl Auth {
    pub fn new(
        clients: Arc<Clients>,
        store: SqliteAuthStore,
        cookie_key: &[u8],
        secure: bool,
    ) -> Auth {
        Auth {
            clients,
            store,
            key: Key::from(cookie_key),
            secure,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("HTTP client builds"),
            typeahead: Mutex::new(Typeahead::default()),
        }
    }

    fn name(&self, base: &str) -> String {
        if self.secure {
            format!("__Host-{base}")
        } else {
            base.to_string()
        }
    }

    fn cookie(&self, base: &str, value: String, same_site: SameSite) -> Cookie<'static> {
        Cookie::build((self.name(base), value))
            .path("/")
            .http_only(true)
            .secure(self.secure)
            .same_site(same_site)
            .build()
    }

    fn session_cookie(&self, value: String) -> Cookie<'static> {
        let mut c = self.cookie("threaded", value, SameSite::Lax);
        c.set_max_age(time::Duration::days(SESSION_DAYS));
        c
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn random_token() -> String {
    let mut b = [0u8; 32];
    rand::rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[derive(Clone)]
struct CsrfToken(String);

/// Issues the CSRF cookie when it is missing and renews the session cookie's
/// lifetime on every response that does not set it.
pub async fn cookie_layer(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    let jar = CookieJar::from_headers(req.headers());
    let csrf_name = st.auth.name("csrf");
    let session_name = st.auth.name("threaded");
    let existing = jar.get(&csrf_name).map(|c| c.value().to_string());
    let session_raw = jar.get(&session_name).map(|c| c.value().to_string());
    let token = existing.clone().unwrap_or_else(random_token);
    req.extensions_mut().insert(CsrfToken(token.clone()));

    let mut res = next.run(req).await;
    let sets_session = res.headers().get_all(header::SET_COOKIE).iter().any(|v| {
        v.to_str()
            .is_ok_and(|s| s.starts_with(&format!("{session_name}=")))
    });
    let mut add = Vec::new();
    if existing.is_none() {
        add.push(st.auth.cookie("csrf", token, SameSite::Strict));
    }
    if let (Some(raw), false) = (session_raw, sets_session) {
        add.push(st.auth.session_cookie(raw));
    }
    for c in add {
        if let Ok(v) = HeaderValue::from_str(&c.to_string()) {
            res.headers_mut().append(header::SET_COOKIE, v);
        }
    }
    res
}

/// The signed-in user, if any, and the CSRF token for forms on the page.
#[derive(Clone, Default)]
pub struct Viewer {
    pub name: Option<String>,
    pub csrf: String,
    pub me: Option<Me>,
    /// This page's absolute, shareable URL, for `og:url`.
    pub url: String,
}

#[derive(Clone)]
pub struct Me {
    pub user_id: i64,
    pub session: SessionRef,
    /// `user`, `moderator`, or `admin`, as read for this request. Pages that
    /// need a role read it again from the database before acting.
    pub role: String,
}

/// The session cookie holds `DID SESSION_ID FLOW`.
fn session_of(jar: &PrivateCookieJar, auth: &Auth) -> Option<SessionRef> {
    let c = jar.get(&auth.name("threaded"))?;
    let mut parts = c.value().split(' ');
    let did = parts.next()?.to_string();
    let session_id = parts.next()?.to_string();
    let flow = Flow::parse(parts.next().unwrap_or("base"));
    Some(SessionRef {
        did,
        session_id,
        flow,
    })
}

impl FromRequestParts<AppState> for Viewer {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Viewer, Infallible> {
        let csrf = parts
            .extensions
            .get::<CsrfToken>()
            .map(|t| t.0.clone())
            .unwrap_or_default();
        let jar = PrivateCookieJar::from_headers(&parts.headers, st.auth.key.clone());
        let mut viewer = Viewer {
            name: None,
            csrf,
            me: None,
            url: st.origin.url(&crate::origin::shareable(
                parts.uri.path(),
                parts.uri.query(),
            )),
        };
        if let Some(session) = session_of(&jar, &st.auth) {
            let live = st
                .auth
                .store
                .session_exists(&session.did, &session.session_id)
                .await
                .unwrap_or(false);
            if live {
                if let Ok(Some(user)) = st.store.user_by_did(&session.did).await {
                    crate::handles::refresh(st, &[user.id]).await;
                    viewer.name = Some(user.name().to_string());
                    viewer.me = Some(Me {
                        user_id: user.id,
                        session,
                        role: user.role.clone(),
                    });
                }
            }
        }
        Ok(viewer)
    }
}

impl Viewer {
    pub fn is_moderator(&self) -> bool {
        self.me
            .as_ref()
            .is_some_and(|m| m.role == "moderator" || m.role == "admin")
    }

    pub fn is_admin(&self) -> bool {
        self.me.as_ref().is_some_and(|m| m.role == "admin")
    }
}

pub(crate) fn check_csrf(viewer: &Viewer, form_token: &str) -> Result<(), AppError> {
    if viewer.csrf.is_empty() || viewer.csrf != form_token {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

/// A local path to return to after sign-in; anything else becomes `/`.
pub fn safe_next(next: &str) -> String {
    let ok = next.starts_with('/')
        && !next.starts_with("//")
        && !next.contains('\\')
        && !next.starts_with("/login")
        && !next.starts_with("/oauth");
    if ok {
        next.to_string()
    } else {
        "/".to_string()
    }
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage {
    site: &'static str,
    nav: &'static str,
    viewer: Viewer,
    identifier: String,
    next: String,
    error: Option<String>,
}

#[derive(Template)]
#[template(path = "login_confirm.html")]
struct ConfirmPage {
    site: &'static str,
    nav: &'static str,
    viewer: Viewer,
    identifier: String,
    account: Option<String>,
    host: HostView,
    pds: Option<HostView>,
    next: String,
    flow: &'static str,
    changed: bool,
}

#[derive(Template)]
#[template(path = "login_error.html")]
struct LoginErrorPage {
    site: &'static str,
    nav: &'static str,
    viewer: Viewer,
    host: Option<String>,
    reason: String,
}

fn login_page(viewer: Viewer, identifier: String, next: String, error: Option<String>) -> Page {
    Ok(Html(
        LoginPage {
            site: SITE_NAME,
            nav: "",
            viewer,
            identifier,
            next,
            error,
        }
        .render()?,
    ))
}

#[derive(Deserialize)]
pub struct NextParam {
    next: Option<String>,
}

pub async fn login(viewer: Viewer, Query(p): Query<NextParam>) -> Page {
    login_page(
        viewer,
        String::new(),
        safe_next(p.next.as_deref().unwrap_or("/")),
        None,
    )
}

#[derive(Deserialize)]
pub struct LoginForm {
    csrf: String,
    identifier: String,
    #[serde(default)]
    next: String,
    #[serde(default)]
    flow: String,
}

/// The account as the sign-in confirmation page names it. A DID, as the
/// settings page sends, shows as the verified handle Threaded holds for it;
/// sign-in itself still goes by the DID. Anything else shows as entered.
pub async fn display_account(
    store: &threaded_store::Store,
    account: Option<String>,
) -> Result<Option<String>, threaded_store::Error> {
    let Some(a) = account else { return Ok(None) };
    if !a.starts_with("did:") {
        return Ok(Some(a));
    }
    Ok(Some(match store.user_by_did(&a).await? {
        Some(u) if u.handle_verified => u.handle,
        _ => a,
    }))
}

async fn confirm(
    st: &AppState,
    viewer: Viewer,
    identifier: String,
    next: String,
    flow: Flow,
    changed: bool,
) -> Page {
    match flow::resolve(&st.auth.clients, &identifier).await {
        Ok(r) => Ok(Html(
            ConfirmPage {
                site: SITE_NAME,
                nav: "",
                account: display_account(&st.store, r.account).await?,
                viewer,
                identifier,
                host: r.auth_host,
                pds: r.pds_host,
                next,
                flow: flow.as_str(),
                changed,
            }
            .render()?,
        )),
        Err(e) => {
            tracing::info!(error = %e, "sign-in: resolve failed");
            let msg = format!("No account or server found for \u{201c}{identifier}\u{201d}. Check the spelling, or enter your server's address.");
            login_page(viewer, identifier, next, Some(msg))
        }
    }
}

pub async fn login_submit(
    State(st): State<AppState>,
    viewer: Viewer,
    Form(f): Form<LoginForm>,
) -> Page {
    check_csrf(&viewer, &f.csrf)?;
    let next = safe_next(&f.next);
    let identifier = flow::normalize(&f.identifier);
    if identifier.is_empty() {
        return login_page(
            viewer,
            identifier,
            next,
            Some("Enter your handle or your server's address.".into()),
        );
    }
    confirm(&st, viewer, identifier, next, Flow::parse(&f.flow), false).await
}

#[derive(Deserialize)]
pub struct GoForm {
    csrf: String,
    identifier: String,
    host: String,
    #[serde(default)]
    next: String,
    #[serde(default)]
    flow: String,
}

pub async fn login_go(
    State(st): State<AppState>,
    viewer: Viewer,
    jar: PrivateCookieJar,
    Form(f): Form<GoForm>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    let next = safe_next(&f.next);
    let identifier = flow::normalize(&f.identifier);
    let flow = Flow::parse(&f.flow);
    let url = match flow::start(&st.auth.clients, flow, &identifier).await {
        Ok(u) => u,
        Err(e) => {
            tracing::warn!(error = %e, "sign-in: start failed");
            return Ok(error_page(
                viewer,
                Some(f.host),
                "The server could not start the sign-in. Try again in a moment.".into(),
            )?
            .into_response());
        }
    };
    let host = host_of(&url).map(|h| h.ascii).unwrap_or_default();
    if host != f.host {
        return Ok(confirm(&st, viewer, identifier, next, flow, true)
            .await?
            .into_response());
    }
    let jar = jar
        .add(st.auth.cookie("return", next, SameSite::Lax))
        .add(
            st.auth
                .cookie("flow", flow.as_str().to_string(), SameSite::Lax),
        );
    Ok((jar, Redirect::to(&url)).into_response())
}

fn error_page(viewer: Viewer, host: Option<String>, reason: String) -> Page {
    Ok(Html(
        LoginErrorPage {
            site: SITE_NAME,
            nav: "",
            viewer,
            host,
            reason,
        }
        .render()?,
    ))
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    iss: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

pub async fn callback(
    State(st): State<AppState>,
    viewer: Viewer,
    jar: PrivateCookieJar,
    Query(q): Query<CallbackQuery>,
) -> Result<Response, AppError> {
    let host = q.iss.as_deref().and_then(host_of).map(|h| h.ascii);
    let named = host.clone().unwrap_or_else(|| "Your server".into());
    if let Some(err) = q.error.as_deref() {
        let reason = if err == "access_denied" {
            "You chose not to allow Threaded.".to_string()
        } else {
            let detail = q.error_description.as_deref().unwrap_or(err);
            format!("{named} declined the sign-in: {detail}")
        };
        return Ok(error_page(viewer, host, reason)?.into_response());
    }
    let Some(code) = q.code.as_deref() else {
        return Ok(error_page(
            viewer,
            host,
            "The sign-in reply was missing its code.".into(),
        )?
        .into_response());
    };
    let flow = Flow::parse(
        jar.get(&st.auth.name("flow"))
            .map(|c| c.value().to_string())
            .as_deref()
            .unwrap_or("base"),
    );
    match flow::finish(
        &st.auth.clients,
        flow,
        code,
        q.state.as_deref(),
        q.iss.as_deref(),
    )
    .await
    {
        Ok(si) => {
            let (shown, verified) = match &si.verified_handle {
                Some(h) => (h.clone(), true),
                None => (si.did.clone(), false),
            };
            st.store
                .record_sign_in(&si.did, &shown, verified, now())
                .await?;
            // Track the account in hydrant, so its public votes arrive even
            // if it never posts. In the background: sign-in never waits on it.
            if let Some(h) = st.hydrant.clone() {
                let did = si.did.clone();
                tokio::spawn(async move {
                    if let Err(e) = h.track(&[&did]).await {
                        tracing::warn!(error = %e, did, "tracking a signed-in account in hydrant");
                    }
                });
            }
            let next = jar
                .get(&st.auth.name("return"))
                .map(|c| safe_next(c.value()))
                .unwrap_or_else(|| "/".into());
            let jar = jar
                .remove(Cookie::build(st.auth.name("return")).path("/"))
                .remove(Cookie::build(st.auth.name("flow")).path("/"))
                .add(st.auth.session_cookie(format!(
                    "{} {} {}",
                    si.did,
                    si.session_id,
                    flow.as_str()
                )));
            Ok((jar, Redirect::to(&next)).into_response())
        }
        Err(e) => {
            tracing::warn!(error = %e, "sign-in: callback failed");
            let reason = match e {
                FlowError::NoAtprotoScope => {
                    format!("{named} did not grant the identity permission Threaded needs.")
                }
                _ => format!("{named} declined the sign-in. Try again."),
            };
            Ok(error_page(viewer, host, reason)?.into_response())
        }
    }
}

#[derive(Deserialize)]
pub struct CsrfForm {
    csrf: String,
}

pub async fn logout(
    State(st): State<AppState>,
    viewer: Viewer,
    jar: PrivateCookieJar,
    Form(f): Form<CsrfForm>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    if let Some(s) = session_of(&jar, &st.auth) {
        flow::sign_out(&st.auth.clients, s.flow, &s.did, &s.session_id).await;
    }
    let jar = jar.remove(Cookie::build(st.auth.name("threaded")).path("/"));
    Ok((jar, Redirect::to("/")).into_response())
}

pub async fn client_metadata(State(st): State<AppState>) -> Response {
    (
        [(header::CACHE_CONTROL, "public, max-age=300")],
        Json(st.auth.clients.metadata_json()),
    )
        .into_response()
}

pub async fn jwks(State(st): State<AppState>) -> Response {
    match st.auth.clients.jwks_json() {
        Some(j) => ([(header::CACHE_CONTROL, "public, max-age=300")], Json(j)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// The client address. Behind nginx on loopback, the last `X-Forwarded-For`
/// entry is the one nginx added.
pub struct ClientIp(pub IpAddr);

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<ClientIp, Infallible> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip())
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
        if peer.is_loopback() {
            let forwarded = parts
                .headers
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit(',').next())
                .and_then(|v| v.trim().parse().ok());
            if let Some(ip) = forwarded {
                return Ok(ClientIp(ip));
            }
        }
        Ok(ClientIp(peer))
    }
}

#[derive(Deserialize)]
pub struct TypeaheadQuery {
    q: Option<String>,
}

pub fn typeahead_wanted(q: &str) -> bool {
    q.chars().count() >= 2 && !q.starts_with("did:") && !q.starts_with("http") && !q.contains('/')
}

pub async fn typeahead(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    Query(p): Query<TypeaheadQuery>,
) -> Response {
    let q = flow::normalize(p.q.as_deref().unwrap_or(""));
    let empty = || Json(serde_json::json!({ "actors": [] })).into_response();
    if !typeahead_wanted(&q) {
        return empty();
    }
    {
        let mut t = st.auth.typeahead.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        let hit = t.hits.entry(ip).or_insert((now, 0));
        if now.duration_since(hit.0) > TYPEAHEAD_WINDOW {
            *hit = (now, 0);
        }
        hit.1 += 1;
        if hit.1 > TYPEAHEAD_LIMIT {
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        }
        if t.hits.len() > 10_000 {
            t.hits
                .retain(|_, (start, _)| now.duration_since(*start) <= TYPEAHEAD_WINDOW);
        }
        if let Some((at, v)) = t.cache.get(&q) {
            if at.elapsed() < TYPEAHEAD_TTL {
                return Json(v.clone()).into_response();
            }
        }
    }

    let url = format!("{TYPEAHEAD_HOST}/xrpc/app.bsky.actor.searchActorsTypeahead");
    let resp = st
        .auth
        .http
        .get(&url)
        .query(&[("q", q.as_str()), ("limit", "6")])
        .send()
        .await;
    let Ok(resp) = resp else { return empty() };
    let Ok(body) = resp.json::<serde_json::Value>().await else {
        return empty();
    };
    let actors: Vec<serde_json::Value> = body["actors"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| {
                    serde_json::json!({
                        "did": x["did"],
                        "handle": x["handle"],
                        "displayName": x["displayName"],
                        "avatar": x["avatar"],
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let out = serde_json::json!({ "actors": actors });
    let mut t = st.auth.typeahead.lock().unwrap_or_else(|e| e.into_inner());
    if t.cache.len() > 1_000 {
        t.cache.clear();
    }
    t.cache.insert(q, (Instant::now(), out.clone()));
    Json(out).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_is_local_only() {
        assert_eq!(safe_next("/s/abc?x=1"), "/s/abc?x=1");
        for bad in [
            "https://evil.example",
            "//evil.example",
            "/\\evil",
            "",
            "evil",
            "/login",
            "/oauth/callback",
        ] {
            assert_eq!(safe_next(bad), "/", "{bad}");
        }
    }

    #[test]
    fn typeahead_only_for_handle_prefixes() {
        assert!(typeahead_wanted("al"));
        assert!(!typeahead_wanted("a"));
        assert!(!typeahead_wanted("did:plc:x"));
        assert!(!typeahead_wanted("https://pds.example"));
        assert!(!typeahead_wanted("a/b"));
    }
}
