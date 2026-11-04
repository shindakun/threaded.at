//! Bluesky blocks, read from each account's repo. A block applies both
//! ways: while signed in, neither account sees the other's stories or
//! comments, and neither can reply to the other. Signed-out readers and
//! everyone else see everything.

use std::collections::HashSet;

use sqlx::{QueryBuilder, Sqlite};

use crate::{Result, Store};

/// SQL true when users `a` and `b`, two `users.id` expressions, block each
/// other in either direction.
pub(crate) fn blocked_sql(a: &str, b: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM users ba JOIN users bb ON bb.id = {b} JOIN blocks bk \
         ON (bk.blocker_did = ba.did AND bk.subject_did = bb.did) \
         OR (bk.blocker_did = bb.did AND bk.subject_did = ba.did) \
         WHERE ba.id = {a})"
    )
}

/// Adds `AND NOT` the block test between a story's author and a bound reader.
pub(crate) fn push_not_blocked(qb: &mut QueryBuilder<'_, Sqlite>, author: &str, reader: i64) {
    qb.push(format!(
        " AND NOT EXISTS (SELECT 1 FROM users ba JOIN users bb ON bb.id = {author} JOIN blocks bk \
         ON (bk.blocker_did = ba.did AND bk.subject_did = bb.did) \
         OR (bk.blocker_did = bb.did AND bk.subject_did = ba.did) WHERE ba.id = "
    ))
    .push_bind(reader)
    .push(")");
}

/// One block record: its record key and the account it blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub rkey: String,
    pub subject_did: String,
}

impl Store {
    /// Replaces every block an account holds with what its repo holds now,
    /// and marks the read finished.
    pub async fn replace_blocks(&self, did: &str, blocks: &[Block], now: i64) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM blocks WHERE blocker_did = ?")
            .bind(did)
            .execute(&mut *tx)
            .await?;
        for b in blocks {
            sqlx::query(
                "INSERT OR REPLACE INTO blocks (blocker_did, rkey, subject_did) VALUES (?, ?, ?)",
            )
            .bind(did)
            .bind(&b.rkey)
            .bind(&b.subject_did)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("UPDATE users SET blocks_read_at = ?, blocks_tried_at = ? WHERE did = ?")
            .bind(now)
            .bind(now)
            .bind(did)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Notes a read that did not finish, so the refresh waits before trying
    /// again; the blocks already held stay.
    pub async fn blocks_tried(&self, did: &str, now: i64) -> Result<()> {
        sqlx::query("UPDATE users SET blocks_tried_at = ? WHERE did = ?")
            .bind(now)
            .bind(did)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// How many blocks an account holds, as last read.
    pub async fn block_count(&self, did: &str) -> Result<i64> {
        sqlx::query_scalar("SELECT count(*) FROM blocks WHERE blocker_did = ?")
            .bind(did)
            .fetch_one(&self.pool)
            .await
    }

    /// When an account's blocks were last read in full.
    pub async fn blocks_read_at(&self, user_id: i64) -> Result<Option<i64>> {
        sqlx::query_scalar("SELECT blocks_read_at FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_one(&self.pool)
            .await
    }

    /// Accounts whose blocks are due a read, longest waiting first: every
    /// `active_every` seconds for an account seen since `active_since`, and
    /// every `idle_every` for the rest, including accounts known only from
    /// the network.
    pub async fn blocks_due(
        &self,
        now: i64,
        active_since: i64,
        active_every: i64,
        idle_every: i64,
        limit: i64,
    ) -> Result<Vec<String>> {
        sqlx::query_scalar(
            "SELECT did FROM users WHERE blocks_tried_at IS NULL \
             OR blocks_tried_at <= ?1 - CASE WHEN last_seen_at >= ?2 THEN ?3 ELSE ?4 END \
             ORDER BY coalesce(blocks_tried_at, 0), id LIMIT ?5",
        )
        .bind(now)
        .bind(active_since)
        .bind(active_every)
        .bind(idle_every)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
    }

    /// Whether two users block each other in either direction.
    pub async fn blocked_between(&self, a: i64, b: i64) -> Result<bool> {
        sqlx::query_scalar(&format!("SELECT {}", blocked_sql("?1", "?2")))
            .bind(a)
            .bind(b)
            .fetch_one(&self.pool)
            .await
    }

    /// Which of these users block, or are blocked by, the reader.
    pub async fn blocked_among(&self, reader: i64, users: &[i64]) -> Result<HashSet<i64>> {
        if users.is_empty() {
            return Ok(HashSet::new());
        }
        let mut qb = QueryBuilder::<Sqlite>::new("SELECT u.id FROM users u WHERE u.id IN (");
        let mut sep = qb.separated(", ");
        for id in users {
            sep.push_bind(*id);
        }
        sep.push_unseparated(")");
        push_blocked_with(&mut qb, reader);
        let ids: Vec<i64> = qb.build_query_scalar().fetch_all(&self.pool).await?;
        Ok(ids.into_iter().collect())
    }
}

/// `AND` the block test between `u.id` and a bound reader.
fn push_blocked_with(qb: &mut QueryBuilder<'_, Sqlite>, reader: i64) {
    qb.push(
        " AND EXISTS (SELECT 1 FROM users ba JOIN users bb ON bb.id = u.id JOIN blocks bk \
         ON (bk.blocker_did = ba.did AND bk.subject_did = bb.did) \
         OR (bk.blocker_did = bb.did AND bk.subject_did = ba.did) WHERE ba.id = ",
    )
    .push_bind(reader)
    .push(")");
}
