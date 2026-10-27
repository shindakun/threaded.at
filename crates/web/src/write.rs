use std::collections::HashMap;

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::{Form, Json};
// Repeated keys (one per checked tag) need serde_html_form.
use axum_extra::extract::Form as HtmlForm;
use serde::Deserialize;
use threaded_auth::client::{has_scopes, scopes, Flow};
use threaded_auth::flow;
use threaded_auth::records::{
    comment_record, submission_record, vote_record, RecordRef, SessionRef, WriteError,
};
use threaded_core::level::{self, Capability};
use threaded_core::link;
use threaded_core::policy::Policy;
use threaded_store::{
    is_locked, CommentInput, CommentRow, Context, Refusal, Source, StoryInput, StoryRow, Target,
    UserRow, VoteChange, TEXT_MAX_BYTES, TEXT_MAX_CHARS, TITLE_MAX,
};

use crate::auth::{check_csrf, Me, Viewer};
use crate::{ago, markdown, prepare_rows, AppError, AppState, Page, StoryView, SITE_NAME};

pub const STORY_REASONS: &[(&str, &str)] = &[
    ("off-topic", "Off-topic"),
    ("already-posted", "Already posted"),
    ("broken-link", "Broken link"),
    ("spam", "Spam"),
];
pub const COMMENT_REASONS: &[(&str, &str)] = &[
    ("off-topic", "Off-topic"),
    ("me-too", "Me-too"),
    ("troll", "Troll"),
    ("unkind", "Unkind"),
    ("spam", "Spam"),
];

pub(crate) fn reason_label(code: &str) -> String {
    STORY_REASONS
        .iter()
        .chain(COMMENT_REASONS)
        .find(|(c, _)| *c == code)
        .map(|(_, l)| l.to_string())
        .unwrap_or_else(|| code.to_string())
}

/// The signed-in user and their account, or a redirect to sign in.
struct Actor {
    me: Me,
    user: UserRow,
    account: level::Account,
    policy: Policy,
}

async fn actor(st: &AppState, viewer: &Viewer, next: &str) -> Result<Actor, AppError> {
    let me = viewer
        .me
        .clone()
        .ok_or_else(|| AppError::SignIn(next.to_string()))?;
    let user = st
        .store
        .user_by_did(&me.session.did)
        .await?
        .ok_or_else(|| AppError::SignIn(next.to_string()))?;
    let account = user.account((st.clock)());
    let policy = st.policy();
    Ok(Actor {
        me,
        user,
        account,
        policy,
    })
}

impl Actor {
    fn can(&self, cap: Capability) -> bool {
        level::can(&self.account, cap, &self.policy.thresholds)
    }

    /// The context for `accept` and its checks, for an item from the web form.
    fn context(&self, st: &AppState) -> Context<'_> {
        Context {
            policy: &self.policy,
            source: Source::Web,
            triage: st.triage_on(),
            now: (st.clock)(),
        }
    }
}

/// The session must hold the scopes this write needs; a session from before
/// writing existed holds `atproto` alone.
async fn require_scopes(
    st: &AppState,
    session: &SessionRef,
    flow: Flow,
    next: &str,
) -> Result<(), AppError> {
    let wanted = scopes(st.auth.clients.ns, flow);
    match flow::granted_scopes(&st.auth.clients, &session.did, &session.session_id).await {
        Some(g) if has_scopes(&g, &wanted) => Ok(()),
        _ => Err(AppError::SignIn(next.to_string())),
    }
}

fn write_err(e: WriteError, next: &str) -> AppError {
    match e {
        WriteError::SignedOut => AppError::SignIn(next.to_string()),
        WriteError::Pds(m) => AppError::Upstream(m),
    }
}

fn record_ref(uri: &Option<String>, cid: &Option<String>) -> Option<RecordRef> {
    Some(RecordRef {
        uri: uri.clone()?,
        cid: cid.clone()?,
    })
}

fn check_text(text: &str, required: bool) -> Result<(), AppError> {
    if required && text.trim().is_empty() {
        return Err(AppError::Refused(Refusal::NoText.to_string()));
    }
    if text.chars().count() > TEXT_MAX_CHARS || text.len() > TEXT_MAX_BYTES {
        return Err(AppError::Refused(Refusal::TooLong.to_string()));
    }
    Ok(())
}

// ---------- story page and comment thread ----------

pub struct CommentView {
    pub short_id: String,
    pub handle: String,
    pub badges: Vec<threaded_store::Badge>,
    pub age: String,
    pub body_html: String,
    pub score: i64,
    /// "[deleted]" or "[removed]": the comment stays only to hold its replies.
    pub placeholder: Option<&'static str>,
    /// Why the comment is hidden, shown to its author and to moderators.
    pub status: Option<String>,
    pub edited: bool,
    pub is_mine: bool,
    pub voted: bool,
    pub can_vote: bool,
    pub can_reply: bool,
    pub flag: FlagView,
}

/// What the flag control on an item shows.
pub struct FlagView {
    pub kind: &'static str,
    pub id: String,
    pub allowed: bool,
    pub current: Option<String>,
    pub reasons: &'static [(&'static str, &'static str)],
    /// Flag reasons with counts, shown only to the author.
    pub received: Vec<(String, i64)>,
}

pub enum ThreadEvent {
    ItemStart(Box<CommentView>),
    ItemEnd,
    ListOpen,
    ListClose,
}

#[derive(Template)]
#[template(path = "story.html")]
struct StoryPage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    here: String,
    s: StoryView,
    body_html: String,
    is_mine: bool,
    is_moderator: bool,
    can_suggest: bool,
    edited: bool,
    mod_edited: bool,
    /// Why the story is hidden, shown to its author and to moderators.
    status: Option<String>,
    /// "Locked: REASON" or "Locked: archived".
    lock_note: Option<String>,
    can_comment: bool,
    comment_note: Option<String>,
    flag: FlagView,
    thread: Vec<ThreadEvent>,
}

/// Why an item is hidden, worded for its author.
fn hidden_status(
    removed_reason: Option<&str>,
    removed: bool,
    held: bool,
    detached: bool,
    author_inactive: bool,
) -> Option<String> {
    if author_inactive {
        Some("Hidden while the network reports this account inactive".to_string())
    } else if removed {
        Some(format!(
            "Removed by a moderator: {}",
            removed_reason.unwrap_or("no reason given")
        ))
    } else if held {
        Some("Held for review: spam (automated)".to_string())
    } else if detached {
        Some("Hidden for review: the link changed after the edit window".to_string())
    } else {
        None
    }
}

