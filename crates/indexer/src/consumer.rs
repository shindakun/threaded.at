//! The task that reads hydrant's stream: it applies each event in order,
//! moves the cursor past it, holds records that arrive before what they
//! refer to, and reconnects from the cursor whenever the connection drops.
//!
//! The cursor moves after an event is applied, not in the same transaction.
//! Applying an event twice changes nothing, so a crash between the two only
//! replays that event.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use threaded_core::policy::Policy;
use threaded_store::{CheckedHandle, Store};

use crate::apply::{apply, Applied, Collections, Settings};
use crate::client::{Event, Hydrant, RecordEvent};

/// How long a record may wait for what it refers to.
pub const PENDING_TTL_SECS: i64 = 86_400;
/// How often waiting records past their time are dropped.
const EXPIRE_EVERY_SECS: i64 = 600;
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

pub type PolicyFn = Arc<dyn Fn() -> Policy + Send + Sync>;
/// Checks a DID's handle both ways, as sign-in does.
pub type CheckFn = Arc<
    dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = CheckedHandle> + Send>>
        + Send
        + Sync,
>;
pub type FlagFn = Arc<dyn Fn() -> bool + Send + Sync>;
pub type ClockFn = Arc<dyn Fn() -> i64 + Send + Sync>;

pub struct Indexer {
    pub store: Store,
    pub hydrant: Hydrant,
    pub collections: Collections,
    /// The rules in force, read for every event so admin changes apply.
    pub policy: PolicyFn,
    /// Whether AI triage is on.
    pub triage: FlagFn,
    pub clock: ClockFn,
    /// Checks a handle when an identity event says it changed.
    pub check_handle: CheckFn,
    last_expired: AtomicI64,
}

impl Indexer {
    pub fn new(
        store: Store,
        hydrant: Hydrant,
        collections: Collections,
        policy: PolicyFn,
        triage: FlagFn,
        clock: ClockFn,
        check_handle: CheckFn,
    ) -> Indexer {
        Indexer {
            store,
            hydrant,
            collections,
            policy,
            triage,
            clock,
            check_handle,
            last_expired: AtomicI64::new(0),
        }
    }

    /// Applies one stream event, then moves the cursor past it.
    pub async fn handle(&self, event: &Event) -> Result<(), threaded_store::Error> {
        let now = (self.clock)();
        match event {
            Event::Record { record, .. } => self.apply_record(record, now).await?,
            Event::Identity { identity, .. } => {
                // hydrant's handle is a hint; the handle is checked here both
                // ways before it is shown, as at sign-in.
                if self.store.user_by_did(&identity.did).await?.is_some() {
                    let found = (self.check_handle)(identity.did.clone()).await;
                    tracing::info!(did = identity.did, ?found, "identity changed");
                    self.store
                        .record_handle_check(&identity.did, &found, now)
                        .await?;
                }
            }
            Event::Account { account, .. } => {
                let changed = self
                    .store
                    .set_account_active(
                        &account.did,
                        account.active,
                        account.status.as_deref(),
                        now,
                    )
                    .await?;
                if changed {
                    tracing::info!(did = account.did, active = account.active, status = ?account.status, "account state changed");
                }
            }
        }
        self.store.set_stream_cursor(event.id() + 1, now).await?;
        self.expire(now).await
    }

