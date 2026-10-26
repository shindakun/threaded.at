//! Admin pages under `/admin`: health, tags, people, and settings.

use askama::Template;
use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum_extra::extract::Form as HtmlForm;
use serde::Deserialize;
use std::collections::HashMap;
use threaded_core::policy::SETTINGS;
use threaded_store::{ModError, RoleChange, RoleName, TagFields, TagRow, TriageStats};

use crate::auth::{check_csrf, Viewer};
use crate::staff::{outcome, staff, DoneQuery, Need};
use crate::{AppError, AppState, Page, SITE_NAME};

// ---------- health ----------

#[derive(Template)]
#[template(path = "admin.html")]
struct HomePage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    done: bool,
    triage: &'static str,
    key: bool,
    flagged: usize,
    holds: usize,
    suggestions: usize,
    stats: TriageStats,
    precision: Option<String>,
    floor: String,
    breaker_open: bool,
}

pub async fn home(State(st): State<AppState>, viewer: Viewer, Query(q): Query<DoneQuery>) -> Page {
    staff(&st, &viewer, Need::Admin, "/admin").await?;
    let policy = st.policy();
    let flagged = st.store.flagged(policy.flags_to_queue).await?.len();
    let stats = st.store.triage_stats((st.clock)()).await?;
    let triage = match (st.triage_key, st.triage_on()) {
        (false, _) => "Off: no API key is configured. Moderation runs on flags and moderators.",
        (true, false) => {
            "Waiting: an API key is configured; save the site rules to turn triage on."
        }
        (true, true) => "On.",
    };
    Ok(Html(
        HomePage {
            site: SITE_NAME,
            viewer,
            nav: "admin",
            done: q.done.is_some(),
            triage,
            key: st.triage_key,
            flagged,
            holds: st.store.holds().await?.len(),
            suggestions: st.store.triage_suggestions().await?.len(),
            precision: stats.hold_precision().map(|p| format!("{:.0}%", p * 100.0)),
            floor: format!("{:.0}%", policy.hold_precision_floor * 100.0),
            breaker_open: stats.breaker_open(policy.hold_precision_floor),
            stats,
        }
        .render()?,
    ))
}

#[derive(Deserialize)]
pub struct ReasonForm {
    csrf: String,
    #[serde(default)]
    reason: String,
}

pub async fn breaker_reset(
    State(st): State<AppState>,
    viewer: Viewer,
    axum::Form(f): axum::Form<ReasonForm>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    let me = staff(&st, &viewer, Need::Admin, "/admin").await?;
    let r = st
        .store
        .reset_breaker(me.by(), &f.reason, (st.clock)())
        .await?;
    outcome(r, "/admin")
}

// ---------- rules ----------

pub struct RulesRow {
    pub id: i64,
    pub author: String,
    pub age: String,
    pub body: String,
}

#[derive(Template)]
#[template(path = "admin_rules.html")]
struct RulesPage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    done: bool,
    current: String,
    history: Vec<RulesRow>,
}

pub async fn rules_page(
    State(st): State<AppState>,
    viewer: Viewer,
    Query(q): Query<DoneQuery>,
) -> Page {
    staff(&st, &viewer, Need::Admin, "/admin/rules").await?;
    let now = (st.clock)();
    let history: Vec<RulesRow> = st
        .store
        .rules_history()
        .await?
        .into_iter()
        .map(|r| RulesRow {
            id: r.id,
            author: r.author.unwrap_or_else(|| "console".into()),
            age: crate::ago(now - r.created_at),
            body: r.body,
        })
        .collect();
    Ok(Html(
        RulesPage {
            site: SITE_NAME,
            viewer,
            nav: "admin",
            done: q.done.is_some(),
            current: history.first().map(|r| r.body.clone()).unwrap_or_default(),
            history,
        }
        .render()?,
    ))
}

#[derive(Deserialize)]
pub struct RulesForm {
    csrf: String,
    body: String,
    #[serde(default)]
    reason: String,
}

pub async fn rules_save(
    State(st): State<AppState>,
    viewer: Viewer,
    axum::Form(f): axum::Form<RulesForm>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    let me = staff(&st, &viewer, Need::Admin, "/admin/rules").await?;
    let r = st
        .store
        .save_rules(&f.body, me.by(), &f.reason, (st.clock)())
        .await?;
    if r.is_ok() {
        st.rules_ready
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    outcome(r, "/admin/rules")
}

// ---------- settings ----------

pub struct SettingRow {
    pub key: &'static str,
    pub label: &'static str,
    pub value: String,
}

#[derive(Template)]
#[template(path = "admin_settings.html")]
struct SettingsPage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    done: bool,
    rows: Vec<SettingRow>,
}

pub async fn settings_page(
    State(st): State<AppState>,
    viewer: Viewer,
    Query(q): Query<DoneQuery>,
) -> Page {
    staff(&st, &viewer, Need::Admin, "/admin/settings").await?;
    let p = st.policy();
    let rows = SETTINGS
        .iter()
        .map(|s| SettingRow {
            key: s.key,
            label: s.label,
            value: p.get(s.key).unwrap_or_default(),
        })
        .collect();
    Ok(Html(
        SettingsPage {
            site: SITE_NAME,
            viewer,
            nav: "admin",
            done: q.done.is_some(),
            rows,
        }
        .render()?,
    ))
}

