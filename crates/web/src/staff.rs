//! Access to `/mod` and `/admin`, the moderator pages, and the public log.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum_extra::extract::Form as HtmlForm;
use serde::Deserialize;
use threaded_core::level;
use threaded_store::{By, LogRow, LogTarget, ModError, QueueItem, StoryOverride, Target, UserRow};

use crate::auth::{check_csrf, Viewer};
use crate::write::reason_label;
use crate::{ago, markdown, AppError, AppState, Page, SITE_NAME};

/// The role a page needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Need {
    Moderator,
    Admin,
}

/// A signed-in moderator or admin, with the role read from the database for
/// this request, so a revoked role stops working at once.
pub(crate) struct Staff {
    pub user: UserRow,
}

impl Staff {
    pub fn is_admin(&self) -> bool {
        self.user.role == "admin"
    }

    pub fn by(&self) -> By {
        By::Moderator(self.user.id)
    }
}

pub(crate) async fn staff(
    st: &AppState,
    viewer: &Viewer,
    need: Need,
    next: &str,
) -> Result<Staff, AppError> {
    let me = viewer
        .me
        .as_ref()
        .ok_or_else(|| AppError::SignIn(next.to_string()))?;
    let user = st
        .store
        .user_by_did(&me.session.did)
        .await?
        .ok_or_else(|| AppError::SignIn(next.to_string()))?;
    let ok = match need {
        Need::Moderator => user.role == "moderator" || user.role == "admin",
        Need::Admin => user.role == "admin",
    };
    if !ok || user.banned_at.is_some() {
        return Err(AppError::Denied(match need {
            Need::Moderator => "Moderators only.",
            Need::Admin => "Admins only.",
        }));
    }
    Ok(Staff { user })
}

pub(crate) fn outcome(r: Result<(), ModError>, back: &str) -> Result<Response, AppError> {
    match r {
        Ok(()) => Ok(Redirect::to(&format!("{back}?done=1")).into_response()),
        Err(ModError::NotFound) => Err(AppError::NotFound),
        Err(e) => Err(AppError::Refused(e.to_string())),
    }
}

#[derive(Deserialize, Default)]
pub struct DoneQuery {
    #[serde(default)]
    pub done: Option<String>,
    #[serde(default)]
    tab: Option<String>,
    #[serde(default)]
    page: Option<i64>,
}

/// A queue row as the templates show it.
pub struct QueueRow {
    pub kind: String,
    pub short_id: String,
    pub story_short_id: String,
    pub title: String,
    pub excerpt: String,
    pub author: String,
    pub author_karma: i64,
    pub tenure: String,
    pub score: i64,
    pub flags: Vec<(String, i64)>,
    pub age: String,
}

impl QueueRow {
    pub fn review_href(&self) -> String {
        match self.kind.as_str() {
            "story" => format!("/mod/s/{}", self.short_id),
            _ => format!("/mod/c/{}", self.short_id),
        }
    }

    pub fn item_href(&self) -> String {
        match self.kind.as_str() {
            "story" => format!("/s/{}", self.short_id),
            _ => format!("/s/{}#c-{}", self.story_short_id, self.short_id),
        }
    }
}

fn excerpt(text: &str) -> String {
    let t: String = text.chars().take(280).collect();
    if t.len() < text.len() {
        format!("{t}\u{2026}")
    } else {
        t
    }
}

async fn queue_row(st: &AppState, q: QueueItem, now: i64) -> Result<QueueRow, AppError> {
    let target = match q.kind.as_str() {
        "story" => Target::Story(q.id),
        _ => Target::Comment(q.id),
    };
    let flags = st
        .store
        .flag_reasons(target)
        .await?
        .into_iter()
        .map(|(r, n)| (reason_label(&r), n))
        .collect();
    Ok(QueueRow {
        excerpt: excerpt(&q.body),
        tenure: ago(now - q.author_since).replace(" ago", ""),
        age: ago(now - q.created_at),
        kind: q.kind,
        short_id: q.short_id,
        story_short_id: q.story_short_id,
        title: q.title,
        author: q.author,
        author_karma: q.author_karma,
        score: q.score,
        flags,
    })
}

// ---------- queue ----------

pub struct Tab {
    pub key: &'static str,
    pub label: &'static str,
    pub count: usize,
}

