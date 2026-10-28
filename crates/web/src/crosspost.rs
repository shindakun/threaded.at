//! Bluesky cross-posting. A user turns it on in `/settings`, which needs a
//! second authorization for the `app.bsky.feed.post` scope; the submit form
//! then offers "Also post to Bluesky". The post is the story's title with a
//! link card to its page here. A failed post leaves the story alone: its
//! author sees why on the story page, with a button to try again.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::Form;
use serde::Deserialize;
use threaded_auth::client::{has_scopes, scopes, Flow, BSKY_POST};
use threaded_auth::flow;
use threaded_auth::records::{bsky_post_record, SessionRef, WriteError};
use threaded_store::{BskyPost, StoryRow, UserRow};

use crate::auth::{check_csrf, Viewer};
use crate::{markdown, AppError, AppState};

/// The session holds every scope `flow` asks for.
pub(crate) async fn granted(st: &AppState, s: &SessionRef, flow: Flow) -> bool {
    flow::granted_scopes(&st.auth.clients, &s.did, &s.session_id)
        .await
        .is_some_and(|g| has_scopes(&g, &scopes(st.auth.clients.ns, flow)))
}

/// Whether the submit form offers "Also post to Bluesky": cross-posting is
/// available here, the user turned it on, and the session can post.
pub(crate) async fn offered(st: &AppState, user: &UserRow, s: &SessionRef) -> bool {
    st.bsky && user.cross_post && granted(st, s, Flow::BSKY).await
}

/// What the link card says under the title: the start of a text post, or
/// where a link goes. Unlike a page's preview it has no comment count, which
/// would be out of date as soon as it was posted.
fn card_description(story: &StoryRow) -> String {
    let text = markdown::plain(&story.body);
    if !text.is_empty() {
        let mut short: String = text.chars().take(200).collect();
        if short.len() < text.len() {
            short.push('\u{2026}');
        }
        return short;
    }
    match story.domain.as_deref() {
        Some(d) if !d.is_empty() => format!("A link to {d}, on Threaded."),
        _ => "On Threaded.".to_string(),
    }
}

/// Posts a story to Bluesky for its author and records what happened.
pub(crate) async fn post(
    st: &AppState,
    session: &SessionRef,
    story: &StoryRow,
) -> Result<BskyPost, AppError> {
    let result = if story.hidden() {
        BskyPost::Failed(
            "The story is not visible to readers yet, so it was not posted. Try again once it is."
                .into(),
        )
    } else if !granted(st, session, Flow::BSKY).await {
        BskyPost::Failed(
            "Threaded does not have permission to post to Bluesky for you. Grant it in settings, then try again."
                .into(),
        )
    } else {
        let page = st.origin.url(&format!("/s/{}", story.short_id));
        let record = bsky_post_record(&story.title, &page, &card_description(story), (st.clock)());
        match st.writer.create(session, BSKY_POST, &record).await {
            Ok(r) => BskyPost::Posted(r.uri),
            Err(WriteError::SignedOut) => {
                BskyPost::Failed("Your sign-in had ended. Sign in again, then try again.".into())
            }
            Err(WriteError::Pds(m)) => {
                tracing::warn!(error = %m, story = %story.short_id, "cross-post refused");
                BskyPost::Failed(format!("Your server refused the post: {m}"))
            }
        }
    };
    st.store.set_bsky_post(story.id, &result).await?;
    Ok(result)
}

/// The address of a Bluesky post on bsky.app, from its AT-URI.
pub(crate) fn web_url(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("at://")?;
    let mut parts = rest.split('/');
    let (did, collection, rkey) = (parts.next()?, parts.next()?, parts.next()?);
    (collection == BSKY_POST && parts.next().is_none() && !rkey.is_empty())
        .then(|| format!("https://bsky.app/profile/{did}/post/{rkey}"))
}

#[derive(Deserialize)]
pub struct RetryForm {
    csrf: String,
}

/// "Try again" on a story whose post failed. Only its author may, and only
/// while no post exists, so a story is never posted twice.
pub async fn retry(
    State(st): State<AppState>,
    viewer: Viewer,
    Path(short_id): Path<String>,
    Form(f): Form<RetryForm>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    let here = format!("/s/{short_id}");
    let me = viewer
        .me
        .as_ref()
        .ok_or_else(|| AppError::SignIn(here.clone()))?;
    let story = st
        .store
        .story_any(&short_id)
        .await?
        .ok_or(AppError::NotFound)?;
    if story.user_id != me.user_id {
        return Err(AppError::Denied(
            "Only the story's author can post it to Bluesky.",
        ));
    }
    if !st.bsky {
        return Err(AppError::Refused(
            "Cross-posting to Bluesky is not available on this site.".into(),
        ));
    }
    if matches!(
        st.store.bsky_post(story.id).await?,
        Some(BskyPost::Posted(_))
    ) {
        return Ok(Redirect::to(&here).into_response());
    }
    post(&st, &me.session, &story).await?;
    Ok(Redirect::to(&here).into_response())
}

#[derive(Deserialize)]
pub struct SettingsForm {
    csrf: String,
    #[serde(default)]
    on: Option<String>,
}

/// Turns cross-posting on or off. On needs the scope first.
pub async fn settings(
    State(st): State<AppState>,
    viewer: Viewer,
    Form(f): Form<SettingsForm>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    let me = viewer
        .me
        .as_ref()
        .ok_or_else(|| AppError::SignIn("/settings".into()))?;
    let on = f.on.is_some();
    if on && !(st.bsky && granted(&st, &me.session, Flow::BSKY).await) {
        return Err(AppError::Refused(
            "Grant Threaded permission to post to Bluesky first.".into(),
        ));
    }
    st.store.set_cross_post(me.user_id, on).await?;
    Ok(Redirect::to("/settings?notice=saved").into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_addresses() {
        assert_eq!(
            web_url("at://did:plc:abc/app.bsky.feed.post/3mxyz").as_deref(),
            Some("https://bsky.app/profile/did:plc:abc/post/3mxyz")
        );
        assert_eq!(web_url("at://did:plc:abc/at.threaded.submission/3m"), None);
        assert_eq!(web_url("at://did:plc:abc/app.bsky.feed.post/"), None);
        assert_eq!(web_url("at://did:plc:abc/app.bsky.feed.post/a/b"), None);
        assert_eq!(web_url("https://bsky.app/x"), None);
    }
}
