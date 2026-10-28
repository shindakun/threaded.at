use std::collections::HashMap;
pub mod admin;
pub mod auth;
pub mod blog;
pub mod handles;
pub mod identity;
pub mod inbox;
pub mod markdown;
pub mod origin;
pub mod staff;
pub mod write;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use askama::Template;
use axum::extract::{FromRef, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde::Deserialize;
use threaded_core::search;
use threaded_store::{Filter, Listing, Store, StoryRow};

use crate::auth::{Auth, Viewer};
use crate::blog::{Blog, Post};

pub const SITE_NAME: &str = "Threaded";
const PAGE_SIZE: i64 = 25;
const BLOG_PAGE_SIZE: usize = 10;

#[derive(Clone)]
pub struct AppState {
    pub store: Store,
    pub blog: Arc<Blog>,
    pub auth: Arc<Auth>,
    pub writer: threaded_auth::records::Writer,
    /// Where background handle checks go.
    pub handles: handles::HandleChecker,
    /// hydrant's API, when the indexer is on; sign-in tracks the account.
    pub hydrant: Option<threaded_indexer::client::Hydrant>,
    /// Where the user review page finds when an account was created.
    pub plc: identity::Plc,
    pub clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    /// The rules in force. Admins change them at runtime.
    pub policy: Arc<std::sync::RwLock<threaded_core::policy::Policy>>,
    /// An Anthropic API key is configured.
    pub triage_key: bool,
    /// A rules version exists. Triage reads nothing until one does.
    pub rules_ready: Arc<std::sync::atomic::AtomicBool>,
    /// The public origin, for absolute URLs in feeds and link previews.
    pub origin: origin::Origin,
}

impl AppState {
    pub fn new(store: Store, blog: Blog, auth: Auth) -> AppState {
        let writer = threaded_auth::records::Writer::Pds(auth.clients.clone());
        let handles = handles::HandleChecker::Network(auth.clients.clone());
        AppState {
            store,
            blog: Arc::new(blog),
            auth: Arc::new(auth),
            writer,
            handles,
            hydrant: None,
            plc: identity::Plc::directory(),
            clock: Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0)
            }),
            policy: Default::default(),
            triage_key: false,
            rules_ready: Default::default(),
            origin: origin::Origin::local(8080),
        }
    }

    /// A copy of the rules in force.
    pub fn policy(&self) -> threaded_core::policy::Policy {
        self.policy.read().map(|p| p.clone()).unwrap_or_default()
    }

    pub fn set_policy(&self, p: threaded_core::policy::Policy) {
        if let Ok(mut w) = self.policy.write() {
            *w = p;
        }
    }

    /// Whether new items are queued for AI triage: a key and rules exist.
    pub fn triage_on(&self) -> bool {
        self.triage_key && self.rules_ready.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl FromRef<AppState> for axum_extra::extract::cookie::Key {
    fn from_ref(st: &AppState) -> Self {
        st.auth.key.clone()
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(front))
        .route("/newest", get(newest))
        .route("/active", get(active))
        .route("/top", get(top))
        .route("/t/{tag}", get(by_tag))
        .route("/domain/{domain}", get(by_domain))
        .route("/s/{short_id}", get(write::story_page))
        .route(
            "/s/{id}/suggest",
            get(write::suggest_page).post(write::suggest),
        )
        .route(
            "/mod/suggestions/{id}",
            axum::routing::post(staff::decide_suggestion),
        )
        .route(
            "/mod/verdicts/{id}",
            axum::routing::post(staff::decide_verdict),
        )
        .route("/mod", get(staff::mod_queue))
        .route(
            "/mod/s/{id}",
            get(staff::mod_story).post(staff::mod_story_action),
        )
        .route(
            "/mod/c/{id}",
            get(staff::mod_comment).post(staff::mod_comment_action),
        )
        .route(
            "/mod/u/{handle}",
            get(staff::mod_user).post(staff::mod_user_action),
        )
        .route("/moderations", get(staff::moderations))
        .route("/admin", get(admin::home))
        .route(
            "/admin/rules",
            get(admin::rules_page).post(admin::rules_save),
        )
        .route("/admin/breaker", axum::routing::post(admin::breaker_reset))
        .route("/moderations/stats", get(staff::moderation_stats))
        .route(
            "/admin/settings",
            get(admin::settings_page).post(admin::settings_save),
        )
        .route("/admin/tags", get(admin::tags_page).post(admin::tags_save))
        .route(
            "/admin/people",
            get(admin::people_page).post(admin::people_save),
        )
        .route(
            "/s/{short_id}/edit",
            get(write::story_edit_page).post(write::story_edit),
        )
        .route(
            "/s/{short_id}/delete",
            axum::routing::post(write::story_delete),
        )
        .route("/c/{short_id}/reply", get(write::reply_page))
        .route(
            "/c/{short_id}/edit",
            get(write::comment_edit_page).post(write::comment_edit),
        )
        .route(
            "/c/{short_id}/delete",
            axum::routing::post(write::comment_delete),
        )
        .route("/submit", get(write::submit_page).post(write::submit))
        .route("/comment", axum::routing::post(write::comment))
        .route("/vote", axum::routing::post(write::vote))
        .route("/flag", axum::routing::post(write::flag))
        .route(
            "/settings/triage",
            axum::routing::post(write::settings_triage),
        )
        .route("/settings", get(write::settings_page).post(write::settings))
        .route("/u/{handle}", get(write::profile))
        .route("/inbox", get(inbox::inbox))
        .route("/saved", get(saved))
        .route("/hidden", get(hidden))
        .route("/s/{short_id}/save", axum::routing::post(write::save))
        .route("/s/{short_id}/hide", axum::routing::post(write::hide))
        .route("/settings/tags", axum::routing::post(write::settings_tags))
        .route("/search", get(search_page))
        .route("/tags", get(tags))
        .route("/blog", get(blog_index))
        .route("/blog/{slug}", get(blog_post))
        .route("/login", get(auth::login).post(auth::login_submit))
        .route("/login/go", axum::routing::post(auth::login_go))
        .route("/login/typeahead", get(auth::typeahead))
        .route("/oauth/callback", get(auth::callback))
        .route("/oauth-client-metadata.json", get(auth::client_metadata))
        .route("/oauth/jwks.json", get(auth::jwks))
        .route("/logout", axum::routing::post(auth::logout))
        .route("/healthz", get(|| async { "ok" }))
        .route("/static/site.css", get(css))
        .route("/static/login.js", get(login_js))
        .route("/static/vote.js", get(vote_js))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            error_pages,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::cookie_layer,
        ))
        .layer(tower_http::catch_panic::CatchPanicLayer::new())
        .with_state(state)
}