/// A triage verdict as moderators see it.
pub struct VerdictView {
    pub id: i64,
    pub verdict: String,
    pub rule: String,
    pub confidence: String,
    pub explanation: String,
    pub model: String,
    pub rules_version: Option<i64>,
    pub error: Option<String>,
    pub held: bool,
    pub decision: Option<String>,
}

impl VerdictView {
    /// The decisions that fit this verdict, with their button labels.
    pub fn choices(&self) -> [(&'static str, &'static str); 2] {
        if self.held {
            [("confirmed", "Confirm hold"), ("released", "Release")]
        } else {
            [("removed", "Remove"), ("dismissed", "Dismiss")]
        }
    }

    fn from(v: threaded_store::VerdictRow) -> VerdictView {
        VerdictView {
            id: v.id,
            verdict: v.verdict,
            rule: v.rule,
            confidence: v.confidence,
            explanation: v.explanation,
            model: v.model,
            rules_version: v.rules_version_id,
            error: v.error,
            held: v.held,
            decision: v.decision,
        }
    }
}

/// A suggested edit as the Edits tab shows it.
pub struct EditRow {
    pub id: i64,
    pub story_short_id: String,
    pub current_title: String,
    pub current_tags: String,
    pub title: Option<String>,
    pub tags: Option<String>,
    pub by: String,
    pub age: String,
}

#[derive(Template)]
#[template(path = "mod.html")]
struct ModPage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    done: bool,
    tabs: Vec<Tab>,
    tab: &'static str,
    rows: Vec<QueueRow>,
    triaged: Vec<(QueueRow, VerdictView)>,
    edits: Vec<EditRow>,
}

pub async fn mod_queue(
    State(st): State<AppState>,
    viewer: Viewer,
    Query(q): Query<DoneQuery>,
) -> Page {
    staff(&st, &viewer, Need::Moderator, "/mod").await?;
    let now = (st.clock)();
    let flagged = st.store.flagged(st.policy().flags_to_queue).await?;
    let suggestions = st.store.open_suggestions().await?;
    let holds = st.store.holds().await?;
    let triage_suggestions = st.store.triage_suggestions().await?;
    let tabs = vec![
        Tab {
            key: "holds",
            label: "Holds",
            count: holds.len(),
        },
        Tab {
            key: "flagged",
            label: "Flagged",
            count: flagged.len(),
        },
        Tab {
            key: "suggestions",
            label: "Suggestions",
            count: triage_suggestions.len(),
        },
        Tab {
            key: "edits",
            label: "Edits",
            count: suggestions.len(),
        },
    ];
    // With nothing given, open the first tab with work in it.
    let tab = tabs
        .iter()
        .find(|t| Some(t.key) == q.tab.as_deref())
        .or_else(|| tabs.iter().find(|t| t.count > 0))
        .unwrap_or(&tabs[1])
        .key;
    let mut rows = Vec::new();
    let mut edits = Vec::new();
    let mut triaged = Vec::new();
    match tab {
        "holds" | "suggestions" => {
            let list = if tab == "holds" {
                holds
            } else {
                triage_suggestions
            };
            for t in list {
                triaged.push((
                    queue_row(&st, t.item, now).await?,
                    VerdictView::from(t.verdict),
                ));
            }
        }
        "edits" => {
            edits = suggestions
                .into_iter()
                .map(|g| EditRow {
                    id: g.id,
                    story_short_id: g.story_short_id,
                    current_title: g.current_title,
                    current_tags: g.current_tags,
                    title: g.title,
                    tags: g.tags,
                    by: g.by_handle,
                    age: ago(now - g.created_at),
                })
                .collect()
        }
        _ => {
            for item in flagged {
                rows.push(queue_row(&st, item, now).await?);
            }
        }
    }
    Ok(Html(
        ModPage {
            site: SITE_NAME,
            viewer,
            nav: "mod",
            done: q.done.is_some(),
            tabs,
            tab,
            rows,
            triaged,
            edits,
        }
        .render()?,
    ))
}

#[derive(Deserialize)]
pub struct DecideForm {
    csrf: String,
    decision: String,
    #[serde(default)]
    reason: String,
}

