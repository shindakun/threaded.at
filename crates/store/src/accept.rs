//! The one way into the index for submissions and comments.
//!
//! The web form and, from M4, hydrant both call `accept_story` and
//! `accept_comment`, so a record written straight to a PDS gets the same
//! checks and the same triage as one written through Threaded. The web also
//! calls the `check_` functions before it writes to the PDS, so a refused
//! form never reaches the author's repo.

use threaded_core::level::{self, Capability, Level};
use threaded_core::link::{self, Link};
use threaded_core::policy::Policy;
use threaded_core::tag;

use crate::m2::insert_comment_tx;
use crate::{insert_story_tx, CommentRow, NewComment, NewStory, Result, Store, StoryRow, UserRow};

pub const TITLE_MAX: usize = 150;
/// The lexicons cap text at 5,000 graphemes and 20,000 bytes.
pub const TEXT_MAX_CHARS: usize = 5_000;
pub const TEXT_MAX_BYTES: usize = 20_000;

/// Where an item came from. The web refuses a bad tag so the author can fix
/// it; a record from the network has unusable tags dropped instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Web,
    Network,
}

/// What every check needs besides the item: the rules in force, where the
/// item came from, whether triage is on, and the time.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    pub policy: &'a Policy,
    pub source: Source,
    pub triage: bool,
    pub now: i64,
}

#[derive(Debug, Clone)]
pub struct StoryInput<'a> {
    pub title: &'a str,
    /// Empty for a text post.
    pub url: &'a str,
    pub text: &'a str,
    pub tags: &'a [String],
    pub authored: bool,
}

/// A story that passed the checks, trimmed and with its link parsed.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckedStory {
    pub title: String,
    pub link: Option<Link>,
    pub text: String,
    pub tags: Vec<String>,
    pub authored: bool,
}

#[derive(Debug, Clone)]
pub struct CommentInput<'a> {
    pub story: &'a StoryRow,
    pub parent: Option<&'a CommentRow>,
    pub text: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    pub id: i64,
    pub short_id: String,
    /// The record was already indexed, usually Threaded's own write coming
    /// back from the network. Nothing was inserted.
    pub existing: bool,
}

/// Why an item was not indexed, worded for its author.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("Your account is banned from posting.")]
    Banned,
    #[error("Your account cannot submit stories.")]
    CannotSubmit,
    #[error("Your account cannot comment.")]
    CannotComment,
    #[error("You can submit {0} stories a day. Try again tomorrow.")]
    StoryLimit(i64),
    #[error("You can post {0} comments an hour. Try again later.")]
    CommentLimit(i64),
    #[error("A title is 1 to {TITLE_MAX} characters.")]
    Title,
    #[error("Add a link, some text, or both.")]
    Empty,
    #[error("Write something first.")]
    NoText,
    #[error("Text is limited to {TEXT_MAX_CHARS} characters.")]
    TooLong,
    #[error("The link is not usable: {0}.")]
    Link(String),
    #[error("Pick at least one tag.")]
    NoTags,
    #[error("Pick at most {} tags.", tag::MAX_PER_STORY)]
    TooManyTags,
    #[error("There is no tag \u{201c}{0}\u{201d}.")]
    UnknownTag(String),
    #[error("Only moderators can use \u{201c}{0}\u{201d}.")]
    Privileged(String),
    #[error("New accounts cannot use \u{201c}{0}\u{201d} yet.")]
    Restricted(String),
    #[error("A \u{201c}show\u{201d} story is something you made: tick \u{201c}I wrote or made this\u{201d}.")]
    ShowNeedsAuthored,
    #[error("An \u{201c}ask\u{201d} story is text only: remove the link.")]
    AskIsText,
    /// The short ID of the story that already has this link.
    #[error("That link was posted recently.")]
    Duplicate(String),
    #[error("This story is closed to new comments.")]
    Locked,
    #[error("That comment cannot take replies.")]
    ParentClosed,
}

fn text_too_long(text: &str) -> bool {
    text.chars().count() > TEXT_MAX_CHARS || text.len() > TEXT_MAX_BYTES
}

