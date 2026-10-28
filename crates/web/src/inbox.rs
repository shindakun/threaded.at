//! `/inbox`: replies to the signed-in reader's stories and comments.

use askama::Template;
use axum::extract::{Query, State};
use axum::response::Html;
use serde::Deserialize;

use crate::auth::Viewer;
use crate::{ago, markdown, signed_in, AppState, Page, SITE_NAME};

const PAGE_SIZE: i64 = 25;

pub struct ReplyView {
    pub short_id: String,
    pub story_id: String,
    pub story_title: String,
    pub handle: String,
    /// A top-level comment on the reader's story, not a reply to a comment.
    pub on_story: bool,
    pub age: String,
    pub body_html: String,
    pub unread: bool,
}

#[derive(Template)]
#[template(path = "inbox.html")]
struct InboxPage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    replies: Vec<ReplyView>,
    page: i64,
    prev: Option<String>,
    next: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct InboxParams {
    page: Option<i64>,
}

/// Lists replies newest first, marks the ones written since the last visit,
/// then counts them all read.
pub async fn inbox(
    State(st): State<AppState>,
    mut viewer: Viewer,
    Query(p): Query<InboxParams>,
) -> Page {
    let user_id = signed_in(&viewer, "/inbox")?;
    let page = p.page.unwrap_or(1).max(1);
    let now = (st.clock)();
    let read_at = st.store.inbox_read_at(user_id).await?;
    let rows = st
        .store
        .replies(user_id, PAGE_SIZE + 1, (page - 1) * PAGE_SIZE)
        .await?;
    st.store.mark_inbox_read(user_id, now).await?;
    // The header was counted before this visit cleared it.
    viewer.inbox = 0;
    let more = rows.len() as i64 > PAGE_SIZE;
    let replies = rows
        .into_iter()
        .take(PAGE_SIZE as usize)
        .map(|c| ReplyView {
            unread: c.created_at > read_at,
            on_story: c.parent_id.is_none(),
            age: ago(now - c.created_at),
            body_html: markdown::user(&c.body),
            short_id: c.short_id,
            story_id: c.story_short_id,
            story_title: c.story_title,
            handle: c.handle,
        })
        .collect();
    let link = |n: i64| format!("/inbox?page={n}");
    Ok(Html(
        InboxPage {
            site: SITE_NAME,
            viewer,
            nav: "inbox",
            replies,
            page,
            prev: (page > 1).then(|| link(page - 1)),
            next: more.then(|| link(page + 1)),
        }
        .render()?,
    ))
}
