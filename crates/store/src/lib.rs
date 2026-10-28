use std::str::FromStr;

use rand::Rng;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{QueryBuilder, Sqlite, SqlitePool};
use threaded_core::rank::{self, RankInput};
use threaded_core::search::{Query, Sort};

mod accept;
mod crosspost;
mod handles;
mod inbox;
mod m2;
mod moderation;
mod network;
mod reader;
mod triage;
pub use accept::*;
pub use crosspost::*;
pub use handles::*;
pub use m2::*;
pub use moderation::*;
pub use network::*;
pub use reader::*;
pub use triage::*;

pub use sqlx::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listing {
    Front,
    New,
    Active,
    /// Highest score among stories newer than the cutoff (unix seconds).
    Top {
        since: i64,
    },
    /// Stories a reader saved, newest save first.
    Saved {
        user_id: i64,
    },
    /// Stories a reader hid, newest first, so they can bring them back.
    Hidden {
        user_id: i64,
    },
}

#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub tag: Option<String>,
    pub domain: Option<String>,
    pub submitter: Option<String>,
    /// The signed-in reader. Their hidden stories and filtered tags leave
    /// the public listings; a filter on `tag` itself does not apply.
    pub reader: Option<i64>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StoryRow {
    pub id: i64,
    pub short_id: String,
    pub title: String,
    pub url: Option<String>,
    pub domain: Option<String>,
    pub score: i64,
    pub comment_count: i64,
    pub created_at: i64,
    pub handle: String,
    /// Space-separated tag names, sorted.
    pub tags: String,
    pub body: String,
    pub user_id: i64,
    pub author_did: String,
    pub user_is_author: bool,
    pub flags: i64,
    pub edited_at: Option<i64>,
    pub locked: bool,
    pub at_uri: Option<String>,
    pub at_cid: Option<String>,
    pub removed_at: Option<i64>,
    pub removed_reason: Option<String>,
    pub held_at: Option<i64>,
    /// The short ID of the story this one was merged into.
    pub merged_into: Option<String>,
    pub locked_reason: Option<String>,
    /// A moderator's edit is in force.
    pub mod_edited: bool,
    /// The link changed after the edit window; hidden for moderators.
    pub detached_at: Option<i64>,
    /// The network reports the author's account inactive.
    pub author_inactive: bool,
}

impl StoryRow {
    pub fn tag_list(&self) -> Vec<&str> {
        self.tags.split_whitespace().collect()
    }

    /// Removed, held, merged, or detached: hidden from listings and readers.
    pub fn hidden(&self) -> bool {
        self.removed_at.is_some()
            || self.held_at.is_some()
            || self.merged_into.is_some()
            || self.detached_at.is_some()
            || self.author_inactive
    }
}

#[derive(Debug, Clone)]
pub struct NewStory<'a> {
    pub user_id: i64,
    pub title: &'a str,
    pub link: Option<&'a threaded_core::link::Link>,
    pub body: &'a str,
    pub user_is_author: bool,
    pub tags: &'a [&'a str],
    pub created_at: i64,
    /// The record's AT-URI and CID, when the story was written to a repo.
    pub record: Option<(&'a str, &'a str)>,
}

pub(crate) const STORY_COLUMNS: &str =
    "s.id, s.short_id, s.title, s.url, s.domain, s.score, s.comment_count, \
     s.created_at, CASE WHEN u.handle_verified THEN u.handle ELSE u.did END AS handle, s.body, s.user_id, u.did AS author_did, s.user_is_author, s.flags, \
     s.edited_at, s.locked, s.at_uri, s.at_cid, s.removed_at, s.removed_reason, s.held_at, \
     (SELECT m.short_id FROM stories m WHERE m.id = s.merged_into) AS merged_into, \
     s.locked_reason, s.override_at IS NOT NULL AS mod_edited, s.detached_at, \
     u.inactive_at IS NOT NULL AS author_inactive, \
     coalesce((SELECT group_concat(name, ' ') FROM \
         (SELECT t.name FROM taggings tg JOIN tags t ON t.id = tg.tag_id \
          WHERE tg.story_id = s.id ORDER BY t.name)), '') AS tags";

/// Stories everyone may see: not deleted, merged, removed, held, or
/// detached, by an account the network reports active.
pub(crate) const VISIBLE: &str = "s.deleted_at IS NULL AND s.merged_into IS NULL \
     AND s.removed_at IS NULL AND s.held_at IS NULL AND s.detached_at IS NULL \
     AND NOT EXISTS (SELECT 1 FROM users x WHERE x.id = s.user_id AND x.inactive_at IS NOT NULL)";