fn flag_allowed(
    a: Option<&Actor>,
    author_id: i64,
    is_story: bool,
    created_at: i64,
    score: i64,
    now: i64,
) -> bool {
    let Some(a) = a else { return false };
    if a.user.id == author_id {
        return false;
    }
    let p = &a.policy;
    let (cap, days, floor) = if is_story {
        (Capability::FlagStory, p.story_flag_days, p.story_flag_floor)
    } else {
        (
            Capability::FlagComment,
            p.comment_flag_days,
            p.comment_flag_floor,
        )
    };
    a.can(cap) && now - created_at <= days * 86_400 && score > floor
}

/// The facts about a story or comment that decide who may flag it.
struct Flaggable<'a> {
    target: Target,
    short_id: &'a str,
    author_id: i64,
    created_at: i64,
    score: i64,
}

async fn flag_view(
    st: &AppState,
    a: Option<&Actor>,
    item: Flaggable<'_>,
    current: Option<&str>,
) -> Result<FlagView, AppError> {
    let Flaggable {
        target,
        short_id,
        author_id,
        created_at,
        score,
    } = item;
    let is_story = matches!(target, Target::Story(_));
    let now = (st.clock)();
    let is_mine = a.is_some_and(|a| a.user.id == author_id);
    let received = if is_mine {
        st.store
            .flag_reasons(target)
            .await?
            .into_iter()
            .map(|(r, n)| (reason_label(&r), n))
            .collect()
    } else {
        Vec::new()
    };
    Ok(FlagView {
        kind: if is_story { "story" } else { "comment" },
        id: short_id.to_string(),
        allowed: current.is_some() || flag_allowed(a, author_id, is_story, created_at, score, now),
        current: current.map(reason_label),
        reasons: if is_story {
            STORY_REASONS
        } else {
            COMMENT_REASONS
        },
        received,
    })
}

pub async fn story_page(
    State(st): State<AppState>,
    viewer: Viewer,
    Path(short_id): Path<String>,
) -> Result<Response, AppError> {
    let row = st
        .store
        .story_any(&short_id)
        .await?
        .ok_or(AppError::NotFound)?;
    if let Some(target) = &row.merged_into {
        return Ok(Redirect::permanent(&format!("/s/{target}")).into_response());
    }
    let now = (st.clock)();
    let here = format!("/s/{short_id}");
    let a = match &viewer.me {
        Some(_) => actor(&st, &viewer, &here).await.ok(),
        None => None,
    };
    let is_mine = a.as_ref().is_some_and(|a| a.user.id == row.user_id);
    let is_moderator = viewer.is_moderator();
    if row.hidden() && !is_mine && !is_moderator {
        return Err(AppError::NotFound);
    }
    let policy = st.policy();
    let votes: HashMap<Target, threaded_store::VoteRow> = match &a {
        Some(a) => st
            .store
            .votes_on_story(a.user.id, row.id)
            .await?
            .into_iter()
            .collect(),
        None => HashMap::new(),
    };
    let story_vote = votes.get(&Target::Story(row.id));
    let locked = is_locked(&row, &policy, now);
    let mut flag = flag_view(
        &st,
        a.as_ref(),
        Flaggable {
            target: Target::Story(row.id),
            short_id: &row.short_id,
            author_id: row.user_id,
            created_at: row.created_at,
            score: row.score,
        },
        story_vote.and_then(|v| v.reason.as_deref()),
    )
    .await?;
    if locked && flag.current.is_none() {
        flag.allowed = false;
    }

    let (can_comment, comment_note) = match &a {
        None => (false, None),
        _ if row.hidden() => (false, None),
        Some(a) if !a.can(Capability::Comment) => {
            (false, Some("Your account cannot comment.".to_string()))
        }
        Some(_) if locked => (false, None),
        Some(_) if row.at_uri.is_none() => (
            false,
            Some("This story has no record to reply to.".to_string()),
        ),
        Some(_) => (true, None),
    };
    let lock_note = if row.locked {
        Some(format!(
            "Locked: {}",
            row.locked_reason.as_deref().unwrap_or("no reason given")
        ))
    } else if locked {
        Some("Locked: archived".to_string())
    } else {
        None
    };

    let comments = st.store.comments_for_story(row.id).await?;
    let thread = build_thread(
        &st,
        a.as_ref(),
        is_moderator,
        &comments,
        &votes,
        can_comment,
        locked,
    )
    .await?;

    let status = hidden_status(
        row.removed_reason.as_deref(),
        row.removed_at.is_some(),
        row.held_at.is_some(),
        row.detached_at.is_some(),
        row.author_inactive,
    );
    let mut s = StoryView::from_row(row.clone(), now);
    prepare_rows(&st, &viewer, std::slice::from_mut(&mut s)).await?;
    if locked || row.hidden() {
        s.can_vote = false;
    }
    Ok(Html(
        StoryPage {
            site: SITE_NAME,
            viewer,
            nav: "",
            here,
            body_html: markdown::user(&row.body),
            is_mine,
            is_moderator,
            can_suggest: !is_mine
                && !row.hidden()
                && a.as_ref().is_some_and(|a| a.can(Capability::SuggestEdit)),
            edited: row.edited_at.is_some(),
            mod_edited: row.mod_edited,
            status,
            lock_note,
            can_comment,
            comment_note,
            flag,
            thread,
            s,
        }
        .render()?,
    )
    .into_response())
}