    async fn apply_record(&self, ev: &RecordEvent, now: i64) -> Result<(), threaded_store::Error> {
        let policy = (self.policy)();
        let settings = Settings {
            collections: &self.collections,
            policy: &policy,
            triage: (self.triage)(),
            now,
        };
        let uri = ev.uri();
        let outcome = apply(&self.store, &settings, ev).await?;
        log(&uri, &outcome);
        match outcome {
            Applied::Pending { waits_for } => {
                let json = serde_json::to_string(ev).unwrap_or_default();
                self.store
                    .hold_pending(&uri, &waits_for, &json, now)
                    .await?;
            }
            Applied::Indexed { .. } => self.release(&uri, &settings).await?,
            Applied::Ignored(_) if matches!(ev.action, crate::client::Action::Delete) => {
                // Deleted while it waited for its subject.
                self.store.drop_pending_uri(&uri).await?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Applies records that waited for `uri`, and in turn those that waited
    /// for them.
    async fn release(&self, uri: &str, s: &Settings<'_>) -> Result<(), threaded_store::Error> {
        let mut ready = vec![uri.to_string()];
        while let Some(arrived) = ready.pop() {
            for p in self.store.pending_for(&arrived).await? {
                let Ok(ev) = serde_json::from_str::<RecordEvent>(&p.event) else {
                    self.store.drop_pending(p.id).await?;
                    continue;
                };
                let outcome = apply(&self.store, s, &ev).await?;
                log(&p.uri, &outcome);
                match outcome {
                    // Still waiting, now for something else, such as a parent.
                    Applied::Pending { waits_for } => {
                        self.store
                            .hold_pending(&p.uri, &waits_for, &p.event, s.now)
                            .await?;
                    }
                    Applied::Indexed { .. } => {
                        self.store.drop_pending(p.id).await?;
                        ready.push(p.uri);
                    }
                    _ => self.store.drop_pending(p.id).await?,
                }
            }
        }
        Ok(())
    }

    async fn expire(&self, now: i64) -> Result<(), threaded_store::Error> {
        let last = self.last_expired.load(Ordering::Relaxed);
        if now - last < EXPIRE_EVERY_SECS {
            return Ok(());
        }
        self.last_expired.store(now, Ordering::Relaxed);
        let dropped = self.store.expire_pending(now - PENDING_TTL_SECS).await?;
        if dropped > 0 {
            tracing::info!(
                dropped,
                "dropped records that waited a day for their subject"
            );
        }
        Ok(())
    }

    /// Reads the stream until the process ends, reconnecting from the cursor
    /// with backoff whenever the connection drops or an event fails to apply.
    pub async fn run(self) {
        let mut backoff = BACKOFF_MIN;
        loop {
            match self.session(&mut backoff).await {
                Ok(()) => tracing::info!("hydrant closed the stream; reconnecting"),
                Err(e) => tracing::warn!(error = %e, "hydrant stream"),
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
    }

    /// One connection: from the cursor until it closes or something fails.
    async fn session(&self, backoff: &mut Duration) -> Result<(), String> {
        let cursor = self
            .store
            .stream_cursor()
            .await
            .map_err(|e| e.to_string())?
            .unwrap_or(0);
        let mut stream = self
            .hydrant
            .stream(Some(cursor))
            .await
            .map_err(|e| e.to_string())?;
        tracing::info!(cursor, "reading hydrant's stream");
        while let Some(next) = stream.next().await {
            let event = next.map_err(|e| e.to_string())?;
            self.handle(&event).await.map_err(|e| e.to_string())?;
            *backoff = BACKOFF_MIN;
        }
        Ok(())
    }
}

fn log(uri: &str, outcome: &Applied) {
    match outcome {
        Applied::Indexed { short_id } => tracing::info!(uri, short_id, "indexed"),
        Applied::Refused(why) => tracing::info!(uri, why, "not indexed"),
        Applied::Pending { waits_for } => tracing::info!(uri, waits_for, "waiting for its subject"),
        Applied::Detached => {
            tracing::info!(uri, "detached: the link changed after the edit window")
        }
        other => tracing::debug!(uri, ?other, "applied"),
    }
}

/// Tracks every DID that has signed in here, so hydrant sends their public
/// votes even if they never post. Returns how many hydrant newly queued.
pub async fn track_signed_in(store: &Store, hydrant: &Hydrant) -> Result<usize, String> {
    let dids = store.signed_in_dids().await.map_err(|e| e.to_string())?;
    let refs: Vec<&str> = dids.iter().map(String::as_str).collect();
    let queued = hydrant.track(&refs).await.map_err(|e| e.to_string())?;
    Ok(queued.len())
}
