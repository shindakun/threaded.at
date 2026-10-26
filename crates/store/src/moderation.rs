//! Moderation: the public log and the actions moderators and admins take.
//! Every action writes a `mod_log` row in the same transaction as the change.

use sqlx::{Sqlite, Transaction};

use crate::{Result, Store};

/// Who acted. Triage and the console have no user row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum By {
    Moderator(i64),
    Triage,
    Console,
}

impl By {
    fn actor(self) -> &'static str {
        match self {
            By::Moderator(_) => "moderator",
            By::Triage => "triage",
            By::Console => "console",
        }
    }

    fn user_id(self) -> Option<i64> {
        match self {
            By::Moderator(id) => Some(id),
            _ => None,
        }
    }
}

/// One `mod_log` row before it is written.
#[derive(Debug, Clone, Default)]
pub struct LogEntry<'a> {
    pub action: &'a str,
    pub user_id: Option<i64>,
    pub story_id: Option<i64>,
    pub comment_id: Option<i64>,
    pub tag_id: Option<i64>,
    pub reason: &'a str,
    pub detail: &'a str,
}

pub(crate) async fn log(
    tx: &mut Transaction<'_, Sqlite>,
    by: By,
    e: &LogEntry<'_>,
    now: i64,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO mod_log (moderator_id, actor, action, user_id, story_id, comment_id, tag_id, reason, detail, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(by.user_id())
    .bind(by.actor())
    .bind(e.action)
    .bind(e.user_id)
    .bind(e.story_id)
    .bind(e.comment_id)
    .bind(e.tag_id)
    .bind(e.reason)
    .bind(e.detail)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// A role a user can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleName {
    User,
    Moderator,
    Admin,
}

impl RoleName {
    pub fn as_str(self) -> &'static str {
        match self {
            RoleName::User => "user",
            RoleName::Moderator => "moderator",
            RoleName::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Option<RoleName> {
        match s {
            "user" => Some(RoleName::User),
            "moderator" => Some(RoleName::Moderator),
            "admin" => Some(RoleName::Admin),
            _ => None,
        }
    }
}

/// What `set_role` changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoleChange {
    /// No user has signed in with this DID.
    NoSuchUser,
    /// The user already held the role; nothing was logged.
    Unchanged,
    Changed {
        from: String,
    },
}

impl Store {
    /// Grants a role, or `RoleName::User` to revoke one, and logs it.
    pub async fn set_role(
        &self,
        did: &str,
        role: RoleName,
        by: By,
        reason: &str,
        now: i64,
    ) -> Result<RoleChange> {
        let mut tx = self.pool().begin().await?;
        let Some((id, from)): Option<(i64, String)> =
            sqlx::query_as("SELECT id, role FROM users WHERE did = ?")
                .bind(did)
                .fetch_optional(&mut *tx)
                .await?
        else {
            return Ok(RoleChange::NoSuchUser);
        };
        if from == role.as_str() {
            return Ok(RoleChange::Unchanged);
        }
        sqlx::query("UPDATE users SET role = ? WHERE id = ?")
            .bind(role.as_str())
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let detail = format!("{from} to {}", role.as_str());
        log(
            &mut tx,
            by,
            &LogEntry {
                action: if role == RoleName::User {
                    "revoke role"
                } else {
                    "grant role"
                },
                user_id: Some(id),
                reason,
                detail: &detail,
                ..Default::default()
            },
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(RoleChange::Changed { from })
    }
}

/// Why a moderator action did not apply.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModError {
    #[error("Not found.")]
    NotFound,
    #[error("Give a reason. It appears in the public moderation log.")]
    NoReason,
    #[error("{0}")]
    Refused(&'static str),
}

pub type Outcome = std::result::Result<(), ModError>;

/// A moderator's new title, link, and tags for a story.
#[derive(Debug, Clone)]
pub struct StoryOverride<'a> {
    pub title: &'a str,
    pub link: Option<&'a threaded_core::link::Link>,
    pub tags: &'a [String],
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Badge {
    pub id: i64,
    pub user_id: i64,
    pub label: String,
    pub link: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct NoteRow {
    pub moderator: String,
    pub note: String,
    pub created_at: i64,
}

/// A `mod_log` row with names resolved, for the public log and reviews.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LogRow {
    pub id: i64,
    pub actor: String,
    pub action: String,
    pub moderator: Option<String>,
    pub user_handle: Option<String>,
    pub story_short_id: Option<String>,
    pub story_title: Option<String>,
    pub comment_short_id: Option<String>,
    pub tag_name: Option<String>,
    pub reason: String,
    pub detail: String,
    pub created_at: i64,
}

const LOG_COLUMNS: &str = "l.id, l.actor, l.action, \
     (SELECT handle FROM users WHERE id = l.moderator_id) AS moderator, \
     (SELECT handle FROM users WHERE id = l.user_id) AS user_handle, \
     coalesce(s.short_id, cs.short_id) AS story_short_id, coalesce(s.title, cs.title) AS story_title, \
     c.short_id AS comment_short_id, \
     (SELECT name FROM tags WHERE id = l.tag_id) AS tag_name, \
     l.reason, l.detail, l.created_at";

const LOG_FROM: &str = "mod_log l LEFT JOIN stories s ON s.id = l.story_id \
     LEFT JOIN comments c ON c.id = l.comment_id LEFT JOIN stories cs ON cs.id = c.story_id";

fn reason_ok(reason: &str) -> Outcome {
    if reason.trim().is_empty() {
        Err(ModError::NoReason)
    } else {
        Ok(())
    }
}

/// Recounts a story's score, flags, and visible comments from the rows, then
/// its rank and its author's karma.
pub(crate) async fn recount(tx: &mut Transaction<'_, Sqlite>, story_id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE stories SET \
         score = 1 + (SELECT coalesce(sum(value), 0) FROM votes WHERE story_id = ?1), \
         flags = (SELECT count(*) FROM votes WHERE story_id = ?1 AND value = -1), \
         comment_count = (SELECT count(*) FROM comments WHERE story_id = ?1 \
             AND deleted_at IS NULL AND removed_at IS NULL AND held_at IS NULL), \
         last_comment_at = (SELECT max(created_at) FROM comments WHERE story_id = ?1 \
             AND deleted_at IS NULL AND removed_at IS NULL AND held_at IS NULL) \
         WHERE id = ?1",
    )
    .bind(story_id)
    .execute(&mut **tx)
    .await?;
    crate::m2::recompute_rank(tx, story_id).await?;
    let author: i64 = sqlx::query_scalar("SELECT user_id FROM stories WHERE id = ?")
        .bind(story_id)
        .fetch_one(&mut **tx)
        .await?;
    crate::m2::recompute_karma(tx, author).await
}

/// The author and story of an item, or `None` when it does not exist.
async fn item_owner(
    tx: &mut Transaction<'_, Sqlite>,
    target: crate::Target,
) -> Result<Option<(i64, i64)>> {
    match target {
        crate::Target::Story(id) => {
            sqlx::query_as("SELECT user_id, id FROM stories WHERE id = ? AND deleted_at IS NULL")
                .bind(id)
                .fetch_optional(&mut **tx)
                .await
        }
        crate::Target::Comment(id) => {
            sqlx::query_as(
                "SELECT user_id, story_id FROM comments WHERE id = ? AND deleted_at IS NULL",
            )
            .bind(id)
            .fetch_optional(&mut **tx)
            .await
        }
    }
}

fn target_ids(target: crate::Target) -> (Option<i64>, Option<i64>) {
    match target {
        crate::Target::Story(id) => (Some(id), None),
        crate::Target::Comment(id) => (None, Some(id)),
    }
}

impl Store {
    /// Hides a story or comment from everyone but its author and moderators.
    pub async fn remove(
        &self,
        target: crate::Target,
        by: By,
        reason: &str,
        now: i64,
    ) -> Result<Outcome> {
        self.set_removed(target, true, by, reason, now).await
    }

    /// Undoes a removal.
    pub async fn restore(
        &self,
        target: crate::Target,
        by: By,
        reason: &str,
        now: i64,
    ) -> Result<Outcome> {
        self.set_removed(target, false, by, reason, now).await
    }

    async fn set_removed(
        &self,
        target: crate::Target,
        removed: bool,
        by: By,
        reason: &str,
        now: i64,
    ) -> Result<Outcome> {
        if let Err(e) = reason_ok(reason) {
            return Ok(Err(e));
        }
        let mut tx = self.pool().begin().await?;
        let Some((author, story_id)) = item_owner(&mut tx, target).await? else {
            return Ok(Err(ModError::NotFound));
        };
        let (table, id) = match target {
            crate::Target::Story(id) => ("stories", id),
            crate::Target::Comment(id) => ("comments", id),
        };
        let changed = if removed {
            sqlx::query(&format!(
                "UPDATE {table} SET removed_at = ?, removed_by = ?, removed_reason = ? \
                 WHERE id = ? AND removed_at IS NULL"
            ))
            .bind(now)
            .bind(by.user_id())
            .bind(reason.trim())
            .bind(id)
            .execute(&mut *tx)
            .await?
        } else {
            sqlx::query(&format!(
                "UPDATE {table} SET removed_at = NULL, removed_by = NULL, removed_reason = NULL \
                 WHERE id = ? AND removed_at IS NOT NULL"
            ))
            .bind(id)
            .execute(&mut *tx)
            .await?
        };
        if changed.rows_affected() == 0 {
            return Ok(Err(ModError::Refused(if removed {
                "It is already removed."
            } else {
                "It is not removed."
            })));
        }
        recount(&mut tx, story_id).await?;
        let (s, c) = target_ids(target);
        let kind = if c.is_some() { "comment" } else { "story" };
        let action = format!("{} {kind}", if removed { "remove" } else { "restore" });
        log(
            &mut tx,
            by,
            &LogEntry {
                action: &action,
                user_id: Some(author),
                story_id: s,
                comment_id: c,
                reason: reason.trim(),
                ..Default::default()
            },
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(Ok(()))
    }

    /// Closes a story to new comments, votes, and flags, or reopens it.
    pub async fn set_locked(
        &self,
        story_id: i64,
        locked: bool,
        by: By,
        reason: &str,
        now: i64,
    ) -> Result<Outcome> {
        if let Err(e) = reason_ok(reason) {
            return Ok(Err(e));
        }
        let mut tx = self.pool().begin().await?;
        let Some((author, _)) = item_owner(&mut tx, crate::Target::Story(story_id)).await? else {
            return Ok(Err(ModError::NotFound));
        };
        let changed = sqlx::query(
            "UPDATE stories SET locked = ?, locked_reason = ? WHERE id = ? AND locked <> ?",
        )
        .bind(locked)
        .bind(locked.then(|| reason.trim()))
        .bind(story_id)
        .bind(locked)
        .execute(&mut *tx)
        .await?;
        if changed.rows_affected() == 0 {
            return Ok(Err(ModError::Refused(if locked {
                "It is already locked."
            } else {
                "It is not locked."
            })));
        }
        log(
            &mut tx,
            by,
            &LogEntry {
                action: if locked { "lock story" } else { "unlock story" },
                user_id: Some(author),
                story_id: Some(story_id),
                reason: reason.trim(),
                ..Default::default()
            },
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(Ok(()))
    }

    /// Replaces what readers see of a story's title, link, and tags. The
    /// author's own values are kept beside them, and the author's later edits
    /// change those, not these.
    pub async fn override_story(
        &self,
        story: &crate::StoryRow,
        o: &StoryOverride<'_>,
        by: By,
        reason: &str,
        now: i64,
    ) -> Result<Outcome> {
        if let Err(e) = reason_ok(reason) {
            return Ok(Err(e));
        }
        let mut tx = self.pool().begin().await?;
        override_story_tx(&mut tx, story, o, by, reason, now).await?;
        tx.commit().await?;
        Ok(Ok(()))
    }

    /// Returns a story to the author's title, link, and tags.
    pub async fn clear_override(
        &self,
        story: &crate::StoryRow,
        by: By,
        reason: &str,
        now: i64,
    ) -> Result<Outcome> {
        if let Err(e) = reason_ok(reason) {
            return Ok(Err(e));
        }
        if !story.mod_edited {
            return Ok(Err(ModError::Refused("No moderator edit is in force.")));
        }
        let mut tx = self.pool().begin().await?;
        let (title, url, tags): (String, Option<String>, String) = sqlx::query_as(
            "SELECT author_title, author_url, coalesce(author_tags, '') FROM stories WHERE id = ?",
        )
        .bind(story.id)
        .fetch_one(&mut *tx)
        .await?;
        let link = url
            .as_deref()
            .and_then(|u| threaded_core::link::parse(u).ok());
        sqlx::query(
            "UPDATE stories SET title = ?, url = ?, canonical_url = ?, domain = ?, \
             author_title = NULL, author_url = NULL, author_tags = NULL, \
             override_by = NULL, override_at = NULL WHERE id = ?",
        )
        .bind(&title)
        .bind(link.as_ref().map(|l| l.url.as_str()))
        .bind(link.as_ref().map(|l| l.canonical.as_str()))
        .bind(link.as_ref().map(|l| l.domain.as_str()))
        .bind(story.id)
        .execute(&mut *tx)
        .await?;
        set_taggings(&mut tx, story.id, tags.split_whitespace()).await?;
        crate::m2::recompute_rank(&mut tx, story.id).await?;
        log(
            &mut tx,
            by,
            &LogEntry {
                action: "clear story edit",
                user_id: Some(story.user_id),
                story_id: Some(story.id),
                reason: reason.trim(),
                ..Default::default()
            },
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(Ok(()))
    }

    /// Merges story `from` into story `into`: comments and votes move, and
    /// `from` redirects. A user who voted on both keeps the vote on `into`.
    pub async fn merge(
        &self,
        from: i64,
        into: i64,
        by: By,
        reason: &str,
        now: i64,
    ) -> Result<Outcome> {
        if let Err(e) = reason_ok(reason) {
            return Ok(Err(e));
        }
        if from == into {
            return Ok(Err(ModError::Refused("A story cannot merge into itself.")));
        }
        let mut tx = self.pool().begin().await?;
        let states: Vec<(i64, i64, Option<i64>)> = sqlx::query_as(
            "SELECT id, user_id, merged_into FROM stories WHERE id IN (?, ?) AND deleted_at IS NULL",
        )
        .bind(from)
        .bind(into)
        .fetch_all(&mut *tx)
        .await?;
        let get = |id: i64| states.iter().find(|s| s.0 == id);
        let (Some(f), Some(t)) = (get(from), get(into)) else {
            return Ok(Err(ModError::NotFound));
        };
        if f.2.is_some() {
            return Ok(Err(ModError::Refused("That story is already merged.")));
        }
        if t.2.is_some() {
            return Ok(Err(ModError::Refused(
                "The target story is itself merged; merge into the story it points to.",
            )));
        }
        let author = f.1;
        sqlx::query("UPDATE comments SET story_id = ?1, merged_from = ?2 WHERE story_id = ?2")
            .bind(into)
            .bind(from)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE votes SET story_id = ?1, merged_from = ?2 WHERE story_id = ?2 \
             AND user_id NOT IN (SELECT user_id FROM votes WHERE story_id = ?1)",
        )
        .bind(into)
        .bind(from)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE stories SET merged_into = ? WHERE id = ?")
            .bind(into)
            .bind(from)
            .execute(&mut *tx)
            .await?;
        recount(&mut tx, into).await?;
        recount(&mut tx, from).await?;
        let target: String = sqlx::query_scalar("SELECT short_id FROM stories WHERE id = ?")
            .bind(into)
            .fetch_one(&mut *tx)
            .await?;
        let detail = format!("into /s/{target}");
        log(
            &mut tx,
            by,
            &LogEntry {
                action: "merge story",
                user_id: Some(author),
                story_id: Some(from),
                reason: reason.trim(),
                detail: &detail,
                ..Default::default()
            },
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(Ok(()))
    }

    /// Undoes a merge: what moved out of `from` moves back.
    pub async fn unmerge(&self, from: i64, by: By, reason: &str, now: i64) -> Result<Outcome> {
        if let Err(e) = reason_ok(reason) {
            return Ok(Err(e));
        }
        let mut tx = self.pool().begin().await?;
        let Some((author, into)): Option<(i64, Option<i64>)> = sqlx::query_as(
            "SELECT user_id, merged_into FROM stories WHERE id = ? AND deleted_at IS NULL",
        )
        .bind(from)
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(Err(ModError::NotFound));
        };
        let Some(into) = into else {
            return Ok(Err(ModError::Refused("That story is not merged.")));
        };
        sqlx::query("UPDATE comments SET story_id = ?1, merged_from = NULL WHERE merged_from = ?1")
            .bind(from)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE votes SET story_id = ?1, merged_from = NULL WHERE merged_from = ?1")
            .bind(from)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE stories SET merged_into = NULL WHERE id = ?")
            .bind(from)
            .execute(&mut *tx)
            .await?;
        recount(&mut tx, into).await?;
        recount(&mut tx, from).await?;
        log(
            &mut tx,
            by,
            &LogEntry {
                action: "unmerge story",
                user_id: Some(author),
                story_id: Some(from),
                reason: reason.trim(),
                ..Default::default()
            },
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(Ok(()))
    }

    /// Bans or unbans a user. Only an admin bans a moderator or admin, and
    /// nobody bans themselves.
    pub async fn set_banned(
        &self,
        user_id: i64,
        banned: bool,
        by: By,
        by_admin: bool,
        reason: &str,
        now: i64,
    ) -> Result<Outcome> {
        if let Err(e) = reason_ok(reason) {
            return Ok(Err(e));
        }
        if by.user_id() == Some(user_id) {
            return Ok(Err(ModError::Refused("You cannot ban yourself.")));
        }
        let mut tx = self.pool().begin().await?;
        let Some((role, banned_at)): Option<(String, Option<i64>)> =
            sqlx::query_as("SELECT role, banned_at FROM users WHERE id = ?")
                .bind(user_id)
                .fetch_optional(&mut *tx)
                .await?
        else {
            return Ok(Err(ModError::NotFound));
        };
        if role != "user" && !by_admin {
            return Ok(Err(ModError::Refused(
                "Only an admin can ban a moderator or admin.",
            )));
        }
        if banned == banned_at.is_some() {
            return Ok(Err(ModError::Refused(if banned {
                "That account is already banned."
            } else {
                "That account is not banned."
            })));
        }
        if banned {
            sqlx::query(
                "UPDATE users SET banned_at = ?, banned_by = ?, banned_reason = ? WHERE id = ?",
            )
            .bind(now)
            .bind(by.user_id())
            .bind(reason.trim())
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        } else {
            sqlx::query("UPDATE users SET banned_at = NULL, banned_by = NULL, banned_reason = NULL WHERE id = ?")
                .bind(user_id)
                .execute(&mut *tx)
                .await?;
        }
        log(
            &mut tx,
            by,
            &LogEntry {
                action: if banned { "ban" } else { "unban" },
                user_id: Some(user_id),
                reason: reason.trim(),
                ..Default::default()
            },
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(Ok(()))
    }

    pub async fn grant_badge(
        &self,
        user_id: i64,
        label: &str,
        link: Option<&str>,
        by: By,
        reason: &str,
        now: i64,
    ) -> Result<Outcome> {
        if let Err(e) = reason_ok(reason) {
            return Ok(Err(e));
        }
        let label = label.trim();
        if label.is_empty() || label.chars().count() > 40 {
            return Ok(Err(ModError::Refused(
                "A badge label is 1 to 40 characters.",
            )));
        }
        if let Some(l) = link {
            if threaded_core::link::parse(l).is_err() {
                return Ok(Err(ModError::Refused(
                    "The badge link must be an http or https URL.",
                )));
            }
        }
        let Some(granted_by) = by.user_id() else {
            return Ok(Err(ModError::Refused("Badges are granted by a moderator.")));
        };
        let mut tx = self.pool().begin().await?;
        sqlx::query("INSERT INTO role_badges (user_id, label, link, granted_by, created_at) VALUES (?, ?, ?, ?, ?)")
            .bind(user_id)
            .bind(label)
            .bind(link)
            .bind(granted_by)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        log(
            &mut tx,
            by,
            &LogEntry {
                action: "grant badge",
                user_id: Some(user_id),
                reason: reason.trim(),
                detail: label,
                ..Default::default()
            },
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(Ok(()))
    }

    pub async fn remove_badge(
        &self,
        badge_id: i64,
        by: By,
        reason: &str,
        now: i64,
    ) -> Result<Outcome> {
        if let Err(e) = reason_ok(reason) {
            return Ok(Err(e));
        }
        let mut tx = self.pool().begin().await?;
        let Some((user_id, label)): Option<(i64, String)> = sqlx::query_as(
            "SELECT user_id, label FROM role_badges WHERE id = ? AND removed_at IS NULL",
        )
        .bind(badge_id)
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(Err(ModError::NotFound));
        };
        sqlx::query("UPDATE role_badges SET removed_at = ? WHERE id = ?")
            .bind(now)
            .bind(badge_id)
            .execute(&mut *tx)
            .await?;
        log(
            &mut tx,
            by,
            &LogEntry {
                action: "remove badge",
                user_id: Some(user_id),
                reason: reason.trim(),
                detail: &label,
                ..Default::default()
            },
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(Ok(()))
    }

    /// Active badges for these users.
    pub async fn badges(&self, user_ids: &[i64]) -> Result<Vec<Badge>> {
        if user_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut qb = sqlx::QueryBuilder::new(
            "SELECT id, user_id, label, link FROM role_badges WHERE removed_at IS NULL AND user_id IN (",
        );
        let mut sep = qb.separated(", ");
        for id in user_ids {
            sep.push_bind(*id);
        }
        qb.push(") ORDER BY created_at");
        qb.build_query_as().fetch_all(self.pool()).await
    }

    /// A private note on a user. Notes are not logged.
    pub async fn add_note(
        &self,
        user_id: i64,
        moderator_id: i64,
        note: &str,
        now: i64,
    ) -> Result<Outcome> {
        if note.trim().is_empty() {
            return Ok(Err(ModError::Refused("Write the note first.")));
        }
        sqlx::query(
            "INSERT INTO mod_notes (user_id, moderator_id, note, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(user_id)
        .bind(moderator_id)
        .bind(note.trim())
        .bind(now)
        .execute(self.pool())
        .await?;
        Ok(Ok(()))
    }

    pub async fn notes(&self, user_id: i64) -> Result<Vec<NoteRow>> {
        sqlx::query_as(
            "SELECT u.handle AS moderator, n.note, n.created_at FROM mod_notes n \
             JOIN users u ON u.id = n.moderator_id WHERE n.user_id = ? ORDER BY n.created_at DESC, n.id DESC",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await
    }

    /// The public log, newest first.
    pub async fn mod_log(&self, limit: i64, offset: i64) -> Result<Vec<LogRow>> {
        sqlx::query_as(&format!(
            "SELECT {LOG_COLUMNS} FROM {LOG_FROM} ORDER BY l.created_at DESC, l.id DESC LIMIT ? OFFSET ?"
        ))
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await
    }

    /// Log rows about one story, its comments, or one user, oldest first.
    pub async fn mod_log_for(&self, target: LogTarget) -> Result<Vec<LogRow>> {
        let (cond, id) = match target {
            LogTarget::Story(id) => ("(l.story_id = ?1 OR c.story_id = ?1)", id),
            LogTarget::Comment(id) => ("l.comment_id = ?1", id),
            LogTarget::User(id) => ("l.user_id = ?1", id),
        };
        sqlx::query_as(&format!(
            "SELECT {LOG_COLUMNS} FROM {LOG_FROM} WHERE {cond} ORDER BY l.created_at, l.id"
        ))
        .bind(id)
        .fetch_all(self.pool())
        .await
    }
}

#[derive(Debug, Clone, Copy)]
pub enum LogTarget {
    Story(i64),
    Comment(i64),
    User(i64),
}

async fn set_taggings<'a>(
    tx: &mut Transaction<'_, Sqlite>,
    story_id: i64,
    names: impl Iterator<Item = &'a str>,
) -> Result<()> {
    sqlx::query("DELETE FROM taggings WHERE story_id = ?")
        .bind(story_id)
        .execute(&mut **tx)
        .await?;
    for name in names {
        sqlx::query(
            "INSERT INTO taggings (story_id, tag_id) SELECT ?, id FROM tags WHERE name = ?",
        )
        .bind(story_id)
        .bind(name)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// One row in a moderation queue tab.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct QueueItem {
    /// `story` or `comment`.
    pub kind: String,
    pub id: i64,
    pub short_id: String,
    /// The story's short ID and title; for a comment, its story's.
    pub story_short_id: String,
    pub title: String,
    pub body: String,
    pub url: Option<String>,
    pub author_id: i64,
    pub author: String,
    pub author_karma: i64,
    pub author_since: i64,
    pub score: i64,
    pub flags: i64,
    pub created_at: i64,
}

const QUEUE_STORY: &str =
    "SELECT 'story' AS kind, s.id, s.short_id, s.short_id AS story_short_id, \
     s.title, s.body, s.url, s.user_id AS author_id, u.handle AS author, u.karma AS author_karma, \
     u.created_at AS author_since, s.score, s.flags, s.created_at \
     FROM stories s JOIN users u ON u.id = s.user_id";

const QUEUE_COMMENT: &str = "SELECT 'comment' AS kind, c.id, c.short_id, s.short_id AS story_short_id, \
     s.title, c.body, NULL AS url, c.user_id AS author_id, u.handle AS author, u.karma AS author_karma, \
     u.created_at AS author_since, c.score, c.flags, c.created_at \
     FROM comments c JOIN users u ON u.id = c.user_id JOIN stories s ON s.id = c.story_id";

/// A flag with who gave it, for moderators.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct FlagRow {
    pub handle: String,
    pub reason: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RevisionRow {
    pub title: Option<String>,
    pub url: Option<String>,
    pub body: String,
    pub tags: Option<String>,
    pub edited_at: i64,
}

/// A user's record as a moderator reviews it.
#[derive(Debug, Clone, Default)]
pub struct Standing {
    pub removals: i64,
    /// Flags received, by reason, with counts.
    pub flags_received: Vec<(String, i64)>,
    pub flags_given: i64,
    /// Flags given on items a moderator later removed.
    pub flags_upheld: i64,
}

impl Store {
    /// Visible items with at least `min_flags` flags, most flagged first.
    pub async fn flagged(&self, min_flags: i64) -> Result<Vec<QueueItem>> {
        sqlx::query_as(&format!(
            "SELECT * FROM ({QUEUE_STORY} WHERE s.flags >= ?1 AND s.deleted_at IS NULL \
             AND s.removed_at IS NULL AND s.merged_into IS NULL \
             UNION ALL {QUEUE_COMMENT} WHERE c.flags >= ?1 AND c.deleted_at IS NULL \
             AND c.removed_at IS NULL) ORDER BY flags DESC, created_at"
        ))
        .bind(min_flags)
        .fetch_all(self.pool())
        .await
    }

    pub async fn queue_item(&self, target: crate::Target) -> Result<Option<QueueItem>> {
        match target {
            crate::Target::Story(id) => {
                sqlx::query_as(&format!("{QUEUE_STORY} WHERE s.id = ?"))
                    .bind(id)
                    .fetch_optional(self.pool())
                    .await
            }
            crate::Target::Comment(id) => {
                sqlx::query_as(&format!("{QUEUE_COMMENT} WHERE c.id = ?"))
                    .bind(id)
                    .fetch_optional(self.pool())
                    .await
            }
        }
    }

    /// Every flag on an item with its flagger. Only moderators see this.
    pub async fn flags_on(&self, target: crate::Target) -> Result<Vec<FlagRow>> {
        let (col, id) = crate::m2::target_cols(target);
        sqlx::query_as(&format!(
            "SELECT u.handle, v.reason, v.created_at FROM votes v JOIN users u ON u.id = v.user_id \
             WHERE v.{col} = ? AND v.value = -1 ORDER BY v.created_at"
        ))
        .bind(id)
        .fetch_all(self.pool())
        .await
    }

    /// Prior versions of a story or comment, oldest first.
    pub async fn revisions(&self, target: crate::Target) -> Result<Vec<RevisionRow>> {
        match target {
            crate::Target::Story(id) => sqlx::query_as(
                "SELECT title, url, body, tags, edited_at FROM story_revisions \
                 WHERE story_id = ? ORDER BY edited_at, id",
            )
            .bind(id)
            .fetch_all(self.pool())
            .await,
            crate::Target::Comment(id) => sqlx::query_as(
                "SELECT NULL AS title, NULL AS url, body, NULL AS tags, edited_at FROM comment_revisions \
                 WHERE comment_id = ? ORDER BY edited_at, id",
            )
            .bind(id)
            .fetch_all(self.pool())
            .await,
        }
    }

    pub async fn standing(&self, user_id: i64) -> Result<Standing> {
        let removals: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM stories WHERE user_id = ?1 AND removed_at IS NOT NULL) + \
             (SELECT count(*) FROM comments WHERE user_id = ?1 AND removed_at IS NOT NULL)",
        )
        .bind(user_id)
        .fetch_one(self.pool())
        .await?;
        let flags_received = sqlx::query_as(
            "SELECT v.reason, count(*) FROM votes v \
             LEFT JOIN stories s ON s.id = v.story_id LEFT JOIN comments c ON c.id = v.comment_id \
             WHERE v.value = -1 AND coalesce(s.user_id, c.user_id) = ? \
             GROUP BY v.reason ORDER BY count(*) DESC, v.reason",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await?;
        let (flags_given, flags_upheld): (i64, i64) = sqlx::query_as(
            "SELECT count(*), coalesce(sum(coalesce(s.removed_at, c.removed_at) IS NOT NULL), 0) \
             FROM votes v LEFT JOIN stories s ON s.id = v.story_id \
             LEFT JOIN comments c ON c.id = v.comment_id WHERE v.user_id = ? AND v.value = -1",
        )
        .bind(user_id)
        .fetch_one(self.pool())
        .await?;
        Ok(Standing {
            removals,
            flags_received,
            flags_given,
            flags_upheld,
        })
    }

    /// A user's stories and comments for review, hidden ones included.
    pub async fn activity(&self, user_id: i64, limit: i64) -> Result<Vec<QueueItem>> {
        sqlx::query_as(&format!(
            "SELECT * FROM ({QUEUE_STORY} WHERE s.user_id = ?1 AND s.deleted_at IS NULL \
             UNION ALL {QUEUE_COMMENT} WHERE c.user_id = ?1 AND c.deleted_at IS NULL) \
             ORDER BY created_at DESC LIMIT ?2"
        ))
        .bind(user_id)
        .bind(limit)
        .fetch_all(self.pool())
        .await
    }

    /// A comment by ID, any state.
    pub async fn comment_by_id(&self, id: i64) -> Result<Option<crate::CommentRow>> {
        let short: Option<String> =
            sqlx::query_scalar("SELECT short_id FROM comments WHERE id = ?")
                .bind(id)
                .fetch_optional(self.pool())
                .await?;
        match short {
            Some(s) => self.comment(&s).await,
            None => Ok(None),
        }
    }
}

/// A tag's fields as an admin edits them.
#[derive(Debug, Clone, PartialEq)]
pub struct TagFields<'a> {
    pub description: &'a str,
    pub category: &'a str,
    pub rank_boost: f64,
    pub privileged: bool,
    pub new_users_ok: bool,
    pub is_media: bool,
    pub active: bool,
}

impl Store {
    /// Stored settings as key and value.
    pub async fn settings(&self) -> Result<Vec<(String, String)>> {
        sqlx::query_as("SELECT key, value FROM settings ORDER BY key")
            .fetch_all(self.pool())
            .await
    }

    /// Saves changed settings, each logged with its old and new value.
    pub async fn save_settings(
        &self,
        changes: &[(&str, String, String)],
        by: By,
        reason: &str,
        now: i64,
    ) -> Result<Outcome> {
        if let Err(e) = reason_ok(reason) {
            return Ok(Err(e));
        }
        let mut tx = self.pool().begin().await?;
        for (key, old, new) in changes {
            sqlx::query(
                "INSERT INTO settings (key, value, updated_by, updated_at) VALUES (?, ?, ?, ?) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, \
                 updated_by = excluded.updated_by, updated_at = excluded.updated_at",
            )
            .bind(key)
            .bind(new)
            .bind(by.user_id())
            .bind(now)
            .execute(&mut *tx)
            .await?;
            let detail = format!("{key}: {old} to {new}");
            log(
                &mut tx,
                by,
                &LogEntry {
                    action: "change setting",
                    reason: reason.trim(),
                    detail: &detail,
                    ..Default::default()
                },
                now,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(Ok(()))
    }

    /// Creates a tag or changes one, logging what changed.
    pub async fn save_tag(
        &self,
        name: &str,
        f: &TagFields<'_>,
        by: By,
        reason: &str,
        now: i64,
    ) -> Result<Outcome> {
        if let Err(e) = reason_ok(reason) {
            return Ok(Err(e));
        }
        if !threaded_core::tag::valid_name(name) {
            return Ok(Err(ModError::Refused(
                "A tag name is lowercase letters, digits, and - _ +, up to 25 characters.",
            )));
        }
        if !(-10.0..=10.0).contains(&f.rank_boost) {
            return Ok(Err(ModError::Refused("A rank boost is from -10 to 10.")));
        }
        let mut tx = self.pool().begin().await?;
        let old: Option<crate::TagRow> = sqlx::query_as(
            "SELECT id, name, description, category, rank_boost, privileged, new_users_ok, is_media, active \
             FROM tags WHERE name = ?",
        )
        .bind(name)
        .fetch_optional(&mut *tx)
        .await?;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO tags (name, description, category, rank_boost, privileged, new_users_ok, is_media, active) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(name) DO UPDATE SET description = excluded.description, category = excluded.category, \
             rank_boost = excluded.rank_boost, privileged = excluded.privileged, \
             new_users_ok = excluded.new_users_ok, is_media = excluded.is_media, active = excluded.active \
             RETURNING id",
        )
        .bind(name)
        .bind(f.description.trim())
        .bind(f.category.trim())
        .bind(f.rank_boost)
        .bind(f.privileged)
        .bind(f.new_users_ok)
        .bind(f.is_media)
        .bind(f.active)
        .fetch_one(&mut *tx)
        .await?;
        let (action, detail) = match old {
            None => ("create tag", String::new()),
            Some(t) => {
                let (d, c, b) = (t.description, t.category, t.rank_boost);
                let (p, n, m, a) = (t.privileged, t.new_users_ok, t.is_media, t.active);
                let mut ch = Vec::new();
                if d != f.description.trim() {
                    ch.push("description".to_string());
                }
                if c != f.category.trim() {
                    ch.push(format!("category: {c} to {}", f.category.trim()));
                }
                if b != f.rank_boost {
                    ch.push(format!("rank boost: {b} to {}", f.rank_boost));
                }
                for (label, was, now) in [
                    ("privileged", p, f.privileged),
                    ("new users", n, f.new_users_ok),
                    ("media", m, f.is_media),
                    ("active", a, f.active),
                ] {
                    if was != now {
                        ch.push(format!("{label}: {was} to {now}"));
                    }
                }
                if ch.is_empty() {
                    return Ok(Err(ModError::Refused("Nothing changed.")));
                }
                ("edit tag", ch.join("; "))
            }
        };
        log(
            &mut tx,
            by,
            &LogEntry {
                action,
                tag_id: Some(id),
                reason: reason.trim(),
                detail: &detail,
                ..Default::default()
            },
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(Ok(()))
    }

    /// Moderators and admins, by role then handle.
    pub async fn staff_members(&self) -> Result<Vec<crate::UserRow>> {
        sqlx::query_as(
            "SELECT id, did, handle, handle_verified, role, karma, public_votes, vouched_by, created_at, \
             banned_at, banned_reason FROM users WHERE role <> 'user' ORDER BY role, handle",
        )
        .fetch_all(self.pool())
        .await
    }

    /// Every active role badge with its holder's handle.
    pub async fn badges_in_use(&self) -> Result<Vec<(String, Badge)>> {
        let rows: Vec<(String, i64, i64, String, Option<String>)> = sqlx::query_as(
            "SELECT u.handle, b.id, b.user_id, b.label, b.link FROM role_badges b \
             JOIN users u ON u.id = b.user_id WHERE b.removed_at IS NULL ORDER BY b.label, u.handle",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|(h, id, user_id, label, link)| {
                (
                    h,
                    Badge {
                        id,
                        user_id,
                        label,
                        link,
                    },
                )
            })
            .collect())
    }
}

async fn override_story_tx(
    tx: &mut Transaction<'_, Sqlite>,
    story: &crate::StoryRow,
    o: &StoryOverride<'_>,
    by: By,
    reason: &str,
    now: i64,
) -> Result<()> {
    sqlx::query("INSERT INTO story_revisions (story_id, title, url, body, tags, at_cid, edited_at) VALUES (?, ?, ?, ?, ?, ?, ?)")
        .bind(story.id)
        .bind(&story.title)
        .bind(&story.url)
        .bind(&story.body)
        .bind(&story.tags)
        .bind(&story.at_cid)
        .bind(now)
        .execute(&mut **tx)
        .await?;
    if !story.mod_edited {
        sqlx::query(
            "UPDATE stories SET author_title = title, author_url = url, author_tags = ? WHERE id = ?",
        )
        .bind(&story.tags)
        .bind(story.id)
        .execute(&mut **tx)
        .await?;
    }
    sqlx::query(
        "UPDATE stories SET title = ?, url = ?, canonical_url = ?, domain = ?, \
         override_by = ?, override_at = ? WHERE id = ?",
    )
    .bind(o.title)
    .bind(o.link.map(|l| l.url.as_str()))
    .bind(o.link.map(|l| l.canonical.as_str()))
    .bind(o.link.map(|l| l.domain.as_str()))
    .bind(by.user_id())
    .bind(now)
    .bind(story.id)
    .execute(&mut **tx)
    .await?;
    set_taggings(tx, story.id, o.tags.iter().map(String::as_str)).await?;
    crate::m2::recompute_rank(tx, story.id).await?;
    let mut changes = Vec::new();
    if o.title != story.title {
        changes.push(format!("title: {} to {}", story.title, o.title));
    }
    let new_url = o.link.map(|l| l.url.clone());
    if new_url != story.url {
        changes.push(format!(
            "link: {} to {}",
            story.url.as_deref().unwrap_or("none"),
            new_url.as_deref().unwrap_or("none")
        ));
    }
    let new_tags = o.tags.join(" ");
    if new_tags != story.tags {
        changes.push(format!("tags: {} to {new_tags}", story.tags));
    }
    let detail = changes.join("; ");
    log(
        tx,
        by,
        &LogEntry {
            action: "edit story",
            user_id: Some(story.user_id),
            story_id: Some(story.id),
            reason: reason.trim(),
            detail: &detail,
            ..Default::default()
        },
        now,
    )
    .await?;
    Ok(())
}

/// An open edit suggestion with what it would change.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SuggestionRow {
    pub id: i64,
    pub story_id: i64,
    pub story_short_id: String,
    pub current_title: String,
    pub current_tags: String,
    pub title: Option<String>,
    pub tags: Option<String>,
    pub by_handle: String,
    pub created_at: i64,
}

impl Store {
    /// Records a user's suggested title or tags for a story, replacing their
    /// earlier open suggestion for it. `None` leaves that field as it is.
    pub async fn suggest(
        &self,
        story_id: i64,
        user_id: i64,
        title: Option<&str>,
        tags: Option<&str>,
        now: i64,
    ) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "DELETE FROM suggestions WHERE story_id = ? AND user_id = ? AND decision IS NULL",
        )
        .bind(story_id)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO suggestions (story_id, user_id, title, tags, created_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(story_id)
        .bind(user_id)
        .bind(title)
        .bind(tags)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await
    }

    /// Open suggestions on visible stories, oldest first.
    pub async fn open_suggestions(&self) -> Result<Vec<SuggestionRow>> {
        sqlx::query_as(
            "SELECT g.id, g.story_id, s.short_id AS story_short_id, s.title AS current_title, \
             coalesce((SELECT group_concat(name, ' ') FROM (SELECT t.name FROM taggings tg \
                 JOIN tags t ON t.id = tg.tag_id WHERE tg.story_id = s.id ORDER BY t.name)), '') AS current_tags, \
             g.title, g.tags, u.handle AS by_handle, g.created_at \
             FROM suggestions g JOIN stories s ON s.id = g.story_id JOIN users u ON u.id = g.user_id \
             WHERE g.decision IS NULL AND s.deleted_at IS NULL AND s.removed_at IS NULL \
             AND s.merged_into IS NULL ORDER BY g.created_at, g.id",
        )
        .fetch_all(self.pool())
        .await
    }

    /// Accepts a suggestion, applying it as a moderator edit, or dismisses it.
    pub async fn decide_suggestion(
        &self,
        id: i64,
        accept: bool,
        by: By,
        reason: &str,
        now: i64,
    ) -> Result<Outcome> {
        if let Err(e) = reason_ok(reason) {
            return Ok(Err(e));
        }
        let Some(g) = self
            .open_suggestions()
            .await?
            .into_iter()
            .find(|g| g.id == id)
        else {
            return Ok(Err(ModError::NotFound));
        };
        let Some(story) = self.story_any(&g.story_short_id).await? else {
            return Ok(Err(ModError::NotFound));
        };
        let mut tx = self.pool().begin().await?;
        let decided = sqlx::query(
            "UPDATE suggestions SET decision = ?, decided_by = ?, decided_at = ? \
             WHERE id = ? AND decision IS NULL",
        )
        .bind(if accept { "accepted" } else { "dismissed" })
        .bind(by.user_id())
        .bind(now)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        if decided.rows_affected() == 0 {
            return Ok(Err(ModError::Refused(
                "That suggestion is already decided.",
            )));
        }
        if accept {
            let title = g.title.as_deref().unwrap_or(&story.title);
            let tags: Vec<String> = g
                .tags
                .as_deref()
                .unwrap_or(&story.tags)
                .split_whitespace()
                .map(str::to_string)
                .collect();
            let link = story
                .url
                .as_deref()
                .and_then(|u| threaded_core::link::parse(u).ok());
            let reason = format!("{} (suggested by {})", reason.trim(), g.by_handle);
            override_story_tx(
                &mut tx,
                &story,
                &StoryOverride {
                    title,
                    link: link.as_ref(),
                    tags: &tags,
                },
                by,
                &reason,
                now,
            )
            .await?;
        } else {
            log(
                &mut tx,
                by,
                &LogEntry {
                    action: "dismiss suggestion",
                    story_id: Some(story.id),
                    reason: reason.trim(),
                    ..Default::default()
                },
                now,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(Ok(()))
    }
}
