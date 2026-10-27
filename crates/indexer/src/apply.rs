//! Applies one record event from hydrant to the index: the network's version
//! of submitting, commenting, voting, editing, and deleting. New items go
//! through `accept`, the same checks as the web form.
//!
//! Applying an event twice changes nothing, so a replay after a crash is safe.

use serde::Deserialize;
use threaded_core::level::{self, Capability};
use threaded_core::link;
use threaded_core::policy::Policy;
use threaded_store::{
    is_locked, CommentInput, CommentRow, Context, Source, Store, StoryEdit, StoryInput, StoryRow,
    Target, VoteChange,
};

use crate::client::{Action, RecordEvent};

/// The record collections Threaded reads: `at.threaded.*` in production,
/// `at.threaded.dev.*` in local builds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collections {
    pub submission: String,
    pub comment: String,
    pub vote: String,
}

impl Collections {
    /// Collections under a namespace prefix, such as `at.threaded`.
    pub fn under(prefix: &str) -> Collections {
        Collections {
            submission: format!("{prefix}.submission"),
            comment: format!("{prefix}.comment"),
            vote: format!("{prefix}.vote"),
        }
    }
}

/// What applying needs besides the event.
#[derive(Debug, Clone, Copy)]
pub struct Settings<'a> {
    pub collections: &'a Collections,
    pub policy: &'a Policy,
    /// AI triage is on; live items are queued for it.
    pub triage: bool,
    /// When the event arrived.
    pub now: i64,
}

/// What applying an event did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    Indexed {
        short_id: String,
    },
    /// Already indexed, usually Threaded's own write coming back; or an
    /// update whose CID is the one stored.
    Known,
    /// The item failed a check, or the record is malformed; not indexed.
    Refused(String),
    /// A comment or vote whose subject is not indexed yet.
    Pending {
        waits_for: String,
    },
    Edited,
    /// An edit changed the link after the edit window; the story is hidden.
    Detached,
    Deleted,
    Voted,
    Unvoted,
    Ignored(&'static str),
}

#[derive(Deserialize)]
struct StrongRef {
    uri: String,
}

#[derive(Deserialize)]
struct Submission {
    title: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    authored: bool,
    #[serde(rename = "createdAt", default)]
    created_at: String,
}

#[derive(Deserialize)]
struct Comment {
    subject: StrongRef,
    #[serde(default)]
    parent: Option<StrongRef>,
    text: String,
    #[serde(rename = "createdAt", default)]
    created_at: String,
}

#[derive(Deserialize)]
struct Vote {
    subject: StrongRef,
}

/// The record body as `T`, after checking that its `$type` is the collection.
fn body<T: serde::de::DeserializeOwned>(ev: &RecordEvent) -> Result<T, String> {
    let Some(rec) = &ev.record else {
        return Err("the event has no record".into());
    };
    if rec.get("$type").and_then(|t| t.as_str()) != Some(ev.collection.as_str()) {
        return Err("the record's $type is not its collection".into());
    }
    serde_json::from_value(rec.clone()).map_err(|e| format!("the record is malformed: {e}"))
}

/// The time an item counts from. A live event's is when it arrived: a
/// record's `createdAt` is chosen by its author, so trusting it would let
/// them dodge rate limits or the front page. A backfilled record keeps its
/// `createdAt`, never later than now.
pub fn item_time(live: bool, created_at: &str, now: i64) -> i64 {
    if live {
        return now;
    }
    time::OffsetDateTime::parse(created_at, &time::format_description::well_known::Rfc3339)
        .map(|t| t.unix_timestamp().min(now))
        .unwrap_or(now)
}

pub async fn apply(
    store: &Store,
    s: &Settings<'_>,
    ev: &RecordEvent,
) -> Result<Applied, threaded_store::Error> {
    let c = s.collections;
    let kind = if ev.collection == c.submission {
        Kind::Submission
    } else if ev.collection == c.comment {
        Kind::Comment
    } else if ev.collection == c.vote {
        Kind::Vote
    } else {
        return Ok(Applied::Ignored("not a Threaded collection"));
    };
    match (kind, ev.action) {
        (Kind::Submission, Action::Create) => create_story(store, s, ev).await,
        (Kind::Submission, Action::Update) => update_story(store, s, ev).await,
        (Kind::Submission, Action::Delete) => match store.story_by_uri(&ev.uri()).await? {
            Some(story) => {
                store.delete_story(story.id, story.user_id, s.now).await?;
                Ok(Applied::Deleted)
            }
            None => Ok(Applied::Ignored("the story is not indexed")),
        },
        (Kind::Comment, Action::Create) => create_comment(store, s, ev).await,
        (Kind::Comment, Action::Update) => update_comment(store, s, ev).await,
        (Kind::Comment, Action::Delete) => match store.comment_by_uri(&ev.uri()).await? {
            Some(c) if c.deleted_at.is_none() => {
                store.delete_comment(&c, s.now).await?;
                Ok(Applied::Deleted)
            }
            _ => Ok(Applied::Ignored("the comment is not indexed")),
        },
        (Kind::Vote, Action::Create) => create_vote(store, s, ev).await,
        (Kind::Vote, Action::Update) => Ok(Applied::Ignored("votes are not edited")),
        (Kind::Vote, Action::Delete) => match store.vote_by_uri(&ev.uri()).await? {
            Some(v) if v.value == 1 => match v.target() {
                Some(t) => {
                    store
                        .set_vote(v.user_id, t, VoteChange::Clear, s.now)
                        .await?;
                    Ok(Applied::Unvoted)
                }
                None => Ok(Applied::Ignored("the vote has no subject")),
            },
            _ => Ok(Applied::Ignored("the vote is not indexed")),
        },
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Submission,
    Comment,
    Vote,
}

fn context<'a>(s: &Settings<'a>, live: bool, at: i64) -> Context<'a> {
    Context {
        policy: s.policy,
        source: Source::Network,
        triage: s.triage && live,
        now: at,
    }
}