async fn build_thread(
    st: &AppState,
    a: Option<&Actor>,
    is_moderator: bool,
    comments: &[CommentRow],
    votes: &HashMap<Target, threaded_store::VoteRow>,
    can_comment: bool,
    locked: bool,
) -> Result<Vec<ThreadEvent>, AppError> {
    let now = (st.clock)();
    let me = a.map(|a| a.user.id);
    let commenters: Vec<i64> = comments.iter().map(|c| c.user_id).collect();
    crate::handles::refresh(st, &commenters).await;
    let badges = crate::badge_map(st, &commenters).await?;
    let mut children: HashMap<Option<i64>, Vec<&CommentRow>> = HashMap::new();
    for c in comments {
        children.entry(c.parent_id).or_default().push(c);
    }
    // Whether this viewer sees the comment itself. A hidden comment shows to
    // its author and to moderators.
    let shown = |c: &CommentRow| {
        c.deleted_at.is_none() && (!c.hidden() || is_moderator || me == Some(c.user_id))
    };
    // A comment the viewer cannot see stays as a placeholder only while a
    // reply below it is visible.
    fn visible(
        c: &CommentRow,
        children: &HashMap<Option<i64>, Vec<&CommentRow>>,
        shown: &dyn Fn(&CommentRow) -> bool,
    ) -> bool {
        shown(c)
            || children
                .get(&Some(c.id))
                .is_some_and(|k| k.iter().any(|x| visible(x, children, shown)))
    }

    let mut events = Vec::new();
    let mut stack: Vec<(Option<i64>, usize)> = vec![(None, 0)];
    events.push(ThreadEvent::ListOpen);
    while let Some((parent, idx)) = stack.pop() {
        let kids: Vec<&CommentRow> = children
            .get(&parent)
            .map(|v| {
                v.iter()
                    .copied()
                    .filter(|c| visible(c, &children, &shown))
                    .collect()
            })
            .unwrap_or_default();
        if idx >= kids.len() {
            events.push(ThreadEvent::ListClose);
            if parent.is_some() {
                events.push(ThreadEvent::ItemEnd);
            }
            continue;
        }
        stack.push((parent, idx + 1));
        let c = kids[idx];
        let vote = votes.get(&Target::Comment(c.id));
        let is_mine = a.is_some_and(|a| a.user.id == c.user_id);
        let deleted = c.deleted_at.is_some();
        let placeholder = if deleted {
            Some("[deleted]")
        } else if !shown(c) {
            Some("[removed]")
        } else {
            None
        };
        let gone = placeholder.is_some();
        let mut flag = flag_view(
            st,
            a,
            Flaggable {
                target: Target::Comment(c.id),
                short_id: &c.short_id,
                author_id: c.user_id,
                created_at: c.created_at,
                score: c.score,
            },
            vote.and_then(|v| v.reason.as_deref()),
        )
        .await?;
        if (locked || c.hidden()) && flag.current.is_none() {
            flag.allowed = false;
        }
        events.push(ThreadEvent::ItemStart(Box::new(CommentView {
            short_id: c.short_id.clone(),
            handle: c.handle.clone(),
            badges: badges.get(&c.user_id).cloned().unwrap_or_default(),
            age: ago(now - c.created_at),
            body_html: if gone {
                String::new()
            } else {
                markdown::user(&c.body)
            },
            score: c.score,
            placeholder,
            status: if gone {
                None
            } else {
                hidden_status(
                    c.removed_reason.as_deref(),
                    c.removed_at.is_some(),
                    c.held_at.is_some(),
                    false,
                    c.author_inactive,
                )
            },
            edited: c.edited_at.is_some(),
            is_mine,
            voted: vote.is_some_and(|v| v.value == 1),
            can_vote: a.is_some() && !is_mine && !gone && !c.hidden() && !locked,
            can_reply: can_comment && !gone && !c.hidden() && c.at_uri.is_some(),
            flag,
        })));
        let has_kids = children
            .get(&Some(c.id))
            .is_some_and(|k| k.iter().any(|x| visible(x, &children, &shown)));
        if has_kids {
            events.push(ThreadEvent::ListOpen);
            stack.push((Some(c.id), 0));
        } else {
            events.push(ThreadEvent::ItemEnd);
        }
    }
    if events.len() == 2 {
        events.clear();
    }
    Ok(events)
}

// ---------- submit ----------

pub struct TagChoice {
    pub name: String,
    pub description: String,
    pub checked: bool,
}

#[derive(Template)]
#[template(path = "submit.html")]
struct SubmitPage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    error: Option<String>,
    title: String,
    url: String,
    text: String,
    authored: bool,
    tags: Vec<TagChoice>,
}