/// The title and text rules every story meets, also on edit.
pub fn check_story_fields(title: &str, url: &str, text: &str) -> std::result::Result<(), Refusal> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > TITLE_MAX {
        return Err(Refusal::Title);
    }
    if url.trim().is_empty() && text.trim().is_empty() {
        return Err(Refusal::Empty);
    }
    if text_too_long(text) {
        return Err(Refusal::TooLong);
    }
    Ok(())
}

/// Whether a story takes no new comments, votes, or flags.
pub fn is_locked(story: &StoryRow, policy: &Policy, now: i64) -> bool {
    story.locked || now - story.created_at > policy.lock_days * 86_400
}

impl Store {
    /// Applies the tag rules. The web refuses a bad tag; the network drops it.
    pub async fn check_tags(
        &self,
        user: &UserRow,
        names: &[String],
        cx: &Context<'_>,
    ) -> Result<std::result::Result<Vec<String>, Refusal>> {
        let (policy, source, now) = (cx.policy, cx.source, cx.now);
        let account = user.account(now);
        let all = self.all_tags().await?;
        let mut picked: Vec<String> = Vec::new();
        for n in names {
            if picked.contains(n) {
                continue;
            }
            let problem = match all.iter().find(|t| &t.name == n && t.active) {
                None => Some(Refusal::UnknownTag(n.clone())),
                Some(t)
                    if t.privileged
                        && !level::can(
                            &account,
                            Capability::UsePrivilegedTag,
                            &policy.thresholds,
                        ) =>
                {
                    Some(Refusal::Privileged(n.clone()))
                }
                Some(t)
                    if !t.new_users_ok
                        && !level::can(
                            &account,
                            Capability::UseRestrictedTag,
                            &policy.thresholds,
                        ) =>
                {
                    Some(Refusal::Restricted(n.clone()))
                }
                Some(_) => None,
            };
            match (problem, source) {
                (Some(r), Source::Web) => return Ok(Err(r)),
                (Some(_), Source::Network) => {}
                (None, _) => picked.push(n.clone()),
            }
        }
        if picked.is_empty() {
            return Ok(Err(Refusal::NoTags));
        }
        if picked.len() > tag::MAX_PER_STORY {
            return Ok(Err(Refusal::TooManyTags));
        }
        Ok(Ok(picked))
    }

    /// Every check a new story must pass, without writing anything.
    pub async fn check_story(
        &self,
        user: &UserRow,
        input: &StoryInput<'_>,
        cx: &Context<'_>,
    ) -> Result<std::result::Result<CheckedStory, Refusal>> {
        let (policy, now) = (cx.policy, cx.now);
        let account = user.account(now);
        if account.banned {
            return Ok(Err(Refusal::Banned));
        }
        if !level::can(&account, Capability::Submit, &policy.thresholds) {
            return Ok(Err(Refusal::CannotSubmit));
        }
        let (per_day, _) = policy.limits(level::level(&account, &policy.thresholds));
        if self.stories_since(user.id, now - 86_400).await? >= per_day {
            return Ok(Err(Refusal::StoryLimit(per_day)));
        }
        if let Err(r) = check_story_fields(input.title, input.url, input.text) {
            return Ok(Err(r));
        }
        let link = if input.url.trim().is_empty() {
            None
        } else {
            match link::parse(input.url) {
                Ok(l) => Some(l),
                Err(e) => return Ok(Err(Refusal::Link(e.to_string()))),
            }
        };
        if let Some(l) = &link {
            if let Some(existing) = self
                .duplicate_of(&l.canonical, now - policy.duplicate_days * 86_400)
                .await?
            {
                return Ok(Err(Refusal::Duplicate(existing)));
            }
        }
        let tags = match self.check_tags(user, input.tags, cx).await? {
            Ok(t) => t,
            Err(r) => return Ok(Err(r)),
        };
        if tags.iter().any(|t| t == "show") && !input.authored {
            return Ok(Err(Refusal::ShowNeedsAuthored));
        }
        if tags.iter().any(|t| t == "ask") && link.is_some() {
            return Ok(Err(Refusal::AskIsText));
        }
        Ok(Ok(CheckedStory {
            title: input.title.trim().to_string(),
            link,
            text: input.text.trim().to_string(),
            tags,
            authored: input.authored,
        }))
    }

