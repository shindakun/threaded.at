//! What one reader keeps for themselves: saved stories, hidden stories,
//! filtered tags, and when they last read each story. All of it is private
//! and lives only in this database; none of it is a record.

use sqlx::{QueryBuilder, Sqlite};

use crate::{Result, Store};

/// Saved and hidden marks a reader holds on some stories.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Marks {
    pub saved: Vec<i64>,
    pub hidden: Vec<i64>,
}

impl Store {
    /// Saves a story for a reader, or takes the save away. Saving twice
    /// keeps the first time.
    pub async fn set_saved(&self, user_id: i64, story_id: i64, on: bool, now: i64) -> Result<()> {
        set_mark(self, "saved_stories", user_id, story_id, on, now).await
    }

    /// Hides a story from a reader's listings, or brings it back.
    pub async fn set_hidden(&self, user_id: i64, story_id: i64, on: bool, now: i64) -> Result<()> {
        set_mark(self, "hidden_stories", user_id, story_id, on, now).await
    }

    /// Which of these stories the reader saved or hid.
    pub async fn marks(&self, user_id: i64, story_ids: &[i64]) -> Result<Marks> {
        if story_ids.is_empty() {
            return Ok(Marks::default());
        }
        let mut qb = QueryBuilder::<Sqlite>::new(
            "SELECT story_id, 'saved' FROM saved_stories WHERE user_id = ",
        );
        qb.push_bind(user_id);
        push_ids(&mut qb, story_ids);
        qb.push(" UNION ALL SELECT story_id, 'hidden' FROM hidden_stories WHERE user_id = ")
            .push_bind(user_id);
        push_ids(&mut qb, story_ids);
        let rows: Vec<(i64, String)> = qb.build_query_as().fetch_all(self.pool()).await?;
        let mut m = Marks::default();
        for (id, kind) in rows {
            if kind == "saved" {
                m.saved.push(id);
            } else {
                m.hidden.push(id);
            }
        }
        Ok(m)
    }

    /// Records that a reader opened a story page.
    pub async fn record_read(&self, user_id: i64, story_id: i64, now: i64) -> Result<()> {
        sqlx::query(
            "INSERT INTO story_reads (user_id, story_id, read_at) VALUES (?1, ?2, ?3) \
             ON CONFLICT(user_id, story_id) DO UPDATE SET read_at = max(read_at, ?3)",
        )
        .bind(user_id)
        .bind(story_id)
        .bind(now)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// For each of these stories the reader has opened, the comments written
    /// since, counting only comments the reader can see and not their own.
    /// Stories the reader never opened, or with nothing new, are left out.
    pub async fn new_comments(&self, user_id: i64, story_ids: &[i64]) -> Result<Vec<(i64, i64)>> {
        if story_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut qb = QueryBuilder::<Sqlite>::new(
            "SELECT r.story_id, count(*) FROM story_reads r \
             JOIN comments c ON c.story_id = r.story_id AND c.created_at > r.read_at \
             JOIN users u ON u.id = c.user_id \
             WHERE c.deleted_at IS NULL AND c.removed_at IS NULL AND c.held_at IS NULL \
             AND u.inactive_at IS NULL AND c.user_id <> r.user_id AND r.user_id = ",
        );
        qb.push_bind(user_id).push(" AND r.story_id IN (");
        let mut sep = qb.separated(", ");
        for id in story_ids {
            sep.push_bind(*id);
        }
        sep.push_unseparated(") GROUP BY r.story_id");
        qb.build_query_as().fetch_all(self.pool()).await
    }

    /// Drops reads of stories created before `cutoff`: they are past the
    /// lock age and take no new comments. Returns how many were dropped.
    pub async fn prune_story_reads(&self, cutoff: i64) -> Result<u64> {
        let r = sqlx::query(
            "DELETE FROM story_reads WHERE story_id IN (SELECT id FROM stories WHERE created_at < ?)",
        )
        .bind(cutoff)
        .execute(self.pool())
        .await?;
        Ok(r.rows_affected())
    }

    /// The tags a reader filters out, by name.
    pub async fn tag_filters(&self, user_id: i64) -> Result<Vec<String>> {
        sqlx::query_scalar(
            "SELECT t.name FROM tag_filters f JOIN tags t ON t.id = f.tag_id \
             WHERE f.user_id = ? ORDER BY t.name",
        )
        .bind(user_id)
        .fetch_all(self.pool())
        .await
    }

    pub async fn set_keyboard_shortcuts(&self, user_id: i64, on: bool) -> Result<()> {
        sqlx::query("UPDATE users SET keyboard_shortcuts = ? WHERE id = ?")
            .bind(on)
            .bind(user_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Replaces a reader's tag filters. Names that are not active tags are
    /// skipped; the caller checks them first to say so.
    pub async fn set_tag_filters(&self, user_id: i64, names: &[String]) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM tag_filters WHERE user_id = ?")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        for name in names {
            sqlx::query(
                "INSERT OR IGNORE INTO tag_filters (user_id, tag_id) \
                 SELECT ?, id FROM tags WHERE name = ? AND active = 1",
            )
            .bind(user_id)
            .bind(name)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await
    }
}

async fn set_mark(
    store: &Store,
    table: &'static str,
    user_id: i64,
    story_id: i64,
    on: bool,
    now: i64,
) -> Result<()> {
    // `table` is one of two constants named by the callers above.
    let q = if on {
        format!(
            "INSERT INTO {table} (user_id, story_id, created_at) VALUES (?, ?, ?) \
             ON CONFLICT(user_id, story_id) DO NOTHING"
        )
    } else {
        format!("DELETE FROM {table} WHERE user_id = ? AND story_id = ?")
    };
    let mut query = sqlx::query(&q).bind(user_id).bind(story_id);
    if on {
        query = query.bind(now);
    }
    query.execute(store.pool()).await?;
    Ok(())
}

fn push_ids(qb: &mut QueryBuilder<'_, Sqlite>, ids: &[i64]) {
    qb.push(" AND story_id IN (");
    let mut sep = qb.separated(", ");
    for id in ids {
        sep.push_bind(*id);
    }
    sep.push_unseparated(")");
}

/// Leaves out the stories a reader hid and those carrying a tag they filter.
/// `shown` names tags the reader asked for on this page, such as the tag of
/// `/t/{tag}`: a story is not filtered out for carrying one of those.
pub(crate) fn push_reader(qb: &mut QueryBuilder<'_, Sqlite>, reader: i64, shown: &[String]) {
    qb.push(
        " AND NOT EXISTS (SELECT 1 FROM hidden_stories h WHERE h.story_id = s.id AND h.user_id = ",
    )
    .push_bind(reader)
    .push(")");
    qb.push(
        " AND NOT EXISTS (SELECT 1 FROM taggings fg JOIN tag_filters tf ON tf.tag_id = fg.tag_id \
         JOIN tags ft ON ft.id = fg.tag_id WHERE fg.story_id = s.id AND tf.user_id = ",
    )
    .push_bind(reader);
    for name in shown {
        qb.push(" AND ft.name <> ").push_bind(name.clone());
    }
    qb.push(")");
}