#[derive(Deserialize, Default)]
pub struct SubmitForm {
    #[serde(default)]
    csrf: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    authored: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

async fn tag_choices(
    st: &AppState,
    a: &Actor,
    checked: &[String],
) -> Result<Vec<TagChoice>, AppError> {
    Ok(st
        .store
        .all_tags()
        .await?
        .into_iter()
        .filter(|t| t.active)
        .filter(|t| !t.privileged || a.can(Capability::UsePrivilegedTag))
        .filter(|t| t.new_users_ok || a.can(Capability::UseRestrictedTag))
        .map(|t| TagChoice {
            checked: checked.contains(&t.name),
            name: t.name,
            description: t.description,
        })
        .collect())
}

fn submit_page_html(
    viewer: Viewer,
    f: SubmitForm,
    tags: Vec<TagChoice>,
    error: Option<String>,
) -> Page {
    Ok(Html(
        SubmitPage {
            site: SITE_NAME,
            viewer,
            nav: "submit",
            error,
            title: f.title,
            url: f.url,
            text: f.text,
            authored: f.authored.is_some(),
            tags,
        }
        .render()?,
    ))
}

pub async fn submit_page(State(st): State<AppState>, viewer: Viewer) -> Page {
    let a = actor(&st, &viewer, "/submit").await?;
    let tags = tag_choices(&st, &a, &[]).await?;
    submit_page_html(viewer, SubmitForm::default(), tags, None)
}

/// Tag names from a form, checked against the tag rules.
async fn validate_tags(st: &AppState, a: &Actor, names: &[String]) -> Result<Vec<String>, String> {
    match st.store.check_tags(&a.user, names, &a.context(st)).await {
        Ok(Ok(t)) => Ok(t),
        Ok(Err(r)) => Err(r.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

pub async fn submit(
    State(st): State<AppState>,
    viewer: Viewer,
    HtmlForm(f): HtmlForm<SubmitForm>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    let a = actor(&st, &viewer, "/submit").await?;
    let cx = a.context(&st);
    let input = StoryInput {
        title: &f.title,
        url: &f.url,
        text: &f.text,
        tags: &f.tags,
        authored: f.authored.is_some(),
    };
    let checked = match st.store.check_story(&a.user, &input, &cx).await? {
        Ok(c) => c,
        Err(Refusal::Duplicate(existing)) => {
            return Ok(Redirect::to(&format!("/s/{existing}?duplicate=1")).into_response())
        }
        Err(r) => {
            let tags = tag_choices(&st, &a, &f.tags).await?;
            return Ok(submit_page_html(viewer, f, tags, Some(r.to_string()))?.into_response());
        }
    };

    require_scopes(&st, &a.me.session, Flow::Base, "/submit").await?;
    let tag_refs: Vec<&str> = checked.tags.iter().map(String::as_str).collect();
    let ns = st.auth.clients.ns;
    let record = submission_record(
        ns,
        &checked.title,
        checked.link.as_ref().map(|l| l.url.as_str()),
        &checked.text,
        &tag_refs,
        checked.authored,
        cx.now,
    );
    let written = st
        .writer
        .create(&a.me.session, &ns.submission(), &record)
        .await
        .map_err(|e| write_err(e, "/submit"))?;
    let accepted = st
        .store
        .accept_story(&a.user, &input, (&written.uri, &written.cid), &cx)
        .await;
    if !matches!(accepted, Ok(Ok(_))) {
        let _ = st
            .writer
            .delete(
                &a.me.session,
                &ns.submission(),
                RecordRef::rkey(&written.uri),
            )
            .await;
    }
    match accepted? {
        Ok(acc) => Ok(Redirect::to(&format!("/s/{}", acc.short_id)).into_response()),
        Err(Refusal::Duplicate(existing)) => {
            Ok(Redirect::to(&format!("/s/{existing}?duplicate=1")).into_response())
        }
        Err(r) => {
            let tags = tag_choices(&st, &a, &f.tags).await?;
            Ok(submit_page_html(viewer, f, tags, Some(r.to_string()))?.into_response())
        }
    }
}

// ---------- comments ----------

#[derive(Deserialize)]
pub struct CommentForm {
    csrf: String,
    story: String,
    #[serde(default)]
    parent: String,
    text: String,
}

pub async fn comment(
    State(st): State<AppState>,
    viewer: Viewer,
    Form(f): Form<CommentForm>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    let here = format!("/s/{}", f.story);
    let a = actor(&st, &viewer, &here).await?;
    let cx = a.context(&st);
    let story = st.store.story(&f.story).await?.ok_or(AppError::NotFound)?;
    let parent = if f.parent.is_empty() {
        None
    } else {
        Some(
            st.store
                .comment(&f.parent)
                .await?
                .ok_or(AppError::NotFound)?,
        )
    };
    let input = CommentInput {
        story: &story,
        parent: parent.as_ref(),
        text: &f.text,
    };
    if let Err(r) = st.store.check_comment(&a.user, &input, &cx).await? {
        return Err(AppError::Refused(r.to_string()));
    }
    let subject = record_ref(&story.at_uri, &story.at_cid)
        .ok_or_else(|| AppError::Refused("This story has no record to reply to.".into()))?;
    let parent_ref =
        match &parent {
            None => None,
            Some(p) => Some(record_ref(&p.at_uri, &p.at_cid).ok_or_else(|| {
                AppError::Refused("That comment has no record to reply to.".into())
            })?),
        };

    require_scopes(&st, &a.me.session, Flow::Base, &here).await?;
    let ns = st.auth.clients.ns;
    let record = comment_record(ns, &subject, parent_ref.as_ref(), f.text.trim(), cx.now);
    let written = st
        .writer
        .create(&a.me.session, &ns.comment(), &record)
        .await
        .map_err(|e| write_err(e, &here))?;
    let accepted = st
        .store
        .accept_comment(&a.user, &input, (&written.uri, &written.cid), &cx)
        .await;
    if !matches!(accepted, Ok(Ok(_))) {
        let _ = st
            .writer
            .delete(&a.me.session, &ns.comment(), RecordRef::rkey(&written.uri))
            .await;
    }
    match accepted? {
        Ok(acc) => Ok(Redirect::to(&format!("{here}#c-{}", acc.short_id)).into_response()),
        Err(r) => Err(AppError::Refused(r.to_string())),
    }
}

#[derive(Template)]
#[template(path = "reply.html")]
struct ReplyPage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    story_id: String,
    story_title: String,
    parent_id: String,
    parent_handle: String,
    parent_html: String,
}

pub async fn reply_page(
    State(st): State<AppState>,
    viewer: Viewer,
    Path(short_id): Path<String>,
) -> Page {
    let c = st
        .store
        .comment(&short_id)
        .await?
        .ok_or(AppError::NotFound)?;
    actor(&st, &viewer, &format!("/c/{short_id}/reply")).await?;
    if c.deleted_at.is_some() {
        return Err(AppError::NotFound);
    }
    Ok(Html(
        ReplyPage {
            site: SITE_NAME,
            viewer,
            nav: "",
            story_id: c.story_short_id.clone(),
            story_title: c.story_title.clone(),
            parent_id: c.short_id.clone(),
            parent_handle: c.handle.clone(),
            parent_html: markdown::user(&c.body),
        }
        .render()?,
    ))
}

// ---------- votes and flags ----------

#[derive(Deserialize)]
pub struct VoteForm {
    csrf: String,
    kind: String,
    id: String,
    #[serde(default)]
    next: String,
}

struct Item {
    target: Target,
    author_id: i64,
    created_at: i64,
    score: i64,
    record: Option<RecordRef>,
    story_short_id: String,
    /// The story takes no new votes or flags.
    locked: bool,
}

/// A visible story or comment by kind and short ID.
async fn item(st: &AppState, kind: &str, id: &str) -> Result<Item, AppError> {
    let policy = st.policy();
    let now = (st.clock)();
    match kind {
        "story" => {
            let s = st.store.story(id).await?.ok_or(AppError::NotFound)?;
            Ok(Item {
                target: Target::Story(s.id),
                author_id: s.user_id,
                created_at: s.created_at,
                score: s.score,
                record: record_ref(&s.at_uri, &s.at_cid),
                locked: is_locked(&s, &policy, now),
                story_short_id: s.short_id,
            })
        }
        "comment" => {
            let c = st.store.comment(id).await?.ok_or(AppError::NotFound)?;
            if c.deleted_at.is_some() || c.hidden() {
                return Err(AppError::NotFound);
            }
            let s = st
                .store
                .story(&c.story_short_id)
                .await?
                .ok_or(AppError::NotFound)?;
            Ok(Item {
                target: Target::Comment(c.id),
                author_id: c.user_id,
                created_at: c.created_at,
                score: c.score,
                record: record_ref(&c.at_uri, &c.at_cid),
                locked: is_locked(&s, &policy, now),
                story_short_id: c.story_short_id,
            })
        }
        _ => Err(AppError::NotFound),
    }
}

fn wants_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("application/json"))
}

/// Withdraws a published upvote record. Failures are logged; the vote is
/// cleared either way.
async fn withdraw_vote_record(st: &AppState, session: &SessionRef, at_uri: &str) {
    let ns = st.auth.clients.ns;
    if let Err(e) = st
        .writer
        .delete(session, &ns.vote(), RecordRef::rkey(at_uri))
        .await
    {
        tracing::warn!(error = %e, "withdrawing a public upvote record");
    }
}

pub async fn vote(
    State(st): State<AppState>,
    viewer: Viewer,
    headers: HeaderMap,
    Form(f): Form<VoteForm>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    let it = item(&st, &f.kind, &f.id).await?;
    let back = if f.next.is_empty() {
        format!("/s/{}", it.story_short_id)
    } else {
        crate::auth::safe_next(&f.next)
    };
    let a = actor(&st, &viewer, &back).await?;
    if !a.can(Capability::Upvote) {
        return Err(AppError::Refused("Your account cannot vote.".into()));
    }
    if a.user.id == it.author_id {
        return Err(AppError::Refused(
            "You cannot vote on your own post.".into(),
        ));
    }
    let now = (st.clock)();
    let current = st.store.vote(a.user.id, it.target).await?;
    let removing = current.as_ref().is_some_and(|v| v.value == 1);
    if it.locked && !removing {
        return Err(AppError::Refused("This story is locked.".into()));
    }
    if let Some(uri) = current.as_ref().and_then(|v| v.at_uri.as_deref()) {
        withdraw_vote_record(&st, &a.me.session, uri).await;
    }
    let score = if removing {
        st.store
            .set_vote(a.user.id, it.target, VoteChange::Clear, now)
            .await?
    } else {
        let mut published: Option<RecordRef> = None;
        if a.user.public_votes {
            let has_scope = flow::granted_scopes(
                &st.auth.clients,
                &a.me.session.did,
                &a.me.session.session_id,
            )
            .await
            .is_some_and(|g| has_scopes(&g, &scopes(st.auth.clients.ns, Flow::Votes)));
            if let (true, Some(subject)) = (has_scope, &it.record) {
                let ns = st.auth.clients.ns;
                match st
                    .writer
                    .create(&a.me.session, &ns.vote(), &vote_record(ns, subject, now))
                    .await
                {
                    Ok(r) => published = Some(r),
                    Err(e) => tracing::warn!(error = %e, "publishing an upvote record"),
                }
            }
        }
        st.store
            .set_vote(
                a.user.id,
                it.target,
                VoteChange::Up {
                    at_uri: published.as_ref().map(|r| r.uri.as_str()),
                },
                now,
            )
            .await?
    };
    if wants_json(&headers) {
        return Ok(Json(serde_json::json!({ "score": score, "voted": !removing })).into_response());
    }
    Ok(Redirect::to(&back).into_response())
}

#[derive(Deserialize)]
pub struct FlagForm {
    csrf: String,
    kind: String,
    id: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    next: String,
}

pub async fn flag(
    State(st): State<AppState>,
    viewer: Viewer,
    Form(f): Form<FlagForm>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    let it = item(&st, &f.kind, &f.id).await?;
    let back = if f.next.is_empty() {
        format!("/s/{}", it.story_short_id)
    } else {
        crate::auth::safe_next(&f.next)
    };
    let a = actor(&st, &viewer, &back).await?;
    let now = (st.clock)();
    let current = st.store.vote(a.user.id, it.target).await?;
    if f.reason.is_empty() {
        if current.as_ref().is_some_and(|v| v.value == -1) {
            st.store
                .set_vote(a.user.id, it.target, VoteChange::Clear, now)
                .await?;
        }
        return Ok(Redirect::to(&back).into_response());
    }
    if it.locked {
        return Err(AppError::Refused("This story is locked.".into()));
    }
    let is_story = matches!(it.target, Target::Story(_));
    let reasons = if is_story {
        STORY_REASONS
    } else {
        COMMENT_REASONS
    };
    if !reasons.iter().any(|(c, _)| *c == f.reason) {
        return Err(AppError::Refused("Pick one of the listed reasons.".into()));
    }
    if !flag_allowed(
        Some(&a),
        it.author_id,
        is_story,
        it.created_at,
        it.score,
        now,
    ) {
        return Err(AppError::Refused("You cannot flag this.".into()));
    }
    if let Some(uri) = current.as_ref().and_then(|v| v.at_uri.as_deref()) {
        withdraw_vote_record(&st, &a.me.session, uri).await;
    }
    st.store
        .set_vote(
            a.user.id,
            it.target,
            VoteChange::Flag { reason: &f.reason },
            now,
        )
        .await?;
    Ok(Redirect::to(&back).into_response())
}

// ---------- edit and delete ----------

#[derive(Template)]
#[template(path = "edit_story.html")]
struct EditStoryPage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    short_id: String,
    error: Option<String>,
    title: String,
    url: String,
    url_locked: bool,
    text: String,
    tags: Vec<TagChoice>,
}