fn record_ref(ev: &RecordEvent) -> Option<(String, String)> {
    Some((ev.uri(), ev.cid.clone()?))
}

async fn create_story(
    store: &Store,
    s: &Settings<'_>,
    ev: &RecordEvent,
) -> Result<Applied, threaded_store::Error> {
    let r: Submission = match body(ev) {
        Ok(r) => r,
        Err(e) => return Ok(Applied::Refused(e)),
    };
    let Some((uri, cid)) = record_ref(ev) else {
        return Ok(Applied::Refused("the event has no CID".into()));
    };
    let at = item_time(ev.live, &r.created_at, s.now);
    let user = store.network_user(&ev.did, s.now).await?;
    let url = r.url.unwrap_or_default();
    let text = r.text.unwrap_or_default();
    let input = StoryInput {
        title: &r.title,
        url: &url,
        text: &text,
        tags: &r.tags,
        authored: r.authored,
    };
    match store
        .accept_story(&user, &input, (&uri, &cid), &context(s, ev.live, at))
        .await?
    {
        Ok(a) if a.existing => Ok(Applied::Known),
        Ok(a) => Ok(Applied::Indexed {
            short_id: a.short_id,
        }),
        Err(refusal) => Ok(Applied::Refused(refusal.to_string())),
    }
}

/// The story an AT-URI names; a merged story stands for the one it merged into.
async fn subject_story(
    store: &Store,
    uri: &str,
) -> Result<Option<StoryRow>, threaded_store::Error> {
    let Some(story) = store.story_by_uri(uri).await? else {
        return Ok(None);
    };
    match &story.merged_into {
        Some(target) => store.story_any(target).await,
        None => Ok(Some(story)),
    }
}

async fn create_comment(
    store: &Store,
    s: &Settings<'_>,
    ev: &RecordEvent,
) -> Result<Applied, threaded_store::Error> {
    if store.comment_by_uri(&ev.uri()).await?.is_some() {
        return Ok(Applied::Known);
    }
    let r: Comment = match body(ev) {
        Ok(r) => r,
        Err(e) => return Ok(Applied::Refused(e)),
    };
    let Some((uri, cid)) = record_ref(ev) else {
        return Ok(Applied::Refused("the event has no CID".into()));
    };
    let Some(story) = subject_story(store, &r.subject.uri).await? else {
        return Ok(Applied::Pending {
            waits_for: r.subject.uri,
        });
    };
    let parent: Option<CommentRow> = match &r.parent {
        None => None,
        Some(p) => match store.comment_by_uri(&p.uri).await? {
            Some(c) => Some(c),
            None => {
                return Ok(Applied::Pending {
                    waits_for: p.uri.clone(),
                })
            }
        },
    };
    let at = item_time(ev.live, &r.created_at, s.now);
    let user = store.network_user(&ev.did, s.now).await?;
    let input = CommentInput {
        story: &story,
        parent: parent.as_ref(),
        text: &r.text,
    };
    match store
        .accept_comment(&user, &input, (&uri, &cid), &context(s, ev.live, at))
        .await?
    {
        Ok(a) if a.existing => Ok(Applied::Known),
        Ok(a) => Ok(Applied::Indexed {
            short_id: a.short_id,
        }),
        Err(refusal) => Ok(Applied::Refused(refusal.to_string())),
    }
}