impl Store {
    pub async fn open(path: &str) -> Result<Store> {
        let opts = SqliteConnectOptions::from_str(path)?
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true);
        let max = if path.contains(":memory:") { 1 } else { 8 };
        let pool = SqlitePoolOptions::new()
            .max_connections(max)
            .connect_with(opts)
            .await?;
        sqlx::migrate!("../../migrations").run(&pool).await?;
        Ok(Store { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn upsert_user(&self, did: &str, handle: &str, now: i64) -> Result<i64> {
        sqlx::query_scalar(
            "INSERT INTO users (did, handle, created_at, last_seen_at) VALUES (?1, ?2, ?3, ?3) \
             ON CONFLICT(did) DO UPDATE SET handle = excluded.handle, last_seen_at = excluded.last_seen_at \
             RETURNING id",
        )
        .bind(did)
        .bind(handle)
        .bind(now)
        .fetch_one(&self.pool)
        .await
    }

    /// Records a sign-in. `handle` is the verified handle, or the DID when the
    /// handle does not verify.
    pub async fn record_sign_in(
        &self,
        did: &str,
        handle: &str,
        verified: bool,
        now: i64,
    ) -> Result<i64> {
        let mut tx = self.pool.begin().await?;
        let id = sqlx::query_scalar(
            "INSERT INTO users (did, handle, handle_verified, created_at, last_seen_at, handle_checked_at) \
             VALUES (?1, ?2, ?3, ?4, ?4, ?4) \
             ON CONFLICT(did) DO UPDATE SET handle = excluded.handle, handle_verified = excluded.handle_verified, \
             last_seen_at = excluded.last_seen_at, handle_checked_at = excluded.handle_checked_at RETURNING id",
        )
        .bind(did)
        .bind(handle)
        .bind(verified)
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
        claim_handle(&mut tx, did, handle, verified, now).await?;
        tx.commit().await?;
        Ok(id)
    }

    /// The name to show for a signed-in DID: the verified handle, or the DID.
    pub async fn display_name(&self, did: &str) -> Result<Option<String>> {
        let row: Option<(String, bool)> =
            sqlx::query_as("SELECT handle, handle_verified FROM users WHERE did = ?")
                .bind(did)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|(h, ok)| if ok { h } else { did.to_string() }))
    }

    pub async fn upsert_tag(&self, name: &str, description: &str, rank_boost: f64) -> Result<i64> {
        sqlx::query_scalar(
            "INSERT INTO tags (name, description, rank_boost) VALUES (?1, ?2, ?3) \
             ON CONFLICT(name) DO UPDATE SET description = excluded.description, \
             rank_boost = excluded.rank_boost RETURNING id",
        )
        .bind(name)
        .bind(description)
        .bind(rank_boost)
        .fetch_one(&self.pool)
        .await
    }

    pub async fn tags(&self) -> Result<Vec<(String, String)>> {
        sqlx::query_as("SELECT name, description FROM tags WHERE active = 1 ORDER BY name")
            .fetch_all(&self.pool)
            .await
    }

    /// Tags must already exist. Returns the new story's short id.
    pub async fn insert_story(&self, s: &NewStory<'_>) -> Result<String> {
        let mut tx = self.pool.begin().await?;
        let (_, short_id) = insert_story_tx(&mut tx, s).await?;
        tx.commit().await?;
        Ok(short_id)
    }

    /// A story that is not deleted, whatever its moderation state. The story
    /// page decides who may see it.
    pub async fn story_any(&self, short_id: &str) -> Result<Option<StoryRow>> {
        sqlx::query_as(&format!(
            "SELECT {STORY_COLUMNS} FROM stories s JOIN users u ON u.id = s.user_id \
             WHERE s.short_id = ? AND s.deleted_at IS NULL"
        ))
        .bind(short_id)
        .fetch_optional(&self.pool)
        .await
    }

    pub async fn story(&self, short_id: &str) -> Result<Option<StoryRow>> {
        sqlx::query_as(&format!(
            "SELECT {STORY_COLUMNS} FROM stories s JOIN users u ON u.id = s.user_id \
             WHERE s.short_id = ? AND {VISIBLE}"
        ))
        .bind(short_id)
        .fetch_optional(&self.pool)
        .await
    }