pub struct StoryView {
    pub id: i64,
    pub author_did: String,
    pub voted: bool,
    pub can_vote: bool,
    /// The signed-in reader saved this story.
    pub saved: bool,
    /// The signed-in reader hid this story from their listings.
    pub hidden: bool,
    /// Comments since the signed-in reader last opened the story; zero
    /// when there are none or they never opened it.
    pub new_comments: i64,
    /// The row offers save and hide: false for a story readers cannot see,
    /// shown to its author or a moderator.
    pub markable: bool,
    pub short_id: String,
    pub title: String,
    pub href: String,
    pub domain: String,
    pub score: i64,
    pub comments: i64,
    pub age: String,
    pub handle: String,
    pub user_id: i64,
    pub badges: Vec<threaded_store::Badge>,
    pub tags: Vec<String>,
}

impl StoryView {
    pub(crate) fn from_row(r: StoryRow, now: i64) -> StoryView {
        StoryView {
            id: r.id,
            author_did: r.author_did.clone(),
            user_id: r.user_id,
            badges: Vec::new(),
            voted: false,
            can_vote: false,
            saved: false,
            hidden: false,
            markable: true,
            new_comments: 0,
            href: r
                .url
                .clone()
                .unwrap_or_else(|| format!("/s/{}", r.short_id)),
            domain: r.domain.clone().unwrap_or_default(),
            tags: r.tag_list().iter().map(|t| t.to_string()).collect(),
            age: ago(now - r.created_at),
            short_id: r.short_id,
            title: r.title,
            score: r.score,
            comments: r.comment_count,
            handle: r.handle,
        }
    }
}

