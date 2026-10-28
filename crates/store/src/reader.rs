//! What one reader keeps for themselves: saved stories, hidden stories, and
//! filtered tags. All of it is private and lives only in this database; none
//! of it is a record.

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
