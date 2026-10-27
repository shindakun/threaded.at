//! Lookups and changes the network indexer needs: records found by AT-URI,
//! accounts known only from the network, and detached stories.

use crate::m2::{COMMENT_COLUMNS, USER_COLUMNS};
use crate::moderation::{log, QUEUE_STORY};
use crate::{
    By, CommentRow, LogEntry, ModError, Outcome, QueueItem, Result, Store, StoryEdit, StoryRow,
    Target, UserRow, STORY_COLUMNS,
};

/// An upvote or flag found by the AT-URI of its published record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::FromRow)]
pub struct VoteByUri {
    pub id: i64,
    pub user_id: i64,
    pub value: i64,
    pub story_id: Option<i64>,
    pub comment_id: Option<i64>,
}

impl VoteByUri {
    pub fn target(&self) -> Option<Target> {
        match (self.story_id, self.comment_id) {
            (Some(s), _) => Some(Target::Story(s)),
            (_, Some(c)) => Some(Target::Comment(c)),
            _ => None,
        }
    }
}

impl Store {
    /// The account for a DID seen on the network, created as a New account
    /// if it has never signed in here. Its handle is the DID until a
    /// background check verifies one.
    pub async fn network_user(&self, did: &str, now: i64) -> Result<UserRow> {
        sqlx::query(
            "INSERT INTO users (did, handle, handle_verified, created_at) VALUES (?1, ?1, 0, ?2) \
             ON CONFLICT(did) DO NOTHING",
        )
        .bind(did)
        .bind(now)
        .execute(self.pool())
        .await?;
        sqlx::query_as(&format!("SELECT {USER_COLUMNS} FROM users WHERE did = ?"))
            .bind(did)
            .fetch_one(self.pool())
            .await
    }

    /// A story by its record's AT-URI, whatever its moderation state, unless
    /// deleted.
    pub async fn story_by_uri(&self, uri: &str) -> Result<Option<StoryRow>> {
        sqlx::query_as(&format!(
            "SELECT {STORY_COLUMNS} FROM stories s JOIN users u ON u.id = s.user_id \
             WHERE s.at_uri = ? AND s.deleted_at IS NULL"
        ))
        .bind(uri)
        .fetch_optional(self.pool())
        .await
    }

    /// A comment by its record's AT-URI, deleted ones included.
    pub async fn comment_by_uri(&self, uri: &str) -> Result<Option<CommentRow>> {
        sqlx::query_as(&format!(
            "SELECT {COMMENT_COLUMNS} FROM comments c JOIN users u ON u.id = c.user_id \
             JOIN stories s ON s.id = c.story_id WHERE c.at_uri = ?"
        ))
        .bind(uri)
        .fetch_optional(self.pool())
        .await
    }

    pub async fn vote_by_uri(&self, uri: &str) -> Result<Option<VoteByUri>> {
        sqlx::query_as(
            "SELECT id, user_id, value, story_id, comment_id FROM votes WHERE at_uri = ?",
        )
        .bind(uri)
        .fetch_optional(self.pool())
        .await
    }

    /// The author's own link while a moderator's edit covers it.
    pub async fn author_url(&self, story_id: i64) -> Result<Option<String>> {
        sqlx::query_scalar("SELECT author_url FROM stories WHERE id = ?")
            .bind(story_id)
            .fetch_one(self.pool())
            .await
    }

    /// Applies an author's edit that changed the link after the edit window,
    /// then hides the story until a moderator reattaches or removes it. The
    /// prior version is kept in `story_revisions`, as for any edit, and the
    /// detach is in the public log.
    pub async fn detach_story(&self, story: &StoryRow, e: &StoryEdit<'_>, now: i64) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        // The author's own link, which a moderator's edit may be covering.
        let old_url: Option<String> = if story.mod_edited {
            sqlx::query_scalar("SELECT author_url FROM stories WHERE id = ?")
                .bind(story.id)
                .fetch_one(&mut *tx)
                .await?
        } else {
            story.url.clone()
        };
        crate::m2::edit_story_tx(&mut tx, story, e, now).await?;
        sqlx::query("UPDATE stories SET detached_at = ? WHERE id = ?")
            .bind(now)
            .bind(story.id)
            .execute(&mut *tx)
            .await?;
        let new_url = e.link.map(|l| l.url.as_str()).unwrap_or("no link");
        let detail = format!(
            "from {} to {new_url}",
            old_url.as_deref().unwrap_or("no link")
        );
        log(
            &mut tx,
            By::Indexer,
            &LogEntry {
                action: "detach story",
                user_id: Some(story.user_id),
                story_id: Some(story.id),
                reason: "The author changed the link after the edit window.",
                detail: &detail,
                ..Default::default()
            },
            now,
        )
        .await?;
        tx.commit().await
    }