async fn own_story(
    st: &AppState,
    viewer: &Viewer,
    short_id: &str,
) -> Result<(Actor, StoryRow), AppError> {
    let here = format!("/s/{short_id}/edit");
    let a = actor(st, viewer, &here).await?;
    let s = st.store.story(short_id).await?.ok_or(AppError::NotFound)?;
    if s.user_id != a.user.id {
        return Err(AppError::Refused(
            "Only the author can change this story.".into(),
        ));
    }
    Ok((a, s))
}

pub async fn story_edit_page(
    State(st): State<AppState>,
    viewer: Viewer,
    Path(short_id): Path<String>,
) -> Page {
    let (a, s) = own_story(&st, &viewer, &short_id).await?;
    let now = (st.clock)();
    let current: Vec<String> = s.tag_list().iter().map(|t| t.to_string()).collect();
    let tags = tag_choices(&st, &a, &current).await?;
    Ok(Html(
        EditStoryPage {
            site: SITE_NAME,
            viewer,
            nav: "",
            short_id,
            error: None,
            url_locked: now - s.created_at > a.policy.url_edit_secs,
            title: s.title,
            url: s.url.unwrap_or_default(),
            text: s.body,
            tags,
        }
        .render()?,
    ))
}

pub async fn story_edit(
    State(st): State<AppState>,
    viewer: Viewer,
    Path(short_id): Path<String>,
    HtmlForm(f): HtmlForm<SubmitForm>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    let (a, s) = own_story(&st, &viewer, &short_id).await?;
    let now = (st.clock)();
    let url_locked = now - s.created_at > a.policy.url_edit_secs;
    let render_err =
        |msg: String, tags: Vec<TagChoice>, f: SubmitForm| -> Result<Response, AppError> {
            Ok(Html(
                EditStoryPage {
                    site: SITE_NAME,
                    viewer: viewer.clone(),
                    nav: "",
                    short_id: short_id.clone(),
                    error: Some(msg),
                    url_locked,
                    title: f.title,
                    url: f.url,
                    text: f.text,
                    tags,
                }
                .render()?,
            )
            .into_response())
        };
    let title = f.title.trim().to_string();
    if title.is_empty() || title.chars().count() > TITLE_MAX {
        let tags = tag_choices(&st, &a, &f.tags).await?;
        return render_err(format!("A title is 1 to {TITLE_MAX} characters."), tags, f);
    }
    if let Err(AppError::Refused(m)) = check_text(&f.text, false) {
        let tags = tag_choices(&st, &a, &f.tags).await?;
        return render_err(m, tags, f);
    }
    let link = if url_locked {
        s.url.as_deref().map(link::parse).transpose().ok().flatten()
    } else if f.url.trim().is_empty() {
        None
    } else {
        match link::parse(&f.url) {
            Ok(l) => Some(l),
            Err(e) => {
                let tags = tag_choices(&st, &a, &f.tags).await?;
                return render_err(format!("The link is not usable: {e}."), tags, f);
            }
        }
    };
    if link.is_none() && f.text.trim().is_empty() {
        let tags = tag_choices(&st, &a, &f.tags).await?;
        return render_err("Add a link, some text, or both.".into(), tags, f);
    }
    let tags = match validate_tags(&st, &a, &f.tags).await {
        Ok(t) => t,
        Err(m) => {
            let choices = tag_choices(&st, &a, &f.tags).await?;
            return render_err(m, choices, f);
        }
    };
    let tag_refs: Vec<&str> = tags.iter().map(String::as_str).collect();
    let text = f.text.trim();
    let here = format!("/s/{short_id}/edit");
    let mut cid = None;
    if let Some(uri) = &s.at_uri {
        require_scopes(&st, &a.me.session, Flow::Base, &here).await?;
        let ns = st.auth.clients.ns;
        let record = submission_record(
            ns,
            &title,
            link.as_ref().map(|l| l.url.as_str()),
            text,
            &tag_refs,
            s.user_is_author,
            s.created_at,
        );
        let r = st
            .writer
            .put(
                &a.me.session,
                &ns.submission(),
                RecordRef::rkey(uri),
                &record,
            )
            .await
            .map_err(|e| write_err(e, &here))?;
        cid = Some(r.cid);
    }
    st.store
        .edit_story(
            &s,
            &threaded_store::StoryEdit {
                title: &title,
                link: link.as_ref(),
                body: text,
                tags: &tag_refs,
                cid: cid.as_deref(),
            },
            now,
        )
        .await?;
    Ok(Redirect::to(&format!("/s/{short_id}")).into_response())
}