pub async fn decide_suggestion(
    State(st): State<AppState>,
    viewer: Viewer,
    Path(id): Path<i64>,
    axum::Form(f): axum::Form<DecideForm>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    let me = staff(&st, &viewer, Need::Moderator, "/mod?tab=edits").await?;
    let accept = match f.decision.as_str() {
        "accept" => true,
        "dismiss" => false,
        _ => return Err(AppError::NotFound),
    };
    let r = st
        .store
        .decide_suggestion(id, accept, me.by(), &f.reason, (st.clock)())
        .await?;
    match r {
        Ok(()) => Ok(Redirect::to("/mod?tab=edits&done=1").into_response()),
        Err(e) => outcome(Err(e), "/mod?tab=edits"),
    }
}

#[derive(Deserialize)]
pub struct VerdictForm {
    csrf: String,
    decision: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    tab: String,
}

pub async fn decide_verdict(
    State(st): State<AppState>,
    viewer: Viewer,
    Path(id): Path<i64>,
    axum::Form(f): axum::Form<VerdictForm>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    let tab = if f.tab == "holds" {
        "holds"
    } else {
        "suggestions"
    };
    let back = format!("/mod?tab={tab}");
    let me = staff(&st, &viewer, Need::Moderator, &back).await?;
    if !matches!(
        f.decision.as_str(),
        "confirmed" | "released" | "removed" | "dismissed"
    ) {
        return Err(AppError::NotFound);
    }
    let r = st
        .store
        .decide_verdict(id, &f.decision, me.by(), &f.reason, (st.clock)())
        .await?;
    match r {
        Ok(()) => Ok(Redirect::to(&format!("{back}&done=1")).into_response()),
        Err(e) => outcome(Err(e), &back),
    }
}

// ---------- item review ----------

pub struct LogView {
    pub when: String,
    pub who: String,
    pub action: String,
    pub target: Option<(String, String)>,
    pub user: Option<String>,
    pub reason: String,
    pub detail: String,
}

fn log_view(r: LogRow, now: i64) -> LogView {
    let who = match r.actor.as_str() {
        "triage" => "triage".to_string(),
        "console" => "console".to_string(),
        _ => r.moderator.unwrap_or_else(|| "a moderator".into()),
    };
    let target = match (&r.story_short_id, &r.comment_short_id) {
        (Some(s), Some(c)) => Some((
            format!("/s/{s}#c-{c}"),
            format!("a comment on {}", r.story_title.clone().unwrap_or_default()),
        )),
        (Some(s), None) => Some((format!("/s/{s}"), r.story_title.clone().unwrap_or_default())),
        _ => r.tag_name.map(|t| (format!("/t/{t}"), format!("tag {t}"))),
    };
    LogView {
        when: ago(now - r.created_at),
        who,
        action: r.action,
        target,
        user: r.user_handle,
        reason: r.reason,
        detail: r.detail,
    }
}

pub struct FlagView {
    pub handle: String,
    pub reason: String,
    pub age: String,
}

pub struct RevisionView {
    pub when: String,
    pub title: Option<String>,
    pub url: Option<String>,
    pub tags: Option<String>,
    pub body: String,
}

pub struct TagPick {
    pub name: String,
    pub checked: bool,
}

#[derive(Template)]
#[template(path = "mod_item.html")]
struct ItemPage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    done: bool,
    kind: &'static str,
    action_url: String,
    row: QueueRow,
    body_html: String,
    url: Option<String>,
    removed: Option<String>,
    held: bool,
    locked: bool,
    merged_into: Option<String>,
    mod_edited: bool,
    tags: Vec<TagPick>,
    flags: Vec<FlagView>,
    revisions: Vec<RevisionView>,
    verdict: Option<VerdictView>,
    log: Vec<LogView>,
}

