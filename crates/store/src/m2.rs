use sqlx::{Sqlite, Transaction};
use threaded_core::level::{Account, Role};
use threaded_core::rank::{self, RankInput};

use crate::{short_id, Result, Store, StoryRow, STORY_COLUMNS, VISIBLE};

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct UserRow {
    pub id: i64,
    pub did: String,
    pub handle: String,
    pub handle_verified: bool,
    pub role: String,
    pub karma: i64,
    pub public_votes: bool,
    pub vouched_by: Option<i64>,
    pub created_at: i64,
    pub banned_at: Option<i64>,
    pub banned_reason: Option<String>,
}

impl UserRow {
    pub fn account(&self, now: i64) -> Account {
        Account {
            tenure_days: (now - self.created_at).max(0) / 86_400,
            karma: self.karma,
            role: Role::from_db(&self.role),
            banned: self.banned_at.is_some(),
            vouched: self.vouched_by.is_some(),
        }
    }

    /// The verified handle, or the DID when the handle does not verify.
    pub fn name(&self) -> &str {
        if self.handle_verified {
            &self.handle
        } else {
            &self.did
        }
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TagRow {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub category: String,
    pub rank_boost: f64,
    pub privileged: bool,
    pub new_users_ok: bool,
    pub is_media: bool,
    pub active: bool,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CommentRow {
    pub id: i64,
    pub short_id: String,
    pub story_id: i64,
    pub story_short_id: String,
    pub story_title: String,
    pub parent_id: Option<i64>,
    pub depth: i64,
    pub body: String,
    pub score: i64,
    pub flags: i64,
    pub created_at: i64,
    pub edited_at: Option<i64>,
    pub deleted_at: Option<i64>,
    pub user_id: i64,
    pub author_did: String,
    pub handle: String,
    pub at_uri: Option<String>,
    pub at_cid: Option<String>,
    pub removed_at: Option<i64>,
    pub removed_reason: Option<String>,
    pub held_at: Option<i64>,
}

impl CommentRow {
    /// Removed or held: hidden from everyone but its author and moderators.
    pub fn hidden(&self) -> bool {
        self.removed_at.is_some() || self.held_at.is_some()
    }
}

const COMMENT_COLUMNS: &str = "c.id, c.short_id, c.story_id, s.short_id AS story_short_id, \
     s.title AS story_title, c.parent_id, c.depth, c.body, c.score, c.flags, c.created_at, \
     c.edited_at, c.deleted_at, c.user_id, u.did AS author_did, \
     CASE WHEN u.handle_verified THEN u.handle ELSE u.did END AS handle, c.at_uri, c.at_cid, \
     c.removed_at, c.removed_reason, c.held_at";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    Story(i64),
    Comment(i64),
}

/// A user's vote on one item: an upvote, or a flag with its reason.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct VoteRow {
    pub id: i64,
    pub value: i64,
    pub reason: Option<String>,
    pub at_uri: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoteChange<'a> {
    Up { at_uri: Option<&'a str> },
    Flag { reason: &'a str },
    Clear,
}

/// A vote row as `votes_on_story` reads it: story ID, comment ID, vote ID,
/// value, flag reason, record URI.
type StoryVote = (
    Option<i64>,
    Option<i64>,
    i64,
    i64,
    Option<String>,
    Option<String>,
);

/// A story's new content. `cid` is the edited record's CID, when it has one.
#[derive(Debug, Clone, Copy)]
pub struct StoryEdit<'a> {
    pub title: &'a str,
    pub link: Option<&'a threaded_core::link::Link>,
    pub body: &'a str,
    pub tags: &'a [&'a str],
    pub cid: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct NewComment<'a> {
    pub story_id: i64,
    pub user_id: i64,
    pub parent_id: Option<i64>,
    pub body: &'a str,
    pub created_at: i64,
    pub record: Option<(&'a str, &'a str)>,
}

/// A published upvote's subject, for publishing past upvotes.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UpvoteRecord {
    pub vote_id: i64,
    pub subject_uri: Option<String>,
    pub subject_cid: Option<String>,
    pub at_uri: Option<String>,
}

