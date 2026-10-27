//! Lookups and changes the network indexer needs: records found by AT-URI,
//! accounts known only from the network, and detached stories.

use crate::m2::{COMMENT_COLUMNS, USER_COLUMNS};
use crate::{CommentRow, Result, Store, StoryEdit, StoryRow, Target, UserRow, STORY_COLUMNS};

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
    /// then hides the story for moderators. The prior version is kept in
    /// `story_revisions`, as for any edit.
    pub async fn detach_story(&self, story: &StoryRow, e: &StoryEdit<'_>, now: i64) -> Result<()> {
        self.edit_story(story, e, now).await?;
        sqlx::query("UPDATE stories SET detached_at = ? WHERE id = ?")
            .bind(now)
            .bind(story.id)
            .execute(self.pool())
            .await?;
        Ok(())
    }
}