#[derive(Deserialize)]
pub struct CsrfOnly {
    csrf: String,
}

pub async fn story_delete(
    State(st): State<AppState>,
    viewer: Viewer,
    Path(short_id): Path<String>,
    Form(f): Form<CsrfOnly>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    let (a, s) = own_story(&st, &viewer, &short_id).await?;
    if let Some(uri) = &s.at_uri {
        let here = format!("/s/{short_id}");
        require_scopes(&st, &a.me.session, Flow::Base, &here).await?;
        let ns = st.auth.clients.ns;
        st.writer
            .delete(&a.me.session, &ns.submission(), RecordRef::rkey(uri))
            .await
            .map_err(|e| write_err(e, &here))?;
    }
    st.store.delete_story(s.id, a.user.id, (st.clock)()).await?;
    Ok(Redirect::to("/").into_response())
}

#[derive(Template)]
#[template(path = "edit_comment.html")]
struct EditCommentPage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    short_id: String,
    story_id: String,
    text: String,
}

async fn own_comment(
    st: &AppState,
    viewer: &Viewer,
    short_id: &str,
) -> Result<(Actor, CommentRow), AppError> {
    let a = actor(st, viewer, &format!("/c/{short_id}/edit")).await?;
    let c = st
        .store
        .comment(short_id)
        .await?
        .ok_or(AppError::NotFound)?;
    if c.user_id != a.user.id || c.deleted_at.is_some() {
        return Err(AppError::Refused(
            "Only the author can change this comment.".into(),
        ));
    }
    Ok((a, c))
}

pub async fn comment_edit_page(
    State(st): State<AppState>,
    viewer: Viewer,
    Path(short_id): Path<String>,
) -> Page {
    let (_, c) = own_comment(&st, &viewer, &short_id).await?;
    Ok(Html(
        EditCommentPage {
            site: SITE_NAME,
            viewer,
            nav: "",
            short_id,
            story_id: c.story_short_id,
            text: c.body,
        }
        .render()?,
    ))
}

#[derive(Deserialize)]
pub struct EditCommentForm {
    csrf: String,
    text: String,
}

pub async fn comment_edit(
    State(st): State<AppState>,
    viewer: Viewer,
    Path(short_id): Path<String>,
    Form(f): Form<EditCommentForm>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    let (a, c) = own_comment(&st, &viewer, &short_id).await?;
    check_text(&f.text, true)?;
    let now = (st.clock)();
    let text = f.text.trim();
    let here = format!("/s/{}#c-{short_id}", c.story_short_id);
    let mut cid = None;
    if let Some(uri) = &c.at_uri {
        require_scopes(&st, &a.me.session, Flow::Base, &here).await?;
        let story = st
            .store
            .story(&c.story_short_id)
            .await?
            .ok_or(AppError::NotFound)?;
        let subject = record_ref(&story.at_uri, &story.at_cid).ok_or(AppError::NotFound)?;
        let parent = match c.parent_id {
            Some(_) => {
                let all = st.store.comments_for_story(c.story_id).await?;
                all.iter()
                    .find(|p| Some(p.id) == c.parent_id)
                    .and_then(|p| record_ref(&p.at_uri, &p.at_cid))
            }
            None => None,
        };
        let ns = st.auth.clients.ns;
        let record = comment_record(ns, &subject, parent.as_ref(), text, c.created_at);
        let r = st
            .writer
            .put(&a.me.session, &ns.comment(), RecordRef::rkey(uri), &record)
            .await
            .map_err(|e| write_err(e, &here))?;
        cid = Some(r.cid);
    }
    st.store.edit_comment(&c, text, cid.as_deref(), now).await?;
    Ok(Redirect::to(&here).into_response())
}