    async fn indexed(&self, table: &str, at_uri: &str) -> Result<Option<(i64, String)>> {
        sqlx::query_as(&format!(
            "SELECT id, short_id FROM {table} WHERE at_uri = ?"
        ))
        .bind(at_uri)
        .fetch_optional(self.pool())
        .await
    }

    /// Checks a story and indexes it with its record, queueing it for triage
    /// when `triage` is on. A record already indexed is returned as it is.
    pub async fn accept_story(
        &self,
        user: &UserRow,
        input: &StoryInput<'_>,
        record: (&str, &str),
        cx: &Context<'_>,
    ) -> Result<std::result::Result<Accepted, Refusal>> {
        let (triage, now) = (cx.triage, cx.now);
        if let Some((id, short_id)) = self.indexed("stories", record.0).await? {
            return Ok(Ok(Accepted {
                id,
                short_id,
                existing: true,
            }));
        }
        let s = match self.check_story(user, input, cx).await? {
            Ok(s) => s,
            Err(r) => return Ok(Err(r)),
        };
        let tag_refs: Vec<&str> = s.tags.iter().map(String::as_str).collect();
        let mut tx = self.pool().begin().await?;
        let (id, short_id) = insert_story_tx(
            &mut tx,
            &NewStory {
                user_id: user.id,
                title: &s.title,
                link: s.link.as_ref(),
                body: &s.text,
                user_is_author: s.authored,
                tags: &tag_refs,
                created_at: now,
                record: Some(record),
            },
        )
        .await?;
        if triage {
            sqlx::query("INSERT INTO triage_queue (story_id, created_at) VALUES (?, ?)")
                .bind(id)
                .bind(now)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(Ok(Accepted {
            id,
            short_id,
            existing: false,
        }))
    }

    /// Every check a new comment must pass, without writing anything.
    pub async fn check_comment(
        &self,
        user: &UserRow,
        input: &CommentInput<'_>,
        cx: &Context<'_>,
    ) -> Result<std::result::Result<(), Refusal>> {
        let (policy, now) = (cx.policy, cx.now);
        let account = user.account(now);
        if account.banned {
            return Ok(Err(Refusal::Banned));
        }
        if !level::can(&account, Capability::Comment, &policy.thresholds) {
            return Ok(Err(Refusal::CannotComment));
        }
        if is_locked(input.story, policy, now) {
            return Ok(Err(Refusal::Locked));
        }
        let level: Level = level::level(&account, &policy.thresholds);
        let (_, per_hour) = policy.limits(level);
        if self.comments_since(user.id, now - 3_600).await? >= per_hour {
            return Ok(Err(Refusal::CommentLimit(per_hour)));
        }
        if input.text.trim().is_empty() {
            return Ok(Err(Refusal::NoText));
        }
        if text_too_long(input.text) {
            return Ok(Err(Refusal::TooLong));
        }
        if let Some(p) = input.parent {
            if p.story_id != input.story.id || p.deleted_at.is_some() {
                return Ok(Err(Refusal::ParentClosed));
            }
        }
        Ok(Ok(()))
    }

    /// Checks a comment and indexes it with its record, queueing it for
    /// triage when `triage` is on. A record already indexed is returned as it is.
    pub async fn accept_comment(
        &self,
        user: &UserRow,
        input: &CommentInput<'_>,
        record: (&str, &str),
        cx: &Context<'_>,
    ) -> Result<std::result::Result<Accepted, Refusal>> {
        let (triage, now) = (cx.triage, cx.now);
        if let Some((id, short_id)) = self.indexed("comments", record.0).await? {
            return Ok(Ok(Accepted {
                id,
                short_id,
                existing: true,
            }));
        }
        if let Err(r) = self.check_comment(user, input, cx).await? {
            return Ok(Err(r));
        }
        let mut tx = self.pool().begin().await?;
        let (id, short_id) = insert_comment_tx(
            &mut tx,
            &NewComment {
                story_id: input.story.id,
                user_id: user.id,
                parent_id: input.parent.map(|p| p.id),
                body: input.text.trim(),
                created_at: now,
                record: Some(record),
            },
        )
        .await?;
        if triage {
            sqlx::query("INSERT INTO triage_queue (comment_id, created_at) VALUES (?, ?)")
                .bind(id)
                .bind(now)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(Ok(Accepted {
            id,
            short_id,
            existing: false,
        }))
    }
}
