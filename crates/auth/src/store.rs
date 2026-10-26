use std::time::{SystemTime, UNIX_EPOCH};

use jacquard_common::bos::BosStr;
use jacquard_common::session::SessionStoreError;
use jacquard_common::types::did::Did;
use jacquard_oauth::authstore::ClientAuthStore;
use jacquard_oauth::session::{AuthRequestData, ClientSessionData};
use sqlx::SqlitePool;

/// Pending authorization requests older than this are deleted by `prune`.
pub const REQUEST_TTL_SECS: i64 = 15 * 60;

#[derive(Clone)]
pub struct SqliteAuthStore {
    pool: SqlitePool,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn db(e: sqlx::Error) -> SessionStoreError {
    SessionStoreError::Other(Box::new(e))
}

impl SqliteAuthStore {
    pub fn new(pool: SqlitePool) -> SqliteAuthStore {
        SqliteAuthStore { pool }
    }

    /// Deletes pending requests older than `REQUEST_TTL_SECS`.
    pub async fn prune(&self) -> Result<u64, sqlx::Error> {
        let r = sqlx::query("DELETE FROM oauth_requests WHERE created_at < ?")
            .bind(now() - REQUEST_TTL_SECS)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    pub async fn session_exists(&self, did: &str, session_id: &str) -> Result<bool, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM oauth_sessions WHERE did = ? AND session_id = ?)",
        )
        .bind(did)
        .bind(session_id)
        .fetch_one(&self.pool)
        .await
    }
}

impl ClientAuthStore for SqliteAuthStore {
    async fn get_session<D: BosStr + Send + Sync>(
        &self,
        did: &Did<D>,
        session_id: &str,
    ) -> Result<Option<ClientSessionData>, SessionStoreError> {
        let row: Option<String> =
            sqlx::query_scalar("SELECT data FROM oauth_sessions WHERE did = ? AND session_id = ?")
                .bind(did.to_string())
                .bind(session_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(db)?;
        row.map(|d| serde_json::from_str(&d).map_err(SessionStoreError::from))
            .transpose()
    }

    async fn upsert_session(&self, session: ClientSessionData) -> Result<(), SessionStoreError> {
        let data = serde_json::to_string(&session)?;
        sqlx::query(
            "INSERT INTO oauth_sessions (did, session_id, data, updated_at) VALUES (?, ?, ?, ?) \
             ON CONFLICT(did, session_id) DO UPDATE SET data = excluded.data, updated_at = excluded.updated_at",
        )
        .bind(session.account_did.to_string())
        .bind(session.session_id.to_string())
        .bind(data)
        .bind(now())
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn delete_session<D: BosStr + Send + Sync>(
        &self,
        did: &Did<D>,
        session_id: &str,
    ) -> Result<(), SessionStoreError> {
        sqlx::query("DELETE FROM oauth_sessions WHERE did = ? AND session_id = ?")
            .bind(did.to_string())
            .bind(session_id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn get_auth_req_info(
        &self,
        state: &str,
    ) -> Result<Option<AuthRequestData>, SessionStoreError> {
        let row: Option<String> = sqlx::query_scalar(
            "SELECT data FROM oauth_requests WHERE state = ? AND created_at >= ?",
        )
        .bind(state)
        .bind(now() - REQUEST_TTL_SECS)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?;
        row.map(|d| serde_json::from_str(&d).map_err(SessionStoreError::from))
            .transpose()
    }

    async fn save_auth_req_info(&self, info: &AuthRequestData) -> Result<(), SessionStoreError> {
        let data = serde_json::to_string(info)?;
        sqlx::query(
            "INSERT OR REPLACE INTO oauth_requests (state, data, created_at) VALUES (?, ?, ?)",
        )
        .bind(info.state.to_string())
        .bind(data)
        .bind(now())
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    async fn delete_auth_req_info(&self, state: &str) -> Result<(), SessionStoreError> {
        sqlx::query("DELETE FROM oauth_requests WHERE state = ?")
            .bind(state)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }
}
