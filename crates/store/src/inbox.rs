//! The replies inbox: comments that answer a reader. A top-level comment on
//! their story, or a reply to their comment, from the web or the network.

use crate::{CommentRow, Result, Store, COMMENT_COLUMNS, VISIBLE};

/// Comments that answer user `?1` and that readers can see: not their own,
/// not deleted, removed, or held, not by an account the network reports
/// inactive, and on a visible story. `FROM comments c JOIN users u JOIN
/// stories s` is assumed.
fn replies_to() -> String {
    format!(
        "c.user_id <> ?1 AND c.deleted_at IS NULL AND c.removed_at IS NULL \
         AND c.held_at IS NULL AND u.inactive_at IS NULL AND {VISIBLE} \
         AND c.id IN (\
           SELECT r.id FROM stories o JOIN comments r ON r.story_id = o.id \
           WHERE o.user_id = ?1 AND r.parent_id IS NULL \
           UNION ALL \
           SELECT r.id FROM comments p JOIN comments r ON r.parent_id = p.id \
           WHERE p.user_id = ?1)"
    )
}

impl Store {
    /// Replies to a user, newest first.
    pub async fn replies(&self, user_id: i64, limit: i64, offset: i64) -> Result<Vec<CommentRow>> {
        sqlx::query_as(&format!(
            "SELECT {COMMENT_COLUMNS} FROM comments c JOIN users u ON u.id = c.user_id \
             JOIN stories s ON s.id = c.story_id WHERE {} \
             ORDER BY c.created_at DESC, c.id DESC LIMIT ?2 OFFSET ?3",
            replies_to()
        ))
        .bind(user_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await
    }

    /// When the user last opened the inbox; replies after it are unread.
    /// An account that never opened it counts from when it was created.
    pub async fn inbox_read_at(&self, user_id: i64) -> Result<i64> {
        sqlx::query_scalar("SELECT coalesce(inbox_read_at, created_at) FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_one(self.pool())
            .await
    }

    /// Replies written after the user last opened the inbox. A reply is
    /// dated by its record, so one backfilled from the network with an
    /// older date does not count as unread.
    pub async fn unread_replies(&self, user_id: i64) -> Result<i64> {
        sqlx::query_scalar(&format!(
            "SELECT count(*) FROM comments c JOIN users u ON u.id = c.user_id \
             JOIN stories s ON s.id = c.story_id WHERE {} \
             AND c.created_at > (SELECT coalesce(inbox_read_at, created_at) FROM users WHERE id = ?1)",
            replies_to()
        ))
        .bind(user_id)
        .fetch_one(self.pool())
        .await
    }

    /// Marks every reply up to `now` read.
    pub async fn mark_inbox_read(&self, user_id: i64, now: i64) -> Result<()> {
        sqlx::query("UPDATE users SET inbox_read_at = ? WHERE id = ?")
            .bind(now)
            .bind(user_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }
}