async fn item_page(
    st: &AppState,
    viewer: Viewer,
    target: Target,
    done: bool,
) -> Result<Html<String>, AppError> {
    let now = (st.clock)();
    let q = st
        .store
        .queue_item(target)
        .await?
        .ok_or(AppError::NotFound)?;
    let flags = st
        .store
        .flags_on(target)
        .await?
        .into_iter()
        .map(|f| FlagView {
            handle: f.handle,
            reason: reason_label(&f.reason),
            age: ago(now - f.created_at),
        })
        .collect();
    let revisions = st
        .store
        .revisions(target)
        .await?
        .into_iter()
        .map(|r| RevisionView {
            when: ago(now - r.edited_at),
            title: r.title,
            url: r.url,
            tags: r.tags,
            body: r.body,
        })
        .collect();
    let log_target = match target {
        Target::Story(id) => LogTarget::Story(id),
        Target::Comment(id) => LogTarget::Comment(id),
    };
    let log = st
        .store
        .mod_log_for(log_target)
        .await?
        .into_iter()
        .map(|r| log_view(r, now))
        .collect();
    let verdict = st.store.verdict_for(target).await?.map(VerdictView::from);
    let body_html = markdown::user(&q.body);
    let url = q.url.clone();
    let (kind, removed, held, locked, merged_into, mod_edited, tags) = match target {
        Target::Story(_) => {
            let s = st
                .store
                .story_any(&q.short_id)
                .await?
                .ok_or(AppError::NotFound)?;
            let current = s.tag_list();
            let tags = st
                .store
                .all_tags()
                .await?
                .into_iter()
                .filter(|t| t.active || current.contains(&t.name.as_str()))
                .map(|t| TagPick {
                    checked: current.contains(&t.name.as_str()),
                    name: t.name,
                })
                .collect();
            (
                "story",
                s.removed_at
                    .map(|_| s.removed_reason.clone().unwrap_or_default()),
                s.held_at.is_some(),
                s.locked,
                s.merged_into.clone(),
                s.mod_edited,
                tags,
            )
        }
        Target::Comment(id) => {
            let c = st
                .store
                .comment_by_id(id)
                .await?
                .ok_or(AppError::NotFound)?;
            (
                "comment",
                c.removed_at
                    .map(|_| c.removed_reason.clone().unwrap_or_default()),
                c.held_at.is_some(),
                false,
                None,
                false,
                Vec::new(),
            )
        }
    };
    let row = queue_row(st, q, now).await?;
    let action_url = row.review_href();
    Ok(Html(
        ItemPage {
            site: SITE_NAME,
            viewer,
            nav: "mod",
            done,
            kind,
            action_url,
            row,
            body_html,
            url,
            removed,
            held,
            locked,
            merged_into,
            mod_edited,
            tags,
            flags,
            revisions,
            verdict,
            log,
        }
        .render()?,
    ))
}

pub async fn mod_story(
    State(st): State<AppState>,
    viewer: Viewer,
    Path(id): Path<String>,
    Query(q): Query<DoneQuery>,
) -> Page {
    staff(&st, &viewer, Need::Moderator, &format!("/mod/s/{id}")).await?;
    let s = st.store.story_any(&id).await?.ok_or(AppError::NotFound)?;
    item_page(&st, viewer, Target::Story(s.id), q.done.is_some()).await
}

pub async fn mod_comment(
    State(st): State<AppState>,
    viewer: Viewer,
    Path(id): Path<String>,
    Query(q): Query<DoneQuery>,
) -> Page {
    staff(&st, &viewer, Need::Moderator, &format!("/mod/c/{id}")).await?;
    let c = st.store.comment(&id).await?.ok_or(AppError::NotFound)?;
    item_page(&st, viewer, Target::Comment(c.id), q.done.is_some()).await
}

#[derive(Deserialize)]
pub struct ActionForm {
    csrf: String,
    action: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    into: String,
}

pub async fn mod_story_action(
    State(st): State<AppState>,
    viewer: Viewer,
    Path(id): Path<String>,
    HtmlForm(f): HtmlForm<ActionForm>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    let back = format!("/mod/s/{id}");
    let me = staff(&st, &viewer, Need::Moderator, &back).await?;
    let s = st.store.story_any(&id).await?.ok_or(AppError::NotFound)?;
    let now = (st.clock)();
    let (by, reason) = (me.by(), f.reason.as_str());
    let r = match f.action.as_str() {
        "remove" => {
            st.store
                .remove(Target::Story(s.id), by, reason, now)
                .await?
        }
        "restore" => {
            st.store
                .restore(Target::Story(s.id), by, reason, now)
                .await?
        }
        "lock" => st.store.set_locked(s.id, true, by, reason, now).await?,
        "unlock" => st.store.set_locked(s.id, false, by, reason, now).await?,
        "unmerge" => st.store.unmerge(s.id, by, reason, now).await?,
        "clear-edit" => st.store.clear_override(&s, by, reason, now).await?,
        "merge" => {
            let into = f.into.trim().trim_start_matches("/s/");
            let into = into.rsplit('/').next().unwrap_or(into);
            match st.store.story_any(into).await? {
                Some(t) => st.store.merge(s.id, t.id, by, reason, now).await?,
                None => Err(ModError::Refused("There is no story with that ID.")),
            }
        }
        "edit" => {
            let title = f.title.trim();
            if title.is_empty() || title.chars().count() > threaded_store::TITLE_MAX {
                return Err(AppError::Refused(
                    threaded_store::Refusal::Title.to_string(),
                ));
            }
            let link = if f.url.trim().is_empty() {
                None
            } else {
                Some(
                    threaded_core::link::parse(&f.url)
                        .map_err(|e| AppError::Refused(format!("The link is not usable: {e}.")))?,
                )
            };
            if link.is_none() && s.body.trim().is_empty() {
                return Err(AppError::Refused(
                    "A story without text needs a link.".into(),
                ));
            }
            let all = st.store.all_tags().await?;
            let mut tags: Vec<String> = Vec::new();
            for t in &f.tags {
                if !all.iter().any(|a| &a.name == t) {
                    return Err(AppError::Refused(format!(
                        "There is no tag \u{201c}{t}\u{201d}."
                    )));
                }
                if !tags.contains(t) {
                    tags.push(t.clone());
                }
            }
            if tags.is_empty() || tags.len() > threaded_core::tag::MAX_PER_STORY {
                return Err(AppError::Refused("Pick 1 to 5 tags.".into()));
            }
            st.store
                .override_story(
                    &s,
                    &StoryOverride {
                        title,
                        link: link.as_ref(),
                        tags: &tags,
                    },
                    by,
                    reason,
                    now,
                )
                .await?
        }
        _ => return Err(AppError::NotFound),
    };
    outcome(r, &back)
}

