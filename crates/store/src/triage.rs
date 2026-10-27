//! Storage for AI triage: the site rules, the queue, verdicts, holds, and the
//! numbers behind the circuit breaker. The model call lives in `threaded-triage`.

use crate::moderation::{log, By, LogEntry, ModError, Outcome, QueueItem};
use crate::{Result, Store, Target};

/// Holds decided since the breaker was last reset count toward it only once
/// there are this many; a single released hold should not switch holds off.
pub const BREAKER_MIN_DECIDED: i64 = 20;
/// The breaker looks at this many most recent decided holds.
pub const BREAKER_WINDOW: i64 = 100;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RulesVersion {
    pub id: i64,
    pub body: String,
    pub author: Option<String>,
    pub created_at: i64,
}

/// A queued item waiting for the worker.
#[derive(Debug, Clone, Copy, sqlx::FromRow)]
pub struct Queued {
    pub id: i64,
    pub story_id: Option<i64>,
    pub comment_id: Option<i64>,
}

impl Queued {
    pub fn target(&self) -> Option<Target> {
        match (self.story_id, self.comment_id) {
            (Some(s), None) => Some(Target::Story(s)),
            (None, Some(c)) => Some(Target::Comment(c)),
            _ => None,
        }
    }
}

/// What triage reads about an item: never votes, notes, or addresses.
#[derive(Debug, Clone, PartialEq)]
pub struct TriageItem {
    pub kind: &'static str,
    pub title: String,
    pub url: Option<String>,
    pub domain: Option<String>,
    pub tags: Vec<String>,
    pub text: String,
    /// For a reply, the comment it answers.
    pub parent: Option<String>,
    pub flag_reasons: Vec<String>,
    pub author: crate::UserRow,
    pub author_removals: i64,
    pub author_opted_out: bool,
}

/// A verdict as the worker records it.
#[derive(Debug, Clone, PartialEq)]
pub struct NewVerdict<'a> {
    pub verdict: &'a str,
    pub rule: &'a str,
    pub confidence: &'a str,
    pub explanation: &'a str,
    pub model: &'a str,
    pub rules_version_id: Option<i64>,
    /// Why the model gave no usable verdict, when it did not.
    pub error: Option<&'a str>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct VerdictRow {
    pub id: i64,
    pub story_id: Option<i64>,
    pub comment_id: Option<i64>,
    pub verdict: String,
    pub rule: String,
    pub confidence: String,
    pub explanation: String,
    pub model: String,
    pub rules_version_id: Option<i64>,
    pub error: Option<String>,
    pub held: bool,
    pub decision: Option<String>,
    pub created_at: i64,
}

/// A queue row with the verdict that put it there.
#[derive(Debug, Clone)]
pub struct TriagedItem {
    pub item: QueueItem,
    pub verdict: VerdictRow,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TriageStats {
    pub queue_depth: i64,
    /// Verdicts in the last day that carry a provider error.
    pub errors_last_day: i64,
    pub verdicts_last_day: i64,
    /// Decided holds in the breaker's window, and how many were confirmed.
    pub holds_decided: i64,
    pub holds_confirmed: i64,
    /// Decided suggestions, and how many a moderator acted on.
    pub suggestions_decided: i64,
    pub suggestions_agreed: i64,
}

impl TriageStats {
    pub fn hold_precision(&self) -> Option<f64> {
        (self.holds_decided > 0).then(|| self.holds_confirmed as f64 / self.holds_decided as f64)
    }