pub async fn comment_delete(
    State(st): State<AppState>,
    viewer: Viewer,
    Path(short_id): Path<String>,
    Form(f): Form<CsrfOnly>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    let (a, c) = own_comment(&st, &viewer, &short_id).await?;
    let here = format!("/s/{}", c.story_short_id);
    if let Some(uri) = &c.at_uri {
        require_scopes(&st, &a.me.session, Flow::Base, &here).await?;
        let ns = st.auth.clients.ns;
        st.writer
            .delete(&a.me.session, &ns.comment(), RecordRef::rkey(uri))
            .await
            .map_err(|e| write_err(e, &here))?;
    }
    st.store.delete_comment(&c, (st.clock)()).await?;
    Ok(Redirect::to(&here).into_response())
}

// ---------- settings ----------

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsPage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    public_votes: bool,
    has_vote_scope: bool,
    triage_opt_out: bool,
    /// AI triage is running: an API key is set and site rules exist.
    triage_on: bool,
    did: String,
    notice: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct SettingsQuery {
    notice: Option<String>,
}

async fn has_vote_scope(st: &AppState, s: &SessionRef) -> bool {
    flow::granted_scopes(&st.auth.clients, &s.did, &s.session_id)
        .await
        .is_some_and(|g| has_scopes(&g, &scopes(st.auth.clients.ns, Flow::Votes)))
}

pub async fn settings_page(
    State(st): State<AppState>,
    viewer: Viewer,
    Query(q): Query<SettingsQuery>,
) -> Page {
    let a = actor(&st, &viewer, "/settings").await?;
    let has_vote_scope = has_vote_scope(&st, &a.me.session).await;
    Ok(Html(
        SettingsPage {
            site: SITE_NAME,
            viewer,
            nav: "",
            public_votes: a.user.public_votes,
            has_vote_scope,
            triage_opt_out: st.store.triage_opt_out(a.user.id).await?,
            triage_on: st.triage_on(),
            did: a.user.did.clone(),
            notice: q.notice,
        }
        .render()?,
    ))
}

#[derive(Deserialize)]
pub struct TriageForm {
    csrf: String,
    #[serde(default)]
    opt_out: Option<String>,
}

/// "Keep my posts out of AI triage": the posts go to a moderator instead.
pub async fn settings_triage(
    State(st): State<AppState>,
    viewer: Viewer,
    Form(f): Form<TriageForm>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    let a = actor(&st, &viewer, "/settings").await?;
    st.store
        .set_triage_opt_out(a.user.id, f.opt_out.is_some())
        .await?;
    Ok(Redirect::to("/settings?notice=saved").into_response())
}

#[derive(Deserialize)]
pub struct SettingsForm {
    csrf: String,
    #[serde(default)]
    public_votes: Option<String>,
    #[serde(default)]
    past: Option<String>,
}

pub async fn settings(
    State(st): State<AppState>,
    viewer: Viewer,
    Form(f): Form<SettingsForm>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    let a = actor(&st, &viewer, "/settings").await?;
    let want = f.public_votes.is_some();
    if want == a.user.public_votes {
        return Ok(Redirect::to("/settings?notice=saved").into_response());
    }
    if !has_vote_scope(&st, &a.me.session).await {
        return Err(AppError::Refused(
            "Grant Threaded permission to publish upvotes first.".into(),
        ));
    }
    let ns = st.auth.clients.ns;
    let now = (st.clock)();
    let mut failed = 0;
    if want {
        st.store.set_public_votes(a.user.id, true).await?;
        if f.past.is_some() {
            for v in st.store.upvote_records(a.user.id).await? {
                let (None, Some(uri), Some(cid)) = (&v.at_uri, v.subject_uri, v.subject_cid) else {
                    continue;
                };
                let subject = RecordRef { uri, cid };
                match st
                    .writer
                    .create(&a.me.session, &ns.vote(), &vote_record(ns, &subject, now))
                    .await
                {
                    Ok(r) => st.store.set_vote_record(v.vote_id, Some(&r.uri)).await?,
                    Err(e) => {
                        tracing::warn!(error = %e, "publishing a past upvote");
                        failed += 1;
                    }
                }
            }
        }
    } else {
        for v in st.store.upvote_records(a.user.id).await? {
            let Some(uri) = &v.at_uri else { continue };
            match st
                .writer
                .delete(&a.me.session, &ns.vote(), RecordRef::rkey(uri))
                .await
            {
                Ok(()) => st.store.set_vote_record(v.vote_id, None).await?,
                Err(e) => {
                    tracing::warn!(error = %e, "withdrawing an upvote record");
                    failed += 1;
                }
            }
        }
        if failed == 0 {
            st.store.set_public_votes(a.user.id, false).await?;
        }
    }
    let notice = if failed == 0 {
        "saved".to_string()
    } else {
        format!("failed-{failed}")
    };
    Ok(Redirect::to(&format!("/settings?notice={notice}")).into_response())
}

// ---------- profiles ----------

pub struct ProfileComment {
    pub short_id: String,
    pub story_id: String,
    pub story_title: String,
    pub age: String,
    pub body_html: String,
}

#[derive(Template)]
#[template(path = "profile.html")]
struct ProfilePage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    here: String,
    name: String,
    joined: String,
    karma: i64,
    level: &'static str,
    stories: Vec<StoryView>,
    comments: Vec<ProfileComment>,
    liked: Option<Vec<StoryView>>,
    /// The author's own hidden posts; `None` for everyone else.
    hidden: Option<Vec<HiddenPost>>,
}

/// A post of the author's that readers cannot see, with the reason, so the
/// author can find it again from their profile.
pub struct HiddenPost {
    pub href: String,
    pub label: String,
    pub status: String,
    pub age: String,
}