pub async fn mod_comment_action(
    State(st): State<AppState>,
    viewer: Viewer,
    Path(id): Path<String>,
    HtmlForm(f): HtmlForm<ActionForm>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    let back = format!("/mod/c/{id}");
    let me = staff(&st, &viewer, Need::Moderator, &back).await?;
    let c = st.store.comment(&id).await?.ok_or(AppError::NotFound)?;
    let now = (st.clock)();
    let r = match f.action.as_str() {
        "remove" => {
            st.store
                .remove(Target::Comment(c.id), me.by(), &f.reason, now)
                .await?
        }
        "restore" => {
            st.store
                .restore(Target::Comment(c.id), me.by(), &f.reason, now)
                .await?
        }
        _ => return Err(AppError::NotFound),
    };
    outcome(r, &back)
}

// ---------- user review ----------

pub struct BadgeView {
    pub id: i64,
    pub label: String,
    pub link: Option<String>,
}

pub struct NoteView {
    pub moderator: String,
    pub note: String,
    pub age: String,
}

#[derive(Template)]
#[template(path = "mod_user.html")]
struct UserPage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    done: bool,
    handle: String,
    did: String,
    role: String,
    level: &'static str,
    joined: String,
    tenure_days: i64,
    karma: i64,
    banned: Option<String>,
    can_ban: bool,
    removals: i64,
    flags_received: Vec<(String, i64)>,
    flags_given: i64,
    flags_upheld: i64,
    badges: Vec<BadgeView>,
    notes: Vec<NoteView>,
    activity: Vec<QueueRow>,
    log: Vec<LogView>,
}

pub async fn mod_user(
    State(st): State<AppState>,
    viewer: Viewer,
    Path(handle): Path<String>,
    Query(q): Query<DoneQuery>,
) -> Page {
    let me = staff(&st, &viewer, Need::Moderator, &format!("/mod/u/{handle}")).await?;
    let now = (st.clock)();
    let u = st
        .store
        .user_by_handle(&handle)
        .await?
        .ok_or(AppError::NotFound)?;
    let policy = st.policy();
    let account = u.account(now);
    let lvl = level::level(&account, &policy.thresholds);
    let standing = st.store.standing(u.id).await?;
    let banned_reason = u
        .banned_at
        .map(|_| u.banned_reason.clone().unwrap_or_default());
    let badges = st
        .store
        .badges(&[u.id])
        .await?
        .into_iter()
        .map(|b| BadgeView {
            id: b.id,
            label: b.label,
            link: b.link,
        })
        .collect();
    let notes = st
        .store
        .notes(u.id)
        .await?
        .into_iter()
        .map(|n| NoteView {
            moderator: n.moderator,
            note: n.note,
            age: ago(now - n.created_at),
        })
        .collect();
    let mut activity = Vec::new();
    for item in st.store.activity(u.id, 30).await? {
        activity.push(queue_row(&st, item, now).await?);
    }
    let log = st
        .store
        .mod_log_for(LogTarget::User(u.id))
        .await?
        .into_iter()
        .map(|r| log_view(r, now))
        .collect();
    Ok(Html(
        UserPage {
            site: SITE_NAME,
            viewer,
            nav: "mod",
            done: q.done.is_some(),
            can_ban: u.id != me.user.id && (u.role == "user" || me.is_admin()),
            handle: u.handle.clone(),
            did: u.did.clone(),
            role: u.role.clone(),
            level: lvl.as_str(),
            joined: ago(now - u.created_at),
            tenure_days: account.tenure_days,
            karma: u.karma,
            banned: banned_reason,
            removals: standing.removals,
            flags_received: standing
                .flags_received
                .into_iter()
                .map(|(r, n)| (reason_label(&r), n))
                .collect(),
            flags_given: standing.flags_given,
            flags_upheld: standing.flags_upheld,
            badges,
            notes,
            activity,
            log,
        }
        .render()?,
    ))
}

