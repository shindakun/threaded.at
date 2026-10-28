//! Bluesky cross-posting: the user's choice, and what became of each story's
//! post. Only stories submitted on the web are cross-posted, never records
//! read from the network.

use crate::{Result, Store};

/// What became of a story's Bluesky post.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BskyPost {
    /// The post's AT-URI.
    Posted(String),
    /// Why the post was not made, in words for the author.
    Failed(String),
}

impl Store {
    pub async fn set_cross_post(&self, user_id: i64, on: bool) -> Result<()> {
        sqlx::query("UPDATE users SET cross_post = ? WHERE id = ?")
            .bind(on)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// A story's Bluesky post, if one was tried. A post that was made stays
    /// made: a later failure, such as a second try, never replaces it.
    pub async fn bsky_post(&self, story_id: i64) -> Result<Option<BskyPost>> {
        let row: Option<(Option<String>, Option<String>)> =
            sqlx::query_as("SELECT bsky_post_uri, bsky_post_error FROM stories WHERE id = ?")
                .bind(story_id)
                .fetch_optional(&self.pool)
                .await?;
        Ok(match row {
            Some((Some(uri), _)) => Some(BskyPost::Posted(uri)),
            Some((None, Some(e))) => Some(BskyPost::Failed(e)),
            _ => None,
        })
    }

    pub async fn set_bsky_post(&self, story_id: i64, post: &BskyPost) -> Result<()> {
        match post {
            BskyPost::Posted(uri) => {
                sqlx::query(
                    "UPDATE stories SET bsky_post_uri = ?, bsky_post_error = NULL WHERE id = ?",
                )
                .bind(uri)
                .bind(story_id)
                .execute(&self.pool)
                .await?
            }
            BskyPost::Failed(e) => {
                sqlx::query(
                    "UPDATE stories SET bsky_post_error = ? WHERE id = ? AND bsky_post_uri IS NULL",
                )
                .bind(e)
                .bind(story_id)
                .execute(&self.pool)
                .await?
            }
        };
        Ok(())
    }
}