    /// Puts a detached story back with its new link.
    pub async fn reattach_story(
        &self,
        story_id: i64,
        by: By,
        reason: &str,
        now: i64,
    ) -> Result<Outcome> {
        if reason.trim().is_empty() {
            return Ok(Err(ModError::NoReason));
        }
        let mut tx = self.pool().begin().await?;
        let author: Option<i64> = sqlx::query_scalar(
            "UPDATE stories SET detached_at = NULL \
             WHERE id = ? AND detached_at IS NOT NULL AND deleted_at IS NULL RETURNING user_id",
        )
        .bind(story_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(author) = author else {
            return Ok(Err(ModError::Refused("It is not detached.")));
        };
        log(
            &mut tx,
            by,
            &LogEntry {
                action: "reattach story",
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

    /// Detached stories not yet removed or deleted, oldest first.
    pub async fn detached(&self) -> Result<Vec<DetachedItem>> {
        sqlx::query_as(&format!(
            "SELECT q.*, s.detached_at, \
             (SELECT l.detail FROM mod_log l WHERE l.story_id = s.id \
              AND l.action = 'detach story' ORDER BY l.id DESC LIMIT 1) AS change \
             FROM ({QUEUE_STORY}) q JOIN stories s ON s.id = q.id \
             WHERE s.detached_at IS NOT NULL AND s.deleted_at IS NULL \
             AND s.removed_at IS NULL AND s.merged_into IS NULL \
             ORDER BY s.detached_at, s.id"
        ))
        .fetch_all(self.pool())
        .await
    }
}

/// A row in the Detached tab: the story, and how its link changed.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DetachedItem {
    #[sqlx(flatten)]
    pub item: QueueItem,
    pub detached_at: i64,
    /// The latest detach's log detail: "from OLD to NEW".
    pub change: Option<String>,
}

/// The consumer's position in hydrant's stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IndexerState {
    /// The next event ID to read, or `None` before the first event.
    pub next_id: Option<u64>,
    /// When the cursor last moved.
    pub updated_at: Option<i64>,
    /// Records waiting for the story or comment they refer to.
    pub pending: i64,
}

/// A record waiting for the story or comment it refers to.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct PendingRecord {
    pub id: i64,
    pub uri: String,
    /// The event as JSON, applied again once its subject arrives.
    pub event: String,
}

impl Store {
    /// The next stream event ID to read, or `None` before the first.
    pub async fn stream_cursor(&self) -> Result<Option<u64>> {
        let next: Option<i64> =
            sqlx::query_scalar("SELECT next_id FROM stream_cursor WHERE id = 1")
                .fetch_optional(self.pool())
                .await?;
        Ok(next.map(|n| n as u64))
    }

    /// Where the consumer is, for `/admin`.
    pub async fn indexer_state(&self) -> Result<IndexerState> {
        let cursor: Option<(i64, i64)> =
            sqlx::query_as("SELECT next_id, updated_at FROM stream_cursor WHERE id = 1")
                .fetch_optional(self.pool())
                .await?;
        let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM pending_records")
            .fetch_one(self.pool())
            .await?;
        Ok(IndexerState {
            next_id: cursor.map(|c| c.0 as u64),
            updated_at: cursor.map(|c| c.1),
            pending,
        })
    }

    pub async fn set_stream_cursor(&self, next: u64, now: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO stream_cursor (id, next_id, updated_at) VALUES (1, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET next_id = excluded.next_id, updated_at = excluded.updated_at",
        )
        .bind(next as i64)
        .bind(now)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Holds a record until `waits_for` is indexed. Holding it again
    /// replaces the earlier copy.
    pub async fn hold_pending(
        &self,
        uri: &str,
        waits_for: &str,
        event: &str,
        now: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO pending_records (uri, waits_for, event, created_at) VALUES (?, ?, ?, ?) \
             ON CONFLICT(uri) DO UPDATE SET waits_for = excluded.waits_for, event = excluded.event",
        )
        .bind(uri)
        .bind(waits_for)
        .bind(event)
        .bind(now)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Records waiting for this AT-URI, oldest first.
    pub async fn pending_for(&self, waits_for: &str) -> Result<Vec<PendingRecord>> {
        sqlx::query_as("SELECT id, uri, event FROM pending_records WHERE waits_for = ? ORDER BY id")
            .bind(waits_for)
            .fetch_all(self.pool())
            .await
    }

    pub async fn drop_pending(&self, id: i64) -> Result<()> {
        sqlx::query("DELETE FROM pending_records WHERE id = ?")
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Drops a waiting record by its own AT-URI, as when it is deleted before
    /// its subject arrives. Returns whether one was waiting.
    pub async fn drop_pending_uri(&self, uri: &str) -> Result<bool> {
        let r = sqlx::query("DELETE FROM pending_records WHERE uri = ?")
            .bind(uri)
            .execute(self.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    /// Drops records that waited since before `before`. Returns how many.
    pub async fn expire_pending(&self, before: i64) -> Result<u64> {
        let r = sqlx::query("DELETE FROM pending_records WHERE created_at < ?")
            .bind(before)
            .execute(self.pool())
            .await?;
        Ok(r.rows_affected())
    }
}

impl Store {
    /// Applies an account event: an inactive account (deactivated, taken
    /// down, deleted) is hidden until the network reports it active again.
    /// Returns whether anything changed; unknown DIDs change nothing.
    pub async fn set_account_active(
        &self,
        did: &str,
        active: bool,
        status: Option<&str>,
        now: i64,
    ) -> Result<bool> {
        let r = if active {
            sqlx::query(
                "UPDATE users SET inactive_at = NULL, inactive_status = NULL \
                 WHERE did = ? AND inactive_at IS NOT NULL",
            )
            .bind(did)
            .execute(self.pool())
            .await?
        } else {
            sqlx::query(
                "UPDATE users SET inactive_at = coalesce(inactive_at, ?), inactive_status = ? WHERE did = ?",
            )
            .bind(now)
            .bind(status)
            .bind(did)
            .execute(self.pool())
            .await?
        };
        Ok(r.rows_affected() > 0)
    }

    /// Every DID that has signed in here, for tracking in hydrant.
    pub async fn signed_in_dids(&self) -> Result<Vec<String>> {
        sqlx::query_scalar("SELECT did FROM users WHERE last_seen_at IS NOT NULL ORDER BY id")
            .fetch_all(self.pool())
            .await
    }

    /// Handles an account has held, newest first.
    pub async fn handle_history(&self, user_id: i64) -> Result<Vec<(String, i64)>> {
        sqlx::query_as(
            "SELECT handle, seen_at FROM handle_history WHERE user_id = ? ORDER BY seen_at DESC, id DESC",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await
    }
}