    /// Whether automatic holds are off: enough decided holds, too few confirmed.
    pub fn breaker_open(&self, floor: f64) -> bool {
        self.holds_decided >= BREAKER_MIN_DECIDED
            && self.hold_precision().is_some_and(|p| p < floor)
    }
}

/// An item as `triage_item` reads it: author ID, title, URL, domain, text,
/// and for a reply, its parent's text.
type ItemRow = (
    i64,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
);

const VERDICT_COLUMNS: &str = "v.id, v.story_id, v.comment_id, v.verdict, v.rule, v.confidence, \
     v.explanation, v.model, v.rules_version_id, v.error, v.held, v.decision, v.created_at";

impl Store {
    pub async fn current_rules(&self) -> Result<Option<RulesVersion>> {
        sqlx::query_as(
            "SELECT r.id, r.body, CASE WHEN u.handle_verified THEN u.handle ELSE u.did END AS author, r.created_at FROM rules_versions r \
             LEFT JOIN users u ON u.id = r.author_id ORDER BY r.id DESC LIMIT 1",
        )
        .fetch_optional(self.pool())
        .await
    }

    pub async fn rules_history(&self) -> Result<Vec<RulesVersion>> {
        sqlx::query_as(
            "SELECT r.id, r.body, CASE WHEN u.handle_verified THEN u.handle ELSE u.did END AS author, r.created_at FROM rules_versions r \
             LEFT JOIN users u ON u.id = r.author_id ORDER BY r.id DESC",
        )
        .fetch_all(self.pool())
        .await
    }

