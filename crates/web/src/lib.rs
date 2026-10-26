pub mod auth;
pub mod blog;

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
    pub clock: Arc<dyn Fn() -> i64 + Send + Sync>,
}

impl AppState {
    pub fn new(store: Store, blog: Blog, auth: Auth) -> AppState {
        AppState {
            store,
            blog: Arc::new(blog),
            auth: Arc::new(auth),
            clock: Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0)
            }),
        }
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
        .route("/s/{short_id}", get(story))
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
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::cookie_layer,
        ))
        .layer(tower_http::catch_panic::CatchPanicLayer::new())
        .with_state(state)
}

pub struct StoryView {
    pub short_id: String,
    pub title: String,
    pub href: String,
    pub domain: String,
    pub score: i64,
    pub comments: i64,
    pub age: String,
    pub handle: String,
    pub tags: Vec<String>,
}

impl StoryView {
    fn from_row(r: StoryRow, now: i64) -> StoryView {
        StoryView {
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
    heading: String,
    hide_heading: bool,
    nav: &'static str,
    stories: Vec<StoryView>,
    page: i64,
    prev: Option<String>,
    next: Option<String>,
}

#[derive(Template)]
#[template(path = "story.html")]
struct StoryPage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    s: StoryView,
}

#[derive(Template)]
#[template(path = "search.html")]
struct SearchPage {
    site: &'static str,
    viewer: Viewer,
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
            AppError::NotFound => (StatusCode::NOT_FOUND, "Not found.".to_string()),
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
        let page = ErrorPage {
            site: SITE_NAME,
            viewer: Viewer::default(),
            nav: "",
            message,
        };
        match page.render() {
            Ok(body) => (status, Html(body)).into_response(),
            Err(_) => status.into_response(),
        }
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
            base,
            extra: "",
        }
    }
}

async fn listing(st: &AppState, viewer: Viewer, spec: Spec<'_>, page: Option<i64>) -> Page {
    let page = page.unwrap_or(1).max(1);
    let now = (st.clock)();
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
    let stories = rows
        .into_iter()
        .take(PAGE_SIZE as usize)
        .map(|r| StoryView::from_row(r, now))
        .collect();
    let link = |p: i64| format!("{}?page={p}{}", spec.base, spec.extra);
    Ok(Html(
        ListPage {
            site: SITE_NAME,
            viewer,
            hide_heading: spec.hide_heading,
            heading: spec.heading,
            nav: spec.nav,
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

async fn story(State(st): State<AppState>, viewer: Viewer, Path(short_id): Path<String>) -> Page {
    let row = st.store.story(&short_id).await?.ok_or(AppError::NotFound)?;
    let now = (st.clock)();
    Ok(Html(
        StoryPage {
            site: SITE_NAME,
            viewer,
            nav: "",
            s: StoryView::from_row(row, now),
        }
        .render()?,
    ))
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
    let stories = st
        .store
        .search(&parsed, PAGE_SIZE, 0)
        .await?
        .into_iter()
        .map(|r| StoryView::from_row(r, now))
        .collect();
    Ok(Html(
        SearchPage {
            site: SITE_NAME,
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