pub fn ago(secs: i64) -> String {
    let (n, unit) = match secs.max(0) {
        s if s < 60 => return "just now".to_string(),
        s if s < 3600 => (s / 60, "minute"),
        s if s < 86_400 => (s / 3600, "hour"),
        s if s < 86_400 * 30 => (s / 86_400, "day"),
        s if s < 86_400 * 365 => (s / (86_400 * 30), "month"),
        s => (s / (86_400 * 365), "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

#[derive(Template)]
#[template(path = "list.html")]
struct ListPage {
    site: &'static str,
    viewer: Viewer,
    here: String,
    heading: String,
    hide_heading: bool,
    nav: &'static str,
    /// A line under the heading, for the reader's own lists.
    lead: &'static str,
    empty: &'static str,
    stories: Vec<StoryView>,
    page: i64,
    prev: Option<String>,
    next: Option<String>,
}

#[derive(Template)]
#[template(path = "search.html")]
struct SearchPage {
    site: &'static str,
    viewer: Viewer,
    here: String,
    nav: &'static str,
    q: String,
    searched: bool,
    stories: Vec<StoryView>,
}

#[derive(Template)]
#[template(path = "tags.html")]
struct TagsPage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    tags: Vec<(String, String)>,
}

#[derive(Template)]
#[template(path = "blog_index.html")]
struct BlogIndexPage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    posts: Vec<Arc<Post>>,
    page: usize,
    prev: Option<String>,
    next: Option<String>,
}

#[derive(Template)]
#[template(path = "blog_post.html")]
struct BlogPostPage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    post: Arc<Post>,
}

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorPage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    message: String,
}

pub enum AppError {
    NotFound,
    Forbidden,
    /// Signed in, but without the role the page needs; the message says which.
    Denied(&'static str),
    /// Sign-in is needed; returns to this path afterwards.
    SignIn(String),
    /// The request breaks a rule; the message says which.
    Refused(String),
    /// The user's PDS refused a write.
    Upstream(String),
    Store(threaded_store::Error),
    Render(askama::Error),
}

impl From<threaded_store::Error> for AppError {
    fn from(e: threaded_store::Error) -> Self {
        AppError::Store(e)
    }
}

impl From<askama::Error> for AppError {
    fn from(e: askama::Error) -> Self {
        AppError::Render(e)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            AppError::SignIn(next) => {
                let next: String = next
                    .bytes()
                    .map(|b| match b {
                        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' => {
                            (b as char).to_string()
                        }
                        _ => format!("%{b:02X}"),
                    })
                    .collect();
                return axum::response::Redirect::to(&format!("/login?next={next}"))
                    .into_response();
            }
            AppError::Refused(m) => (StatusCode::BAD_REQUEST, m),
            AppError::Upstream(m) => {
                tracing::warn!(error = %m, "PDS write refused");
                (
                    StatusCode::BAD_GATEWAY,
                    format!("Your server refused the change: {m}"),
                )
            }
            AppError::NotFound => (StatusCode::NOT_FOUND, "Not found.".to_string()),
            AppError::Denied(m) => (StatusCode::FORBIDDEN, m.to_string()),
            AppError::Forbidden => (
                StatusCode::FORBIDDEN,
                "That form expired. Go back, reload the page, and try again.".to_string(),
            ),
            AppError::Store(e) => {
                tracing::error!(error = %e, "store");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Something went wrong.".to_string(),
                )
            }
            AppError::Render(e) => {
                tracing::error!(error = %e, "render");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Something went wrong.".to_string(),
                )
            }
        };
        // Rendered here without a viewer; `error_pages` renders it again
        // with the signed-in user's header.
        let page = ErrorPage {
            site: SITE_NAME,
            viewer: Viewer::default(),
            nav: "",
            message: message.clone(),
        };
        let mut res = match page.render() {
            Ok(body) => (status, Html(body)).into_response(),
            Err(_) => status.into_response(),
        };
        res.extensions_mut().insert(ErrorMessage(message));
        res
    }
}

/// The message of an error response, for `error_pages`.
#[derive(Clone)]
struct ErrorMessage(String);

