//! Background re-checks of handles. A handle can change, or pass to another
//! account, at any time; sign-in checks it once, and these checks keep it
//! current between sign-ins: daily, or hourly while an account has no
//! verified handle, as Bluesky's AppView does.

use crate::{claim_handle, Result, Store};

pub const HANDLE_CHECK_SECS: i64 = 86_400;
pub const UNVERIFIED_CHECK_SECS: i64 = 3_600;

/// What a re-check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckedHandle {
    Verified(String),
    Unverified,
    /// The lookup failed; keep what is stored and try again in an hour.
    Failed,
}

impl Store {
    /// Claims the next check of this account's handle if one is due, so only
    /// one request starts it. Returns whether the caller should check.
    pub async fn claim_handle_check(&self, user_id: i64, now: i64) -> Result<bool> {
        let claimed = sqlx::query(
            "UPDATE users SET handle_checked_at = ?1 WHERE id = ?2 AND (handle_checked_at IS NULL \
             OR handle_checked_at <= ?1 - CASE WHEN handle_verified THEN ?3 ELSE ?4 END)",
        )
        .bind(now)
        .bind(user_id)
        .bind(HANDLE_CHECK_SECS)
        .bind(UNVERIFIED_CHECK_SECS)
        .execute(self.pool())
        .await?;
        Ok(claimed.rows_affected() == 1)
    }

    /// Of these accounts, the ones whose handle check is due.
    pub async fn handle_checks_due(
        &self,
        user_ids: &[i64],
        now: i64,
    ) -> Result<Vec<(i64, String)>> {
        if user_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut qb = sqlx::QueryBuilder::new(
            "SELECT id, did FROM users WHERE (handle_checked_at IS NULL OR handle_checked_at <= ",
        );
        qb.push_bind(now)
            .push(" - CASE WHEN handle_verified THEN ")
            .push_bind(HANDLE_CHECK_SECS)
            .push(" ELSE ")
            .push_bind(UNVERIFIED_CHECK_SECS)
            .push(" END) AND id IN (");
        let mut sep = qb.separated(", ");
        for id in user_ids {
            sep.push_bind(*id);
        }
        qb.push(")");
        qb.build_query_as().fetch_all(self.pool()).await
    }

    /// Stores what a check found. A verified handle is claimed from any other
    /// account holding it. A failed check keeps the stored handle and is tried
    /// again in an hour.
    pub async fn record_handle_check(
        &self,
        did: &str,
        found: &CheckedHandle,
        now: i64,
    ) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        match found {
            CheckedHandle::Verified(handle) => {
                sqlx::query(
                    "UPDATE users SET handle = ?, handle_verified = 1, handle_checked_at = ? WHERE did = ?",
                )
                .bind(handle)
                .bind(now)
                .bind(did)
                .execute(&mut *tx)
                .await?;
                claim_handle(&mut tx, did, handle, true, now).await?;
            }
            CheckedHandle::Unverified => {
                sqlx::query(
                    "UPDATE users SET handle_verified = 0, handle_checked_at = ? WHERE did = ?",
                )
                .bind(now)
                .bind(did)
                .execute(&mut *tx)
                .await?;
            }
            CheckedHandle::Failed => {
                // Due again an hour from now, whatever the account's state.
                sqlx::query(
                    "UPDATE users SET handle_checked_at = ? - CASE WHEN handle_verified THEN ? ELSE ? END + ? \
                     WHERE did = ?",
                )
                .bind(now)
                .bind(HANDLE_CHECK_SECS)
                .bind(UNVERIFIED_CHECK_SECS)
                .bind(UNVERIFIED_CHECK_SECS)
                .bind(did)
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await
    }
}