async fn create_vote(
    store: &Store,
    s: &Settings<'_>,
    ev: &RecordEvent,
) -> Result<Applied, threaded_store::Error> {
    let uri = ev.uri();
    if store.vote_by_uri(&uri).await?.is_some() {
        return Ok(Applied::Known);
    }
    let r: Vote = match body(ev) {
        Ok(r) => r,
        Err(e) => return Ok(Applied::Refused(e)),
    };
    // Votes count only from accounts that have signed in here.
    let Some(user) = store.user_by_did(&ev.did).await? else {
        return Ok(Applied::Ignored("the voter has not signed in here"));
    };
    if !user.signed_in() {
        return Ok(Applied::Ignored("the voter has not signed in here"));
    }
    let account = user.account(s.now);
    if !level::can(&account, Capability::Upvote, &s.policy.thresholds) {
        return Ok(Applied::Ignored("the voter may not upvote"));
    }
    let (target, author, story) = if let Some(st) = subject_story(store, &r.subject.uri).await? {
        (Target::Story(st.id), st.user_id, st)
    } else if let Some(c) = store.comment_by_uri(&r.subject.uri).await? {
        if c.deleted_at.is_some() {
            return Ok(Applied::Ignored("the comment is deleted"));
        }
        let Some(st) = store.story_any(&c.story_short_id).await? else {
            return Ok(Applied::Ignored("the comment's story is gone"));
        };
        (Target::Comment(c.id), c.user_id, st)
    } else {
        return Ok(Applied::Pending {
            waits_for: r.subject.uri,
        });
    };
    if author == user.id {
        return Ok(Applied::Ignored("a vote on one's own post"));
    }
    if is_locked(&story, s.policy, s.now) {
        return Ok(Applied::Ignored("the story is locked"));
    }
    // One vote per account and subject: a record for a subject the user
    // already upvoted here attaches to that vote.
    match store.vote(user.id, target).await? {
        Some(v) if v.value == 1 && v.at_uri.is_none() => {
            store.set_vote_record(v.id, Some(&uri)).await?;
            Ok(Applied::Voted)
        }
        Some(v) if v.value == 1 => Ok(Applied::Ignored("already upvoted by another record")),
        Some(_) => Ok(Applied::Ignored("the voter flagged it")),
        None => {
            store
                .set_vote(
                    user.id,
                    target,
                    VoteChange::Up { at_uri: Some(&uri) },
                    s.now,
                )
                .await?;
            Ok(Applied::Voted)
        }
    }
}

async fn update_story(
    store: &Store,
    s: &Settings<'_>,
    ev: &RecordEvent,
) -> Result<Applied, threaded_store::Error> {
    let Some(story) = store.story_by_uri(&ev.uri()).await? else {
        return Ok(Applied::Ignored("the story is not indexed"));
    };
    if ev.cid.is_some() && ev.cid == story.at_cid {
        return Ok(Applied::Known);
    }
    let r: Submission = match body(ev) {
        Ok(r) => r,
        Err(e) => return Ok(Applied::Refused(e)),
    };
    let url = r.url.unwrap_or_default();
    let text = r.text.unwrap_or_default();
    if let Err(refusal) = threaded_store::check_story_fields(&r.title, &url, &text) {
        return Ok(Applied::Refused(refusal.to_string()));
    }
    let link = if url.trim().is_empty() {
        None
    } else {
        match link::parse(&url) {
            Ok(l) => Some(l),
            Err(e) => return Ok(Applied::Refused(format!("The link is not usable: {e}."))),
        }
    };
    let user = store.network_user(&ev.did, s.now).await?;
    let tags = match store
        .check_tags(&user, &r.tags, &context(s, ev.live, s.now))
        .await?
    {
        Ok(t) => t,
        Err(refusal) => return Ok(Applied::Refused(refusal.to_string())),
    };
    let tag_refs: Vec<&str> = tags.iter().map(String::as_str).collect();
    let edit = StoryEdit {
        title: r.title.trim(),
        link: link.as_ref(),
        body: text.trim(),
        tags: &tag_refs,
        cid: ev.cid.as_deref(),
    };
    // The author's own link, which a moderator's edit may be covering.
    let authors_url = if story.mod_edited {
        store.author_url(story.id).await?
    } else {
        story.url.clone()
    };
    let new_url = link.as_ref().map(|l| l.url.clone());
    let late = s.now - story.created_at > s.policy.url_edit_secs;
    if new_url != authors_url && late {
        store.detach_story(&story, &edit, s.now).await?;
        return Ok(Applied::Detached);
    }
    store.edit_story(&story, &edit, s.now).await?;
    Ok(Applied::Edited)
}

async fn update_comment(
    store: &Store,
    s: &Settings<'_>,
    ev: &RecordEvent,
) -> Result<Applied, threaded_store::Error> {
    let Some(c) = store.comment_by_uri(&ev.uri()).await? else {
        return Ok(Applied::Ignored("the comment is not indexed"));
    };
    if c.deleted_at.is_some() {
        return Ok(Applied::Ignored("the comment is deleted"));
    }
    if ev.cid.is_some() && ev.cid == c.at_cid {
        return Ok(Applied::Known);
    }
    let r: Comment = match body(ev) {
        Ok(r) => r,
        Err(e) => return Ok(Applied::Refused(e)),
    };
    let text = r.text.trim();
    if text.is_empty() {
        return Ok(Applied::Refused(
            threaded_store::Refusal::NoText.to_string(),
        ));
    }
    if text.chars().count() > threaded_store::TEXT_MAX_CHARS
        || text.len() > threaded_store::TEXT_MAX_BYTES
    {
        return Ok(Applied::Refused(
            threaded_store::Refusal::TooLong.to_string(),
        ));
    }
    store
        .edit_comment(&c, text, ev.cid.as_deref(), s.now)
        .await?;
    Ok(Applied::Edited)
}