pub async fn profile(
    State(st): State<AppState>,
    viewer: Viewer,
    Path(handle): Path<String>,
) -> Page {
    let user = st
        .store
        .user_by_handle(&handle)
        .await?
        .ok_or(AppError::NotFound)?;
    let now = (st.clock)();
    let level = level::level(&user.account(now), &st.policy().thresholds);
    let mut stories: Vec<StoryView> = st
        .store
        .stories_by_user(user.id, 30)
        .await?
        .into_iter()
        .map(|r| StoryView::from_row(r, now))
        .collect();
    prepare_rows(&st, &viewer, &mut stories).await?;
    let comments = st
        .store
        .comments_by_user(user.id, 30)
        .await?
        .into_iter()
        .map(|c| ProfileComment {
            age: ago(now - c.created_at),
            body_html: markdown::user(&c.body),
            short_id: c.short_id,
            story_id: c.story_short_id,
            story_title: c.story_title,
        })
        .collect();
    let is_me = viewer
        .me
        .as_ref()
        .is_some_and(|m| m.session.did == user.did);
    let hidden = if is_me {
        let mut posts: Vec<(i64, HiddenPost)> = Vec::new();
        for r in st.store.hidden_stories_by_user(user.id, 30).await? {
            let status = hidden_status(
                r.removed_reason.as_deref(),
                r.removed_at.is_some(),
                r.held_at.is_some(),
                r.detached_at.is_some(),
                r.author_inactive,
            )
            .unwrap_or_default();
            posts.push((
                r.created_at,
                HiddenPost {
                    href: format!("/s/{}", r.short_id),
                    label: r.title,
                    status,
                    age: ago(now - r.created_at),
                },
            ));
        }
        for c in st.store.hidden_comments_by_user(user.id, 30).await? {
            let status = hidden_status(
                c.removed_reason.as_deref(),
                c.removed_at.is_some(),
                c.held_at.is_some(),
                false,
                c.author_inactive,
            )
            .unwrap_or_default();
            posts.push((
                c.created_at,
                HiddenPost {
                    href: format!("/s/{}#c-{}", c.story_short_id, c.short_id),
                    label: format!("Comment on {}", c.story_title),
                    status,
                    age: ago(now - c.created_at),
                },
            ));
        }
        posts.sort_by_key(|p| std::cmp::Reverse(p.0));
        Some(posts.into_iter().map(|p| p.1).collect())
    } else {
        None
    };
    let liked = if user.public_votes {
        let mut l: Vec<StoryView> = st
            .store
            .liked_stories(user.id, 30)
            .await?
            .into_iter()
            .map(|r| StoryView::from_row(r, now))
            .collect();
        prepare_rows(&st, &viewer, &mut l).await?;
        Some(l)
    } else {
        None
    };
    Ok(Html(
        ProfilePage {
            site: SITE_NAME,
            viewer,
            nav: "",
            here: format!("/u/{handle}"),
            name: user.name().to_string(),
            joined: ago(now - user.created_at),
            karma: user.karma,
            level: level.as_str(),
            stories,
            comments,
            liked,
            hidden,
        }
        .render()?,
    ))
}

// ---------- edit suggestions ----------

#[derive(Template)]
#[template(path = "suggest.html")]
struct SuggestPage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    short_id: String,
    error: Option<String>,
    title: String,
    tags: Vec<TagChoice>,
}

/// The story, if this user may suggest an edit to it.
async fn suggestable(
    st: &AppState,
    viewer: &Viewer,
    short_id: &str,
) -> Result<(Actor, StoryRow), AppError> {
    let here = format!("/s/{short_id}/suggest");
    let a = actor(st, viewer, &here).await?;
    let s = st.store.story(short_id).await?.ok_or(AppError::NotFound)?;
    if s.user_id == a.user.id {
        return Err(AppError::Refused(
            "This is your story: edit it instead.".into(),
        ));
    }
    if !a.can(Capability::SuggestEdit) {
        return Err(AppError::Refused(format!(
            "Suggesting edits needs karma of {} or more.",
            a.policy.thresholds.suggest_karma
        )));
    }
    Ok((a, s))
}

fn suggest_html(
    viewer: Viewer,
    short_id: String,
    title: String,
    tags: Vec<TagChoice>,
    error: Option<String>,
) -> Result<Response, AppError> {
    let status = if error.is_some() {
        axum::http::StatusCode::BAD_REQUEST
    } else {
        axum::http::StatusCode::OK
    };
    let html = SuggestPage {
        site: SITE_NAME,
        viewer,
        nav: "",
        short_id,
        error,
        title,
        tags,
    }
    .render()?;
    Ok((status, Html(html)).into_response())
}

async fn all_tag_choices(st: &AppState, checked: &[String]) -> Result<Vec<TagChoice>, AppError> {
    Ok(st
        .store
        .all_tags()
        .await?
        .into_iter()
        .filter(|t| t.active)
        .map(|t| TagChoice {
            checked: checked.contains(&t.name),
            name: t.name,
            description: t.description,
        })
        .collect())
}

pub async fn suggest_page(
    State(st): State<AppState>,
    viewer: Viewer,
    Path(short_id): Path<String>,
) -> Result<Response, AppError> {
    let (_, s) = suggestable(&st, &viewer, &short_id).await?;
    let current: Vec<String> = s.tag_list().iter().map(|t| t.to_string()).collect();
    let tags = all_tag_choices(&st, &current).await?;
    suggest_html(viewer, short_id, s.title, tags, None)
}

#[derive(Deserialize)]
pub struct SuggestForm {
    csrf: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    tags: Vec<String>,
}

pub async fn suggest(
    State(st): State<AppState>,
    viewer: Viewer,
    Path(short_id): Path<String>,
    HtmlForm(f): HtmlForm<SuggestForm>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    let (a, s) = suggestable(&st, &viewer, &short_id).await?;
    let title = f.title.trim();
    let mut tags: Vec<String> = Vec::new();
    for t in &f.tags {
        if !tags.contains(t) {
            tags.push(t.clone());
        }
    }
    tags.sort();
    let active: Vec<String> = st
        .store
        .all_tags()
        .await?
        .into_iter()
        .filter(|t| t.active)
        .map(|t| t.name)
        .collect();
    let problem = if title.is_empty() || title.chars().count() > TITLE_MAX {
        Some(Refusal::Title.to_string())
    } else if tags.is_empty() || tags.len() > threaded_core::tag::MAX_PER_STORY {
        Some("Pick 1 to 5 tags.".to_string())
    } else if let Some(t) = tags.iter().find(|t| !active.contains(t)) {
        Some(Refusal::UnknownTag(t.clone()).to_string())
    } else if title == s.title && tags.join(" ") == s.tags {
        Some("Nothing changed.".to_string())
    } else {
        None
    };
    if let Some(msg) = problem {
        let choices = all_tag_choices(&st, &tags).await?;
        return suggest_html(viewer, short_id, f.title, choices, Some(msg));
    }
    let new_title = (title != s.title).then_some(title);
    let joined = tags.join(" ");
    let new_tags = (joined != s.tags).then_some(joined.as_str());
    st.store
        .suggest(s.id, a.user.id, new_title, new_tags, (st.clock)())
        .await?;
    Ok(Redirect::to(&format!("/s/{short_id}?suggested=1")).into_response())
}