    pub async fn list(
        &self,
        listing: Listing,
        f: &Filter,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<StoryRow>> {
        let mut qb = QueryBuilder::<Sqlite>::new(format!(
            "SELECT {STORY_COLUMNS} FROM stories s JOIN users u ON u.id = s.user_id WHERE {VISIBLE}"
        ));
        if let Some(tag) = &f.tag {
            push_has_tag(&mut qb, tag, true);
        }
        if let Some(d) = &f.domain {
            push_domain(&mut qb, d, true);
        }
        if let Some(h) = &f.submitter {
            push_user(&mut qb, "u", h);
        }
        let public = !matches!(listing, Listing::Saved { .. } | Listing::Hidden { .. });
        if let (true, Some(reader)) = (public, f.reader) {
            let shown: Vec<String> = f.tag.iter().cloned().collect();
            reader::push_reader(&mut qb, reader, &shown);
        }
        match listing {
            Listing::Front => qb.push(" ORDER BY s.rank DESC"),
            Listing::New => qb.push(" ORDER BY s.created_at DESC"),
            Listing::Active => {
                qb.push(" AND s.last_comment_at IS NOT NULL ORDER BY s.last_comment_at DESC")
            }
            Listing::Top { since } => qb
                .push(" AND s.created_at >= ")
                .push_bind(since)
                .push(" ORDER BY s.score DESC, s.created_at DESC"),
            Listing::Saved { user_id } => push_marked(&mut qb, "saved_stories", user_id),
            Listing::Hidden { user_id } => push_marked(&mut qb, "hidden_stories", user_id),
        };
        qb.push(" LIMIT ")
            .push_bind(limit)
            .push(" OFFSET ")
            .push_bind(offset);
        qb.build_query_as().fetch_all(&self.pool).await
    }

    /// Searches visible stories. A `reader`'s hidden stories and filtered
    /// tags are left out, except tags the query asks for with `tag:`.
    pub async fn search(
        &self,
        q: &Query,
        reader: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<StoryRow>> {
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let fts = q.fts();
        let mut qb = QueryBuilder::<Sqlite>::new(format!("SELECT {STORY_COLUMNS} FROM stories s "));
        if fts.is_some() {
            qb.push("JOIN stories_fts ON stories_fts.rowid = s.id ");
        }
        qb.push(format!("JOIN users u ON u.id = s.user_id WHERE {VISIBLE}"));
        if let Some(m) = &fts {
            qb.push(" AND stories_fts MATCH ").push_bind(m.clone());
        }
        for t in &q.tags {
            push_has_tag(&mut qb, t, true);
        }
        for t in &q.not_tags {
            push_has_tag(&mut qb, t, false);
        }
        for d in &q.domains {
            push_domain(&mut qb, d, true);
        }
        for d in &q.not_domains {
            push_domain(&mut qb, d, false);
        }
        if let Some(h) = &q.submitter {
            push_user(&mut qb, "u", h);
        }
        if let Some(h) = &q.commenter {
            qb.push(
                " AND EXISTS (SELECT 1 FROM comments c JOIN users cu ON cu.id = c.user_id \
                 WHERE c.story_id = s.id AND c.deleted_at IS NULL",
            );
            push_user(&mut qb, "cu", h);
            qb.push(")");
        }
        if let Some(reader) = reader {
            reader::push_reader(&mut qb, reader, &q.tags);
        }
        match (q.sort, fts.is_some()) {
            (Sort::Relevance, true) => qb.push(" ORDER BY bm25(stories_fts, 10.0, 1.0, 2.0, 2.0)"),
            (Sort::Score, _) => qb.push(" ORDER BY s.score DESC, s.created_at DESC"),
            _ => qb.push(" ORDER BY s.created_at DESC"),
        };
        qb.push(" LIMIT ")
            .push_bind(limit)
            .push(" OFFSET ")
            .push_bind(offset);
        qb.build_query_as().fetch_all(&self.pool).await
    }
}

/// Matches a user by DID, or by handle only while it is verified: a handle can
/// pass to another account, so an unverified one names nobody.
fn push_user(qb: &mut QueryBuilder<'_, Sqlite>, alias: &str, who: &str) {
    qb.push(format!(" AND ({alias}.did = "))
        .push_bind(who.to_string())
        .push(format!(
            " OR ({alias}.handle_verified AND {alias}.handle = "
        ))
        .push_bind(who.to_string())
        .push("))");
}

/// Stories in a reader's saved or hidden list, newest mark first.
/// `table` is one of the two constants named in `Store::list`.
fn push_marked<'a, 'q>(
    qb: &'a mut QueryBuilder<'q, Sqlite>,
    table: &'static str,
    user_id: i64,
) -> &'a mut QueryBuilder<'q, Sqlite> {
    qb.push(format!(
        " AND EXISTS (SELECT 1 FROM {table} x WHERE x.story_id = s.id AND x.user_id = "
    ))
    .push_bind(user_id)
    .push(format!(
        ") ORDER BY (SELECT x.created_at FROM {table} x WHERE x.story_id = s.id AND x.user_id = "
    ))
    .push_bind(user_id)
    .push(") DESC, s.id DESC")
}

fn push_has_tag(qb: &mut QueryBuilder<'_, Sqlite>, tag: &str, has: bool) {
    qb.push(if has {
        " AND EXISTS"
    } else {
        " AND NOT EXISTS"
    })
    .push(
        " (SELECT 1 FROM taggings tg JOIN tags t ON t.id = tg.tag_id \
             WHERE tg.story_id = s.id AND t.name = ",
    )
    .push_bind(tag.to_string())
    .push(")");
}

/// Matches the domain and its subdomains.
fn push_domain(qb: &mut QueryBuilder<'_, Sqlite>, domain: &str, has: bool) {
    qb.push(if has { " AND (" } else { " AND NOT (" })
        .push("coalesce(s.domain, '') = ")
        .push_bind(domain.to_string())
        .push(" OR coalesce(s.domain, '') LIKE ")
        .push_bind(format!("%.{}", escape_like(domain)))
        .push(" ESCAPE '\\')");
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub(crate) fn short_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    (0..6)
        .map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char)
        .collect()
}

/// Inserts a story inside the caller's transaction. Returns its ID and short ID.
pub(crate) async fn insert_story_tx(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    s: &NewStory<'_>,
) -> Result<(i64, String)> {
    let mut tag_ids = Vec::with_capacity(s.tags.len());
    let mut tag_boost = 0.0;
    for name in s.tags {
        let (id, m): (i64, f64) =
            sqlx::query_as("SELECT id, rank_boost FROM tags WHERE name = ? AND active = 1")
                .bind(name)
                .fetch_one(&mut **tx)
                .await?;
        tag_ids.push(id);
        tag_boost += m;
    }

    let rank = rank::compute(RankInput {
        score: 1,
        comment_points: 0.0,
        tag_boost,
        user_is_author: s.user_is_author,
        is_link: s.link.is_some(),
        created_at: s.created_at,
    });

    let short_id = loop {
        let candidate = short_id();
        let taken: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM stories WHERE short_id = ?)")
                .bind(&candidate)
                .fetch_one(&mut **tx)
                .await?;
        if !taken {
            break candidate;
        }
    };