#[derive(Deserialize)]
pub struct UserActionForm {
    csrf: String,
    action: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    link: String,
    #[serde(default)]
    badge: Option<i64>,
}

pub async fn mod_user_action(
    State(st): State<AppState>,
    viewer: Viewer,
    Path(handle): Path<String>,
    HtmlForm(f): HtmlForm<UserActionForm>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    let back = format!("/mod/u/{handle}");
    let me = staff(&st, &viewer, Need::Moderator, &back).await?;
    let u = st
        .store
        .user_by_handle(&handle)
        .await?
        .ok_or(AppError::NotFound)?;
    let now = (st.clock)();
    let (by, reason) = (me.by(), f.reason.as_str());
    let r = match f.action.as_str() {
        "ban" => {
            st.store
                .set_banned(u.id, true, by, me.is_admin(), reason, now)
                .await?
        }
        "unban" => {
            st.store
                .set_banned(u.id, false, by, me.is_admin(), reason, now)
                .await?
        }
        "note" => st.store.add_note(u.id, me.user.id, &f.note, now).await?,
        "badge" => {
            let link = Some(f.link.trim()).filter(|l| !l.is_empty());
            st.store
                .grant_badge(u.id, &f.label, link, by, reason, now)
                .await?
        }
        "unbadge" => {
            let id = f.badge.ok_or(AppError::NotFound)?;
            let owned = st.store.badges(&[u.id]).await?.iter().any(|b| b.id == id);
            if !owned {
                return Err(AppError::NotFound);
            }
            st.store.remove_badge(id, by, reason, now).await?
        }
        _ => return Err(AppError::NotFound),
    };
    outcome(r, &back)
}

// ---------- public log ----------

const LOG_PAGE: i64 = 50;

#[derive(Template)]
#[template(path = "moderations.html")]
struct LogPage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    rows: Vec<LogView>,
    page: i64,
    more: bool,
}

pub async fn moderations(
    State(st): State<AppState>,
    viewer: Viewer,
    Query(q): Query<DoneQuery>,
) -> Page {
    let now = (st.clock)();
    let page = q.page.unwrap_or(1).max(1);
    let mut rows = st
        .store
        .mod_log(LOG_PAGE + 1, (page - 1) * LOG_PAGE)
        .await?;
    let more = rows.len() as i64 > LOG_PAGE;
    rows.truncate(LOG_PAGE as usize);
    Ok(Html(
        LogPage {
            site: SITE_NAME,
            viewer,
            nav: "",
            rows: rows.into_iter().map(|r| log_view(r, now)).collect(),
            page,
            more,
        }
        .render()?,
    ))
}

#[derive(Template)]
#[template(path = "moderation_stats.html")]
struct StatsPage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    stats: threaded_store::TriageStats,
    holds_rate: Option<String>,
    agreement: Option<String>,
}

pub async fn moderation_stats(State(st): State<AppState>, viewer: Viewer) -> Page {
    let stats = st.store.triage_stats((st.clock)()).await?;
    let pct = |n: i64, d: i64| (d > 0).then(|| format!("{:.0}%", n as f64 * 100.0 / d as f64));
    Ok(Html(
        StatsPage {
            site: SITE_NAME,
            viewer,
            nav: "",
            holds_rate: pct(stats.holds_confirmed, stats.holds_decided),
            agreement: pct(stats.suggestions_agreed, stats.suggestions_decided),
            stats,
        }
        .render()?,
    ))
}