pub async fn settings_save(
    State(st): State<AppState>,
    viewer: Viewer,
    HtmlForm(f): HtmlForm<HashMap<String, String>>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, f.get("csrf").map(String::as_str).unwrap_or(""))?;
    let me = staff(&st, &viewer, Need::Admin, "/admin/settings").await?;
    let reason = f.get("reason").map(String::as_str).unwrap_or("");
    let current = st.policy();
    let mut next = current.clone();
    let mut changes = Vec::new();
    for s in SETTINGS {
        let Some(v) = f.get(s.key) else { continue };
        let old = current.get(s.key).unwrap_or_default();
        if v.trim() == old {
            continue;
        }
        next.set(s.key, v).map_err(AppError::Refused)?;
        changes.push((s.key, old, next.get(s.key).unwrap_or_default()));
    }
    if changes.is_empty() {
        return Err(AppError::Refused("Nothing changed.".into()));
    }
    let r = st
        .store
        .save_settings(&changes, me.by(), reason, (st.clock)())
        .await?;
    if r.is_ok() {
        st.set_policy(next);
    }
    outcome(r, "/admin/settings")
}

// ---------- tags ----------

#[derive(Template)]
#[template(path = "admin_tags.html")]
struct TagsPage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    done: bool,
    tags: Vec<TagRow>,
}

pub async fn tags_page(
    State(st): State<AppState>,
    viewer: Viewer,
    Query(q): Query<DoneQuery>,
) -> Page {
    staff(&st, &viewer, Need::Admin, "/admin/tags").await?;
    Ok(Html(
        TagsPage {
            site: SITE_NAME,
            viewer,
            nav: "admin",
            done: q.done.is_some(),
            tags: st.store.all_tags().await?,
        }
        .render()?,
    ))
}

#[derive(Deserialize)]
pub struct TagForm {
    csrf: String,
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    rank_boost: String,
    #[serde(default)]
    privileged: Option<String>,
    #[serde(default)]
    new_users_ok: Option<String>,
    #[serde(default)]
    is_media: Option<String>,
    #[serde(default)]
    active: Option<String>,
    #[serde(default)]
    reason: String,
}

pub async fn tags_save(
    State(st): State<AppState>,
    viewer: Viewer,
    HtmlForm(f): HtmlForm<TagForm>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    let me = staff(&st, &viewer, Need::Admin, "/admin/tags").await?;
    let boost = if f.rank_boost.trim().is_empty() {
        0.0
    } else {
        f.rank_boost
            .trim()
            .parse::<f64>()
            .map_err(|_| AppError::Refused("A rank boost is a number from -10 to 10.".into()))?
    };
    let r = st
        .store
        .save_tag(
            f.name.trim(),
            &TagFields {
                description: &f.description,
                category: &f.category,
                rank_boost: boost,
                privileged: f.privileged.is_some(),
                new_users_ok: f.new_users_ok.is_some(),
                is_media: f.is_media.is_some(),
                active: f.active.is_some(),
            },
            me.by(),
            &f.reason,
            (st.clock)(),
        )
        .await?;
    outcome(r, "/admin/tags")
}

// ---------- people ----------

pub struct PersonRow {
    pub handle: String,
    pub did: String,
    pub role: String,
    pub is_me: bool,
}

#[derive(Template)]
#[template(path = "admin_people.html")]
struct PeoplePage {
    site: &'static str,
    viewer: Viewer,
    nav: &'static str,
    done: bool,
    people: Vec<PersonRow>,
    badges: Vec<(String, threaded_store::Badge)>,
}

pub async fn people_page(
    State(st): State<AppState>,
    viewer: Viewer,
    Query(q): Query<DoneQuery>,
) -> Page {
    let me = staff(&st, &viewer, Need::Admin, "/admin/people").await?;
    let people = st
        .store
        .staff_members()
        .await?
        .into_iter()
        .map(|u| PersonRow {
            is_me: u.id == me.user.id,
            handle: u.handle,
            did: u.did,
            role: u.role,
        })
        .collect();
    Ok(Html(
        PeoplePage {
            site: SITE_NAME,
            viewer,
            nav: "admin",
            done: q.done.is_some(),
            people,
            badges: st.store.badges_in_use().await?,
        }
        .render()?,
    ))
}

#[derive(Deserialize)]
pub struct RoleForm {
    csrf: String,
    did: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    reason: String,
}

pub async fn people_save(
    State(st): State<AppState>,
    viewer: Viewer,
    HtmlForm(f): HtmlForm<RoleForm>,
) -> Result<Response, AppError> {
    check_csrf(&viewer, &f.csrf)?;
    let me = staff(&st, &viewer, Need::Admin, "/admin/people").await?;
    let did = f.did.trim();
    if !did.starts_with("did:") {
        return Err(AppError::Refused(
            "Roles take a DID, such as did:plc:..., not a handle.".into(),
        ));
    }
    if did == me.user.did {
        return Err(AppError::Refused("You cannot change your own role.".into()));
    }
    let role = match f.role.as_str() {
        "moderator" => RoleName::Moderator,
        "admin" => RoleName::Admin,
        "user" => RoleName::User,
        _ => return Err(AppError::Refused("Pick a role.".into())),
    };
    if f.reason.trim().is_empty() {
        return Err(AppError::Refused(ModError::NoReason.to_string()));
    }
    let r = match st
        .store
        .set_role(did, role, me.by(), f.reason.trim(), (st.clock)())
        .await?
    {
        RoleChange::NoSuchUser => Err(ModError::Refused(
            "Nobody with that DID has signed in yet. They sign in once, then you grant the role.",
        )),
        RoleChange::Unchanged => Err(ModError::Refused("They already hold that role.")),
        RoleChange::Changed { .. } => Ok(()),
    };
    if r.is_ok() {
        return Ok(Redirect::to("/admin/people?done=1").into_response());
    }
    outcome(r, "/admin/people")
}