/// Renders error pages with the viewer, so a signed-in user's header stays
/// signed in on a 404 or a 403. `AppError` cannot see the request itself.
async fn error_pages(
    State(st): State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    use axum::extract::FromRequestParts;
    let (parts, body) = req.into_parts();
    let mut saved = parts.clone();
    let res = next
        .run(axum::extract::Request::from_parts(parts, body))
        .await;
    let Some(ErrorMessage(message)) = res.extensions().get::<ErrorMessage>().cloned() else {
        return res;
    };
    let Ok(viewer) = Viewer::from_request_parts(&mut saved, &st).await;
    let page = ErrorPage {
        site: SITE_NAME,
        viewer,
        nav: "",
        message,
    };
    match page.render() {
        Ok(html) => {
            let (mut head, _) = res.into_parts();
            head.headers.remove(header::CONTENT_LENGTH);
            Response::from_parts(head, axum::body::Body::from(html))
        }
        Err(_) => res,
    }
}

pub(crate) type Page = Result<Html<String>, AppError>;

#[derive(Deserialize, Default)]
struct PageParams {
    page: Option<i64>,
    t: Option<String>,
}

struct Spec<'a> {
    listing: Listing,
    filter: Filter,
    heading: String,
    hide_heading: bool,
    nav: &'static str,
    lead: &'static str,
    empty: &'static str,
    base: &'a str,
    extra: &'a str,
}