    /// Saves a new version of the site rules; it takes effect at once.
    pub async fn save_rules(&self, body: &str, by: By, reason: &str, now: i64) -> Result<Outcome> {
        if reason.trim().is_empty() {
            return Ok(Err(ModError::NoReason));
        }
        let body = body.trim();
        if body.is_empty() {
            return Ok(Err(ModError::Refused("Write the rules first.")));
        }
        if self.current_rules().await?.is_some_and(|r| r.body == body) {
            return Ok(Err(ModError::Refused("Nothing changed.")));
        }
        let mut tx = self.pool().begin().await?;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO rules_versions (body, author_id, created_at) VALUES (?, ?, ?) RETURNING id",
        )
        .bind(body)
        .bind(match by {
            By::Moderator(id) => Some(id),
            _ => None,
        })
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
        let detail = format!("version {id}");
        log(
            &mut tx,
            by,
            &LogEntry {
                action: "save rules",
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

    /// The oldest queued item.
    pub async fn triage_next(&self) -> Result<Option<Queued>> {
        sqlx::query_as("SELECT id, story_id, comment_id FROM triage_queue ORDER BY id LIMIT 1")
            .fetch_optional(self.pool())
            .await
    }

    pub async fn triage_drop(&self, queue_id: i64) -> Result<()> {
        sqlx::query("DELETE FROM triage_queue WHERE id = ?")
            .bind(queue_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// What triage reads about a queued item, or `None` if it is gone.
    pub async fn triage_item(&self, target: Target) -> Result<Option<TriageItem>> {
        let (kind, row): (&'static str, Option<ItemRow>) =
            match target {
                Target::Story(id) => (
                    "story",
                    sqlx::query_as(
                        "SELECT user_id, title, url, domain, body, NULL FROM stories \
                         WHERE id = ? AND deleted_at IS NULL",
                    )
                    .bind(id)
                    .fetch_optional(self.pool())
                    .await?,
                ),
                Target::Comment(id) => (
                    "comment",
                    sqlx::query_as(
                        "SELECT c.user_id, s.title, s.url, s.domain, c.body, p.body FROM comments c \
                         JOIN stories s ON s.id = c.story_id LEFT JOIN comments p ON p.id = c.parent_id \
                         WHERE c.id = ? AND c.deleted_at IS NULL",
                    )
                    .bind(id)
                    .fetch_optional(self.pool())
                    .await?,
                ),
            };
        let Some((user_id, title, url, domain, text, parent)) = row else {
            return Ok(None);
        };
        let tags: String = match target {
            Target::Story(id) => {
                sqlx::query_scalar(
                    "SELECT coalesce(group_concat(t.name, ' '), '') FROM taggings tg \
                     JOIN tags t ON t.id = tg.tag_id WHERE tg.story_id = ?",
                )
                .bind(id)
                .fetch_one(self.pool())
                .await?
            }
            Target::Comment(_) => String::new(),
        };
        let author: crate::UserRow = sqlx::query_as(&format!(
            "SELECT {} FROM users WHERE id = ?",
            crate::m2::USER_COLUMNS
        ))
        .bind(user_id)
        .fetch_one(self.pool())
        .await?;
        let opted_out: bool = sqlx::query_scalar("SELECT triage_opt_out FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_one(self.pool())
            .await?;
        let removals = self.standing(user_id).await?.removals;
        let flag_reasons = self
            .flag_reasons(target)
            .await?
            .into_iter()
            .map(|(r, _)| r)
            .collect();
        Ok(Some(TriageItem {
            kind,
            title,
            url,
            domain,
            tags: tags.split_whitespace().map(str::to_string).collect(),
            text,
            parent,
            flag_reasons,
            author,
            author_removals: removals,
            author_opted_out: opted_out,
        }))
    }

    /// Records a verdict for a queued item and takes it off the queue. With
    /// `hold`, the item is hidden and the hold is logged with triage as the actor.
    pub async fn record_verdict(
        &self,
        queue_id: i64,
        target: Target,
        v: &NewVerdict<'_>,
        hold: bool,
        now: i64,
    ) -> Result<()> {
        let (s, c) = match target {
            Target::Story(id) => (Some(id), None),
            Target::Comment(id) => (None, Some(id)),
        };
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "INSERT OR IGNORE INTO triage_verdicts (story_id, comment_id, verdict, rule, confidence, \
             explanation, model, rules_version_id, error, held, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(s)
        .bind(c)
        .bind(v.verdict)
        .bind(v.rule)
        .bind(v.confidence)
        .bind(v.explanation)
        .bind(v.model)
        .bind(v.rules_version_id)
        .bind(v.error)
        .bind(hold)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        if hold {
            let (table, id) = match target {
                Target::Story(id) => ("stories", id),
                Target::Comment(id) => ("comments", id),
            };
            sqlx::query(&format!("UPDATE {table} SET held_at = ? WHERE id = ?"))
                .bind(now)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            let (author, story_id): (i64, i64) = match target {
                Target::Story(id) => {
                    sqlx::query_as("SELECT user_id, id FROM stories WHERE id = ?")
                        .bind(id)
                        .fetch_one(&mut *tx)
                        .await?
                }
                Target::Comment(id) => {
                    sqlx::query_as("SELECT user_id, story_id FROM comments WHERE id = ?")
                        .bind(id)
                        .fetch_one(&mut *tx)
                        .await?
                }
            };
            crate::moderation::recount(&mut tx, story_id).await?;
            let kind = if c.is_some() { "comment" } else { "story" };
            let action = format!("hold {kind}");
            let reason = if v.rule.is_empty() { "Spam" } else { v.rule };
            log(
                &mut tx,
                By::Triage,
                &LogEntry {
                    action: &action,
                    user_id: Some(author),
                    story_id: s,
                    comment_id: c,
                    reason,
                    detail: v.explanation,
                    ..Default::default()
                },
                now,
            )
            .await?;
        }
        sqlx::query("DELETE FROM triage_queue WHERE id = ?")
            .bind(queue_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await
    }

    pub async fn verdict_for(&self, target: Target) -> Result<Option<VerdictRow>> {
        let (col, id) = crate::m2::target_cols(target);
        sqlx::query_as(&format!(
            "SELECT {VERDICT_COLUMNS} FROM triage_verdicts v WHERE v.{col} = ?"
        ))
        .bind(id)
        .fetch_optional(self.pool())
        .await
    }

    async fn triaged(&self, cond: &str, order: &str) -> Result<Vec<TriagedItem>> {
        let verdicts: Vec<VerdictRow> = sqlx::query_as(&format!(
            "SELECT {VERDICT_COLUMNS} FROM triage_verdicts v WHERE v.decision IS NULL AND {cond} ORDER BY {order}"
        ))
        .fetch_all(self.pool())
        .await?;
        let mut out = Vec::new();
        for v in verdicts {
            let target = match (v.story_id, v.comment_id) {
                (Some(s), _) => Target::Story(s),
                (_, Some(c)) => Target::Comment(c),
                _ => continue,
            };
            if let Some(item) = self.queue_item(target).await? {
                out.push(TriagedItem { item, verdict: v });
            }
        }
        Ok(out)
    }

    /// Items triage held, oldest first, so nothing waits long.
    pub async fn holds(&self) -> Result<Vec<TriagedItem>> {
        self.triaged("v.held = 1", "v.created_at, v.id").await
    }

    /// Verdicts other than `ok` that held nothing, highest confidence first.
    pub async fn triage_suggestions(&self) -> Result<Vec<TriagedItem>> {
        self.triaged(
            "v.held = 0 AND v.verdict <> 'ok'",
            "CASE v.confidence WHEN 'high' THEN 0 WHEN 'medium' THEN 1 WHEN 'low' THEN 2 ELSE 3 END, v.created_at",
        )
        .await
    }

    /// A moderator's decision on a verdict: `confirmed` or `released` for a
    /// hold, `removed` or `dismissed` for a suggestion.
    pub async fn decide_verdict(
        &self,
        verdict_id: i64,
        decision: &str,
        by: By,
        reason: &str,
        now: i64,
    ) -> Result<Outcome> {
        if reason.trim().is_empty() {
            return Ok(Err(ModError::NoReason));
        }
        let Some(v): Option<VerdictRow> = sqlx::query_as(&format!(
            "SELECT {VERDICT_COLUMNS} FROM triage_verdicts v WHERE v.id = ? AND v.decision IS NULL"
        ))
        .bind(verdict_id)
        .fetch_optional(self.pool())
        .await?
        else {
            return Ok(Err(ModError::NotFound));
        };
        let valid = if v.held {
            matches!(decision, "confirmed" | "released")
        } else {
            matches!(decision, "removed" | "dismissed")
        };
        if !valid {
            return Ok(Err(ModError::Refused(
                "That decision does not fit this verdict.",
            )));
        }
        let target = match (v.story_id, v.comment_id) {
            (Some(s), _) => Target::Story(s),
            (_, Some(c)) => Target::Comment(c),
            _ => return Ok(Err(ModError::NotFound)),
        };
        let (table, id, kind) = match target {
            Target::Story(id) => ("stories", id, "story"),
            Target::Comment(id) => ("comments", id, "comment"),
        };
        let mut tx = self.pool().begin().await?;
        let owner: Option<(i64, i64)> = match target {
            Target::Story(id) => {
                sqlx::query_as("SELECT user_id, id FROM stories WHERE id = ?")
                    .bind(id)
                    .fetch_optional(&mut *tx)
                    .await?
            }
            Target::Comment(id) => {
                sqlx::query_as("SELECT user_id, story_id FROM comments WHERE id = ?")
                    .bind(id)
                    .fetch_optional(&mut *tx)
                    .await?
            }
        };
        let Some((author, story_id)) = owner else {
            return Ok(Err(ModError::NotFound));
        };
        match decision {
            // A confirmed hold becomes a removal, which a moderator can restore.
            "confirmed" | "removed" => {
                sqlx::query(&format!(
                    "UPDATE {table} SET held_at = NULL, removed_at = coalesce(removed_at, ?), \
                     removed_by = coalesce(removed_by, ?), removed_reason = coalesce(removed_reason, ?) WHERE id = ?"
                ))
                .bind(now)
                .bind(match by {
                    By::Moderator(m) => Some(m),
                    _ => None,
                })
                .bind(reason.trim())
                .bind(id)
                .execute(&mut *tx)
                .await?;
            }
            "released" => {
                sqlx::query(&format!("UPDATE {table} SET held_at = NULL WHERE id = ?"))
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
            }
            _ => {}
        }
        sqlx::query(
            "UPDATE triage_verdicts SET decision = ?, decided_by = ?, decided_at = ? WHERE id = ?",
        )
        .bind(decision)
        .bind(match by {
            By::Moderator(m) => Some(m),
            _ => None,
        })
        .bind(now)
        .bind(verdict_id)
        .execute(&mut *tx)
        .await?;
        crate::moderation::recount(&mut tx, story_id).await?;
        let action = match decision {
            "confirmed" => format!("confirm hold on {kind}"),
            "released" => format!("release {kind}"),
            "removed" => format!("remove {kind}"),
            _ => "dismiss triage suggestion".to_string(),
        };
        let (s, c) = match target {
            Target::Story(id) => (Some(id), None),
            Target::Comment(id) => (None, Some(id)),
        };
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

    /// When the breaker was last reset; holds decided before it do not count.
    pub async fn breaker_reset_at(&self) -> Result<i64> {
        let v: Option<String> =
            sqlx::query_scalar("SELECT value FROM settings WHERE key = 'breaker_reset_at'")
                .fetch_optional(self.pool())
                .await?;
        Ok(v.and_then(|v| v.parse().ok()).unwrap_or(0))
    }

    pub async fn reset_breaker(&self, by: By, reason: &str, now: i64) -> Result<Outcome> {
        if reason.trim().is_empty() {
            return Ok(Err(ModError::NoReason));
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "INSERT INTO settings (key, value, updated_by, updated_at) VALUES ('breaker_reset_at', ?, ?, ?) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_by = excluded.updated_by, \
             updated_at = excluded.updated_at",
        )
        .bind(now.to_string())
        .bind(match by {
            By::Moderator(m) => Some(m),
            _ => None,
        })
        .bind(now)
        .execute(&mut *tx)
        .await?;
        log(
            &mut tx,
            by,
            &LogEntry {
                action: "reset triage breaker",
                reason: reason.trim(),
                ..Default::default()
            },
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(Ok(()))
    }

    pub async fn triage_stats(&self, now: i64) -> Result<TriageStats> {
        let reset = self.breaker_reset_at().await?;
        let queue_depth: i64 = sqlx::query_scalar("SELECT count(*) FROM triage_queue")
            .fetch_one(self.pool())
            .await?;
        let (verdicts_last_day, errors_last_day): (i64, i64) = sqlx::query_as(
            "SELECT count(*), coalesce(sum(error IS NOT NULL), 0) FROM triage_verdicts WHERE created_at >= ?",
        )
        .bind(now - 86_400)
        .fetch_one(self.pool())
        .await?;
        let (holds_decided, holds_confirmed): (i64, i64) = sqlx::query_as(
            "SELECT count(*), coalesce(sum(decision = 'confirmed'), 0) FROM \
             (SELECT decision FROM triage_verdicts WHERE held = 1 AND decision IS NOT NULL \
              AND decided_at >= ? ORDER BY decided_at DESC, id DESC LIMIT ?)",
        )
        .bind(reset)
        .bind(BREAKER_WINDOW)
        .fetch_one(self.pool())
        .await?;
        let (suggestions_decided, suggestions_agreed): (i64, i64) = sqlx::query_as(
            "SELECT count(*), coalesce(sum(decision = 'removed'), 0) FROM triage_verdicts \
             WHERE held = 0 AND verdict <> 'ok' AND decision IS NOT NULL",
        )
        .fetch_one(self.pool())
        .await?;
        Ok(TriageStats {
            queue_depth,
            errors_last_day,
            verdicts_last_day,
            holds_decided,
            holds_confirmed,
            suggestions_decided,
            suggestions_agreed,
        })
    }

    pub async fn set_triage_opt_out(&self, user_id: i64, out: bool) -> Result<()> {
        sqlx::query("UPDATE users SET triage_opt_out = ? WHERE id = ?")
            .bind(out)
            .bind(user_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn triage_opt_out(&self, user_id: i64) -> Result<bool> {
        sqlx::query_scalar("SELECT triage_opt_out FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_one(self.pool())
            .await
    }
}