pub(crate) fn target_cols(t: Target) -> (&'static str, i64) {
    match t {
        Target::Story(id) => ("story_id", id),
        Target::Comment(id) => ("comment_id", id),
    }
}

pub(crate) async fn recompute_rank(tx: &mut Transaction<'_, Sqlite>, story_id: i64) -> Result<()> {
    let (score, user_id, is_link, user_is_author, created_at): (i64, i64, bool, bool, i64) =
        sqlx::query_as("SELECT score, user_id, url IS NOT NULL, user_is_author, created_at FROM stories WHERE id = ?")
            .bind(story_id)
            .fetch_one(&mut **tx)
            .await?;
    let comment_points: f64 = sqlx::query_scalar(
        "SELECT CAST(coalesce(sum(score + 1), 0) AS REAL) FROM comments \
         WHERE story_id = ? AND user_id <> ? AND deleted_at IS NULL",
    )
    .bind(story_id)
    .bind(user_id)
    .fetch_one(&mut **tx)
    .await?;
    let tag_boost: f64 = sqlx::query_scalar(
        "SELECT CAST(coalesce(sum(t.rank_boost), 0) AS REAL) FROM taggings tg JOIN tags t ON t.id = tg.tag_id WHERE tg.story_id = ?",
    )
    .bind(story_id)
    .fetch_one(&mut **tx)
    .await?;
    let r = rank::compute(RankInput {
        score,
        comment_points,
        tag_boost,
        user_is_author,
        is_link,
        created_at,
    });
    sqlx::query("UPDATE stories SET rank = ? WHERE id = ?")
        .bind(r)
        .bind(story_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Karma is the sum of a user's item scores, less the implicit vote on each.
pub(crate) async fn recompute_karma(tx: &mut Transaction<'_, Sqlite>, user_id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE users SET karma = \
         (SELECT coalesce(sum(score - 1), 0) FROM stories WHERE user_id = ?1 AND deleted_at IS NULL) + \
         (SELECT coalesce(sum(score - 1), 0) FROM comments WHERE user_id = ?1 AND deleted_at IS NULL) \
         WHERE id = ?1",
    )
    .bind(user_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

impl Store {
    pub async fn user_by_did(&self, did: &str) -> Result<Option<UserRow>> {
        sqlx::query_as("SELECT id, did, handle, handle_verified, role, karma, public_votes, vouched_by, created_at, banned_at, banned_reason FROM users WHERE did = ?")
            .bind(did)
            .fetch_optional(&self.pool)
            .await
    }

    pub async fn user_by_handle(&self, handle: &str) -> Result<Option<UserRow>> {
        sqlx::query_as("SELECT id, did, handle, handle_verified, role, karma, public_votes, vouched_by, created_at, banned_at, banned_reason FROM users WHERE handle = ? OR did = ?1 LIMIT 1")
            .bind(handle)
            .fetch_optional(&self.pool)
            .await
    }

    pub async fn all_tags(&self) -> Result<Vec<TagRow>> {
        sqlx::query_as("SELECT id, name, description, category, rank_boost, privileged, new_users_ok, is_media, active FROM tags ORDER BY name")
            .fetch_all(&self.pool)
            .await
    }

    pub async fn stories_since(&self, user_id: i64, since: i64) -> Result<i64> {
        sqlx::query_scalar("SELECT count(*) FROM stories WHERE user_id = ? AND created_at >= ?")
            .bind(user_id)
            .bind(since)
            .fetch_one(&self.pool)
            .await
    }

    pub async fn comments_since(&self, user_id: i64, since: i64) -> Result<i64> {
        sqlx::query_scalar("SELECT count(*) FROM comments WHERE user_id = ? AND created_at >= ?")
            .bind(user_id)
            .bind(since)
            .fetch_one(&self.pool)
            .await
    }

    /// The short ID of a visible story with this canonical URL submitted since `since`.
    pub async fn duplicate_of(&self, canonical: &str, since: i64) -> Result<Option<String>> {
        sqlx::query_scalar(&format!(
            "SELECT s.short_id FROM stories s WHERE s.canonical_url = ? AND s.created_at >= ? AND {VISIBLE} \
             ORDER BY s.created_at LIMIT 1"
        ))
        .bind(canonical)
        .bind(since)
        .fetch_optional(&self.pool)
        .await
    }

    pub async fn comments_for_story(&self, story_id: i64) -> Result<Vec<CommentRow>> {
        sqlx::query_as(&format!(
            "SELECT {COMMENT_COLUMNS} FROM comments c JOIN users u ON u.id = c.user_id \
             JOIN stories s ON s.id = c.story_id WHERE c.story_id = ? ORDER BY c.created_at, c.id"
        ))
        .bind(story_id)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn comment(&self, short_id: &str) -> Result<Option<CommentRow>> {
        sqlx::query_as(&format!(
            "SELECT {COMMENT_COLUMNS} FROM comments c JOIN users u ON u.id = c.user_id \
             JOIN stories s ON s.id = c.story_id WHERE c.short_id = ?"
        ))
        .bind(short_id)
        .fetch_optional(&self.pool)
        .await
    }

    pub async fn comment_has_replies(&self, comment_id: i64) -> Result<bool> {
        sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM comments WHERE parent_id = ? AND deleted_at IS NULL)",
        )
        .bind(comment_id)
        .fetch_one(&self.pool)
        .await
    }

    pub async fn insert_comment(&self, c: &NewComment<'_>) -> Result<String> {
        let mut tx = self.pool.begin().await?;
        let (_, sid) = insert_comment_tx(&mut tx, c).await?;
        tx.commit().await?;
        Ok(sid)
    }

    pub async fn vote(&self, user_id: i64, target: Target) -> Result<Option<VoteRow>> {
        let (col, id) = target_cols(target);
        sqlx::query_as(&format!(
            "SELECT id, value, reason, at_uri FROM votes WHERE user_id = ? AND {col} = ?"
        ))
        .bind(user_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
    }

    /// Every vote a user holds on a story and its comments, keyed by target.
    pub async fn votes_on_story(
        &self,
        user_id: i64,
        story_id: i64,
    ) -> Result<Vec<(Target, VoteRow)>> {
        let rows: Vec<StoryVote> = sqlx::query_as(
            "SELECT v.story_id, v.comment_id, v.id, v.value, v.reason, v.at_uri FROM votes v \
             LEFT JOIN comments c ON c.id = v.comment_id \
             WHERE v.user_id = ? AND (v.story_id = ?2 OR c.story_id = ?2)",
        )
        .bind(user_id)
        .bind(story_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(s, c, id, value, reason, at_uri)| {
                let t = match (s, c) {
                    (Some(s), _) => Target::Story(s),
                    (_, Some(c)) => Target::Comment(c),
                    _ => return None,
                };
                Some((
                    t,
                    VoteRow {
                        id,
                        value,
                        reason,
                        at_uri,
                    },
                ))
            })
            .collect())
    }

    /// Upvotes a user holds on the given stories.
    pub async fn upvoted_stories(&self, user_id: i64, story_ids: &[i64]) -> Result<Vec<i64>> {
        if story_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut qb = sqlx::QueryBuilder::<Sqlite>::new(
            "SELECT story_id FROM votes WHERE value = 1 AND user_id = ",
        );
        qb.push_bind(user_id).push(" AND story_id IN (");
        let mut sep = qb.separated(", ");
        for id in story_ids {
            sep.push_bind(*id);
        }
        qb.push(")");
        qb.build_query_scalar().fetch_all(&self.pool).await
    }

    /// Applies a vote change and updates the target's score and flag count,
    /// the story's rank, and the author's karma. Returns the new score.
    pub async fn set_vote(
        &self,
        user_id: i64,
        target: Target,
        change: VoteChange<'_>,
        now: i64,
    ) -> Result<i64> {
        let mut tx = self.pool.begin().await?;
        let (col, id) = target_cols(target);
        let old: Option<(i64, i64)> = sqlx::query_as(&format!(
            "SELECT id, value FROM votes WHERE user_id = ? AND {col} = ?"
        ))
        .bind(user_id)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let (old_value, old_id) = match old {
            Some((vid, v)) => (v, Some(vid)),
            None => (0, None),
        };
        let new_value = match change {
            VoteChange::Up { .. } => 1,
            VoteChange::Flag { .. } => -1,
            VoteChange::Clear => 0,
        };
        if let Some(vid) = old_id {
            sqlx::query("DELETE FROM votes WHERE id = ?")
                .bind(vid)
                .execute(&mut *tx)
                .await?;
        }
        match change {
            VoteChange::Up { at_uri } => {
                sqlx::query(&format!("INSERT INTO votes (user_id, {col}, value, at_uri, created_at) VALUES (?, ?, 1, ?, ?)"))
                    .bind(user_id)
                    .bind(id)
                    .bind(at_uri)
                    .bind(now)
                    .execute(&mut *tx)
                    .await?;
            }
            VoteChange::Flag { reason } => {
                sqlx::query(&format!("INSERT INTO votes (user_id, {col}, value, reason, created_at) VALUES (?, ?, -1, ?, ?)"))
                    .bind(user_id)
                    .bind(id)
                    .bind(reason)
                    .bind(now)
                    .execute(&mut *tx)
                    .await?;
            }
            VoteChange::Clear => {}
        }
        let flag_delta = i64::from(new_value == -1) - i64::from(old_value == -1);
        let table = match target {
            Target::Story(_) => "stories",
            Target::Comment(_) => "comments",
        };
        sqlx::query(&format!(
            "UPDATE {table} SET score = score + ?, flags = flags + ? WHERE id = ?"
        ))
        .bind(new_value - old_value)
        .bind(flag_delta)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        let (score, author, story_id): (i64, i64, i64) = match target {
            Target::Story(s) => {
                sqlx::query_as("SELECT score, user_id, id FROM stories WHERE id = ?")
                    .bind(s)
                    .fetch_one(&mut *tx)
                    .await?
            }
            Target::Comment(c) => {
                sqlx::query_as("SELECT score, user_id, story_id FROM comments WHERE id = ?")
                    .bind(c)
                    .fetch_one(&mut *tx)
                    .await?
            }
        };
        recompute_rank(&mut tx, story_id).await?;
        recompute_karma(&mut tx, author).await?;
        tx.commit().await?;
        Ok(score)
    }

    /// Replaces a story's title, body, tags, and (within the edit window) URL,
    /// keeping the prior version in `story_revisions`.
    pub async fn edit_story(&self, story: &StoryRow, e: &StoryEdit<'_>, now: i64) -> Result<()> {
        let StoryEdit {
            title,
            link,
            body,
            tags,
            cid,
        } = *e;
        let mut tx = self.pool.begin().await?;
        sqlx::query("INSERT INTO story_revisions (story_id, title, url, body, tags, at_cid, edited_at) VALUES (?, ?, ?, ?, ?, ?, ?)")
            .bind(story.id)
            .bind(&story.title)
            .bind(&story.url)
            .bind(&story.body)
            .bind(&story.tags)
            .bind(&story.at_cid)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        if story.mod_edited {
            // A moderator's title, URL, and tags stay in force; the author's
            // own values change underneath them.
            sqlx::query(
                "UPDATE stories SET author_title = ?, author_url = ?, author_tags = ?, body = ?, \
                 at_cid = coalesce(?, at_cid), edited_at = ? WHERE id = ?",
            )
            .bind(title)
            .bind(link.map(|l| l.url.as_str()))
            .bind(tags.join(" "))
            .bind(body)
            .bind(cid)
            .bind(now)
            .bind(story.id)
            .execute(&mut *tx)
            .await?;
            return tx.commit().await;
        }
        sqlx::query(
            "UPDATE stories SET title = ?, url = ?, canonical_url = ?, domain = ?, body = ?, \
             at_cid = coalesce(?, at_cid), edited_at = ? WHERE id = ?",
        )
        .bind(title)
        .bind(link.map(|l| l.url.as_str()))
        .bind(link.map(|l| l.canonical.as_str()))
        .bind(link.map(|l| l.domain.as_str()))
        .bind(body)
        .bind(cid)
        .bind(now)
        .bind(story.id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM taggings WHERE story_id = ?")
            .bind(story.id)
            .execute(&mut *tx)
            .await?;
        for name in tags {
            sqlx::query("INSERT INTO taggings (story_id, tag_id) SELECT ?, id FROM tags WHERE name = ? AND active = 1")
                .bind(story.id)
                .bind(name)
                .execute(&mut *tx)
                .await?;
        }
        recompute_rank(&mut tx, story.id).await?;
        tx.commit().await
    }

    pub async fn edit_comment(
        &self,
        comment: &CommentRow,
        body: &str,
        cid: Option<&str>,
        now: i64,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("INSERT INTO comment_revisions (comment_id, body, at_cid, edited_at) VALUES (?, ?, ?, ?)")
            .bind(comment.id)
            .bind(&comment.body)
            .bind(&comment.at_cid)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE comments SET body = ?, at_cid = coalesce(?, at_cid), edited_at = ? WHERE id = ?")
            .bind(body)
            .bind(cid)
            .bind(now)
            .bind(comment.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await
    }

    /// The author's own deletion.
    pub async fn delete_story(&self, story_id: i64, user_id: i64, now: i64) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE stories SET deleted_at = ?, deleted_by = ?, deleted_reason = 'Deleted by its author' WHERE id = ?")
            .bind(now)
            .bind(user_id)
            .bind(story_id)
            .execute(&mut *tx)
            .await?;
        recompute_karma(&mut tx, user_id).await?;
        tx.commit().await
    }

    pub async fn delete_comment(&self, comment: &CommentRow, now: i64) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE comments SET deleted_at = ?, deleted_by = ?, deleted_reason = 'Deleted by its author' WHERE id = ?")
            .bind(now)
            .bind(comment.user_id)
            .bind(comment.id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE stories SET comment_count = max(comment_count - 1, 0) WHERE id = ?")
            .bind(comment.story_id)
            .execute(&mut *tx)
            .await?;
        recompute_rank(&mut tx, comment.story_id).await?;
        recompute_karma(&mut tx, comment.user_id).await?;
        tx.commit().await
    }

    pub async fn set_public_votes(&self, user_id: i64, on: bool) -> Result<()> {
        sqlx::query("UPDATE users SET public_votes = ? WHERE id = ?")
            .bind(on)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// The user's upvotes with their subjects' records, for publishing or
    /// withdrawing public upvotes.
    pub async fn upvote_records(&self, user_id: i64) -> Result<Vec<UpvoteRecord>> {
        sqlx::query_as(
            "SELECT v.id AS vote_id, coalesce(s.at_uri, c.at_uri) AS subject_uri, \
             coalesce(s.at_cid, c.at_cid) AS subject_cid, v.at_uri \
             FROM votes v LEFT JOIN stories s ON s.id = v.story_id LEFT JOIN comments c ON c.id = v.comment_id \
             WHERE v.user_id = ? AND v.value = 1 ORDER BY v.created_at",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn set_vote_record(&self, vote_id: i64, at_uri: Option<&str>) -> Result<()> {
        sqlx::query("UPDATE votes SET at_uri = ? WHERE id = ?")
            .bind(at_uri)
            .bind(vote_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn stories_by_user(&self, user_id: i64, limit: i64) -> Result<Vec<StoryRow>> {
        sqlx::query_as(&format!(
            "SELECT {STORY_COLUMNS} FROM stories s JOIN users u ON u.id = s.user_id \
             WHERE s.user_id = ? AND {VISIBLE} ORDER BY s.created_at DESC LIMIT ?"
        ))
        .bind(user_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn comments_by_user(&self, user_id: i64, limit: i64) -> Result<Vec<CommentRow>> {
        sqlx::query_as(&format!(
            "SELECT {COMMENT_COLUMNS} FROM comments c JOIN users u ON u.id = c.user_id \
             JOIN stories s ON s.id = c.story_id \
             WHERE c.user_id = ? AND c.deleted_at IS NULL AND c.removed_at IS NULL AND c.held_at IS NULL \
             AND {VISIBLE} ORDER BY c.created_at DESC LIMIT ?"
        ))
        .bind(user_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
    }

    /// Stories a user upvoted, newest vote first.
    pub async fn liked_stories(&self, user_id: i64, limit: i64) -> Result<Vec<StoryRow>> {
        sqlx::query_as(&format!(
            "SELECT {STORY_COLUMNS} FROM votes v JOIN stories s ON s.id = v.story_id \
             JOIN users u ON u.id = s.user_id \
             WHERE v.user_id = ? AND v.value = 1 AND {VISIBLE} ORDER BY v.created_at DESC LIMIT ?"
        ))
        .bind(user_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
    }

    /// Flag reasons on an item with their counts, for its author.
    pub async fn flag_reasons(&self, target: Target) -> Result<Vec<(String, i64)>> {
        let (col, id) = target_cols(target);
        sqlx::query_as(&format!(
            "SELECT reason, count(*) FROM votes WHERE {col} = ? AND value = -1 GROUP BY reason ORDER BY count(*) DESC, reason"
        ))
        .bind(id)
        .fetch_all(&self.pool)
        .await
    }
}

/// Inserts a comment inside the caller's transaction. Returns its ID and short ID.
pub(crate) async fn insert_comment_tx(
    tx: &mut Transaction<'_, Sqlite>,
    c: &NewComment<'_>,
) -> Result<(i64, String)> {
    let depth: i64 = match c.parent_id {
        Some(p) => {
            sqlx::query_scalar::<_, i64>(
                "SELECT depth + 1 FROM comments WHERE id = ? AND story_id = ?",
            )
            .bind(p)
            .bind(c.story_id)
            .fetch_one(&mut **tx)
            .await?
        }
        None => 0,
    };
    let sid = loop {
        let candidate = short_id();
        let taken: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM comments WHERE short_id = ?)")
                .bind(&candidate)
                .fetch_one(&mut **tx)
                .await?;
        if !taken {
            break candidate;
        }
    };
    let comment_id: i64 = sqlx::query_scalar(
        "INSERT INTO comments (short_id, story_id, user_id, parent_id, depth, body, created_at, at_uri, at_cid) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(&sid)
    .bind(c.story_id)
    .bind(c.user_id)
    .bind(c.parent_id)
    .bind(depth)
    .bind(c.body)
    .bind(c.created_at)
    .bind(c.record.map(|r| r.0))
    .bind(c.record.map(|r| r.1))
    .fetch_one(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE stories SET comment_count = comment_count + 1, last_comment_at = ? WHERE id = ?",
    )
    .bind(c.created_at)
    .bind(c.story_id)
    .execute(&mut **tx)
    .await?;
    recompute_rank(tx, c.story_id).await?;
    Ok((comment_id, sid))
}