impl<'a> Spec<'a> {
    fn new(listing: Listing, base: &'a str) -> Spec<'a> {
        Spec {
            listing,
            filter: Filter::default(),
            heading: String::new(),
            hide_heading: false,
            nav: "",
            lead: "",
            empty: "No stories.",
            base,
            extra: "",
        }
    }
}

async fn listing(st: &AppState, viewer: Viewer, mut spec: Spec<'_>, page: Option<i64>) -> Page {
    let page = page.unwrap_or(1).max(1);
    let now = (st.clock)();
    spec.filter.reader = viewer.me.as_ref().map(|m| m.user_id);
    let rows = st
        .store
        .list(
            spec.listing,
            &spec.filter,
            PAGE_SIZE + 1,
            (page - 1) * PAGE_SIZE,
        )
        .await?;
    let more = rows.len() as i64 > PAGE_SIZE;
    let mut stories: Vec<StoryView> = rows
        .into_iter()
        .take(PAGE_SIZE as usize)
        .map(|r| StoryView::from_row(r, now))
        .collect();
    prepare_rows(st, &viewer, &mut stories).await?;
    let link = |p: i64| format!("{}?page={p}{}", spec.base, spec.extra);
    let here = if page > 1 {
        link(page)
    } else {
        spec.base.to_string()
    };
    Ok(Html(
        ListPage {
            site: SITE_NAME,
            viewer,
            here,
            hide_heading: spec.hide_heading,
            heading: spec.heading,
            nav: spec.nav,
            lead: spec.lead,
            empty: spec.empty,
            stories,
            page,
            prev: (page > 1).then(|| link(page - 1)),
            next: more.then(|| link(page + 1)),
        }
        .render()?,
    ))
}

async fn front(State(st): State<AppState>, viewer: Viewer, Query(p): Query<PageParams>) -> Page {
    let spec = Spec {
        nav: "front",
        heading: "Front page".into(),
        hide_heading: true,
        ..Spec::new(Listing::Front, "/")
    };
    listing(&st, viewer, spec, p.page).await
}

async fn newest(State(st): State<AppState>, viewer: Viewer, Query(p): Query<PageParams>) -> Page {
    let spec = Spec {
        nav: "newest",
        heading: "Newest".into(),
        ..Spec::new(Listing::New, "/newest")
    };
    listing(&st, viewer, spec, p.page).await
}

async fn active(State(st): State<AppState>, viewer: Viewer, Query(p): Query<PageParams>) -> Page {
    let spec = Spec {
        nav: "active",
        heading: "Active discussions".into(),
        ..Spec::new(Listing::Active, "/active")
    };
    listing(&st, viewer, spec, p.page).await
}

async fn top(State(st): State<AppState>, viewer: Viewer, Query(p): Query<PageParams>) -> Page {
    let (label, span, key) = match p.t.as_deref() {
        Some("day") => ("day", 86_400, "day"),
        Some("month") => ("month", 86_400 * 30, "month"),
        Some("year") => ("year", 86_400 * 365, "year"),
        Some("all") => ("all time", i64::MAX / 2, "all"),
        _ => ("week", 86_400 * 7, "week"),
    };
    let since = (st.clock)().saturating_sub(span);
    let extra = format!("&t={key}");
    let spec = Spec {
        nav: "top",
        heading: format!("Top: {label}"),
        extra: &extra,
        ..Spec::new(Listing::Top { since }, "/top")
    };
    listing(&st, viewer, spec, p.page).await
}

async fn by_tag(
    State(st): State<AppState>,
    viewer: Viewer,
    Path(tag): Path<String>,
    Query(p): Query<PageParams>,
) -> Page {
    if !threaded_core::tag::valid_name(&tag) {
        return Err(AppError::NotFound);
    }
    let base = format!("/t/{tag}");
    let spec = Spec {
        heading: format!("Tagged {tag}"),
        filter: Filter {
            tag: Some(tag),
            ..Default::default()
        },
        ..Spec::new(Listing::Front, &base)
    };
    listing(&st, viewer, spec, p.page).await
}

async fn by_domain(
    State(st): State<AppState>,
    viewer: Viewer,
    Path(domain): Path<String>,
    Query(p): Query<PageParams>,
) -> Page {
    let domain = domain.to_ascii_lowercase();
    if domain.is_empty()
        || !domain
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-.".contains(&b))
    {
        return Err(AppError::NotFound);
    }
    let base = format!("/domain/{domain}");
    let spec = Spec {
        heading: format!("Stories from {domain}"),
        filter: Filter {
            domain: Some(domain),
            ..Default::default()
        },
        ..Spec::new(Listing::New, &base)
    };
    listing(&st, viewer, spec, p.page).await
}

async fn saved(State(st): State<AppState>, viewer: Viewer, Query(p): Query<PageParams>) -> Page {
    let user_id = signed_in(&viewer, "/saved")?;
    let spec = Spec {
        nav: "saved",
        heading: "Saved stories".into(),
        lead: "Only you see this list.",
        empty: "No saved stories. Save one with the \"save\" link under it.",
        ..Spec::new(Listing::Saved { user_id }, "/saved")
    };
    listing(&st, viewer, spec, p.page).await
}

async fn hidden(State(st): State<AppState>, viewer: Viewer, Query(p): Query<PageParams>) -> Page {
    let user_id = signed_in(&viewer, "/hidden")?;
    let spec = Spec {
        heading: "Hidden stories".into(),
        lead: "Stories you hid stay out of your listings and search. Unhide one to bring it back.",
        empty: "No hidden stories.",
        ..Spec::new(Listing::Hidden { user_id }, "/hidden")
    };
    listing(&st, viewer, spec, p.page).await
}

/// The signed-in reader's user ID, or a redirect to sign in.
pub(crate) fn signed_in(viewer: &Viewer, next: &str) -> Result<i64, AppError> {
    viewer
        .me
        .as_ref()
        .map(|m| m.user_id)
        .ok_or_else(|| AppError::SignIn(next.to_string()))
}

#[derive(Deserialize)]
struct SearchParams {
    q: Option<String>,
}

async fn search_page(
    State(st): State<AppState>,
    viewer: Viewer,
    Query(p): Query<SearchParams>,
) -> Page {
    let q = p.q.unwrap_or_default();
    let parsed = search::parse(&q);
    let now = (st.clock)();
    let mut stories: Vec<StoryView> = st
        .store
        .search(&parsed, viewer.me.as_ref().map(|m| m.user_id), PAGE_SIZE, 0)
        .await?
        .into_iter()
        .map(|r| StoryView::from_row(r, now))
        .collect();
    prepare_rows(&st, &viewer, &mut stories).await?;
    Ok(Html(
        SearchPage {
            site: SITE_NAME,
            here: viewer.here.clone(),
            viewer,
            nav: "search",
            searched: !parsed.is_empty(),
            q,
            stories,
        }
        .render()?,
    ))
}

async fn tags(State(st): State<AppState>, viewer: Viewer) -> Page {
    Ok(Html(
        TagsPage {
            site: SITE_NAME,
            viewer,
            nav: "tags",
            tags: st.store.tags().await?,
        }
        .render()?,
    ))
}

#[derive(Deserialize)]
struct BlogParams {
    page: Option<usize>,
}

async fn blog_index(
    State(st): State<AppState>,
    viewer: Viewer,
    Query(p): Query<BlogParams>,
) -> Page {
    let page = p.page.unwrap_or(1).max(1);
    let all = st.blog.posts();
    let start = (page - 1).saturating_mul(BLOG_PAGE_SIZE);
    if start > 0 && start >= all.len() {
        return Err(AppError::NotFound);
    }
    let posts = all
        .iter()
        .skip(start)
        .take(BLOG_PAGE_SIZE)
        .cloned()
        .collect();
    let link = |n: usize| format!("/blog?page={n}");
    Ok(Html(
        BlogIndexPage {
            site: SITE_NAME,
            viewer,
            nav: "blog",
            posts,
            page,
            prev: (page > 1).then(|| link(page - 1)),
            next: (start + BLOG_PAGE_SIZE < all.len()).then(|| link(page + 1)),
        }
        .render()?,
    ))
}

async fn blog_post(State(st): State<AppState>, viewer: Viewer, Path(slug): Path<String>) -> Page {
    let post = st.blog.post(&slug).ok_or(AppError::NotFound)?;
    Ok(Html(
        BlogPostPage {
            site: SITE_NAME,
            viewer,
            nav: "blog",
            post,
        }
        .render()?,
    ))
}

/// Active role badges for these users, by user ID.
pub(crate) async fn badge_map(
    st: &AppState,
    user_ids: &[i64],
) -> Result<HashMap<i64, Vec<threaded_store::Badge>>, AppError> {
    let mut ids = user_ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    let mut map: HashMap<i64, Vec<threaded_store::Badge>> = HashMap::new();
    for b in st.store.badges(&ids).await? {
        map.entry(b.user_id).or_default().push(b);
    }
    Ok(map)
}

/// Fills in what a story row needs beyond its database row: the authors'
/// badges, and for a signed-in viewer, their upvotes, saves, hides, and
/// comments new since they last read each story.
pub(crate) async fn prepare_rows(
    st: &AppState,
    viewer: &Viewer,
    stories: &mut [StoryView],
) -> Result<(), AppError> {
    let authors: Vec<i64> = stories.iter().map(|s| s.user_id).collect();
    handles::refresh(st, &authors).await;
    let badges = badge_map(st, &authors).await?;
    for s in stories.iter_mut() {
        s.badges = badges.get(&s.user_id).cloned().unwrap_or_default();
    }
    let Some(me) = &viewer.me else { return Ok(()) };
    let ids: Vec<i64> = stories.iter().map(|s| s.id).collect();
    let voted = st.store.upvoted_stories(me.user_id, &ids).await?;
    let marks = st.store.marks(me.user_id, &ids).await?;
    let fresh: HashMap<i64, i64> = st
        .store
        .new_comments(me.user_id, &ids)
        .await?
        .into_iter()
        .collect();
    for s in stories.iter_mut() {
        s.can_vote = s.author_did != me.session.did;
        s.voted = voted.contains(&s.id);
        s.saved = marks.saved.contains(&s.id);
        s.hidden = marks.hidden.contains(&s.id);
        s.new_comments = fresh.get(&s.id).copied().unwrap_or(0);
    }
    Ok(())
}

async fn vote_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../static/vote.js"),
    )
}

async fn login_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        include_str!("../static/login.js"),
    )
}

async fn css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../static/site.css"),
    )
}

#[cfg(test)]
mod tests {
    use super::ago;

    #[test]
    fn ages() {
        assert_eq!(ago(5), "just now");
        assert_eq!(ago(60), "1 minute ago");
        assert_eq!(ago(7200), "2 hours ago");
        assert_eq!(ago(86_400 * 3), "3 days ago");
        assert_eq!(ago(86_400 * 400), "1 year ago");
        assert_eq!(ago(-10), "just now");
    }
}