    let story_id: i64 = sqlx::query_scalar(
        "INSERT INTO stories (short_id, user_id, title, url, canonical_url, domain, body, \
         user_is_author, rank, created_at, at_uri, at_cid) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(&short_id)
    .bind(s.user_id)
    .bind(s.title)
    .bind(s.link.map(|l| l.url.as_str()))
    .bind(s.link.map(|l| l.canonical.as_str()))
    .bind(s.link.map(|l| l.domain.as_str()))
    .bind(s.body)
    .bind(s.user_is_author)
    .bind(rank)
    .bind(s.created_at)
    .bind(s.record.map(|r| r.0))
    .bind(s.record.map(|r| r.1))
    .fetch_one(&mut **tx)
    .await?;

    for id in tag_ids {
        sqlx::query("INSERT INTO taggings (story_id, tag_id) VALUES (?, ?)")
            .bind(story_id)
            .bind(id)
            .execute(&mut **tx)
            .await?;
    }

    Ok((story_id, short_id))
}

/// A verified handle belongs to one DID at a time. When `did` verifies
/// `handle`, any other account still holding it loses the verification and
/// shows its DID until it verifies a handle of its own again.
pub(crate) async fn claim_handle(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    did: &str,
    handle: &str,
    verified: bool,
    now: i64,
) -> Result<()> {
    if verified {
        sqlx::query(
            "UPDATE users SET handle_verified = 0 WHERE handle = ? AND did <> ? AND handle_verified",
        )
        .bind(handle)
        .bind(did)
        .execute(&mut **tx)
        .await?;
        // Handle history: a row whenever the verified handle differs from
        // the last one recorded for this account.
        sqlx::query(
            "INSERT INTO handle_history (user_id, handle, seen_at) \
             SELECT u.id, ?2, ?3 FROM users u WHERE u.did = ?1 AND ?2 IS NOT \
             (SELECT h.handle FROM handle_history h WHERE h.user_id = u.id ORDER BY h.seen_at DESC, h.id DESC LIMIT 1)",
        )
        .bind(did)
        .bind(handle)
        .bind(now)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}
