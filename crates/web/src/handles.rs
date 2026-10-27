//! Keeps handles current between sign-ins. A handle can change, or pass to
//! another account, at any time. Pages check the handles they show in the
//! background, at most daily per account (hourly while it has none), so a
//! request never waits on a DID lookup.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use threaded_auth::client::Clients;
use threaded_store::CheckedHandle;

use crate::AppState;

/// Most accounts one request starts a check for.
const CHECKS_PER_REQUEST: usize = 10;

/// Where handle checks go: the network, or fixed answers in tests.
#[derive(Clone)]
pub enum HandleChecker {
    Network(Arc<Clients>),
    /// Answers by DID; a DID with no entry fails, as a network error would.
    Fixed(Arc<Mutex<HashMap<String, CheckedHandle>>>),
}

impl HandleChecker {
    async fn check(&self, did: &str) -> CheckedHandle {
        match self {
            HandleChecker::Fixed(answers) => answers
                .lock()
                .unwrap()
                .get(did)
                .cloned()
                .unwrap_or(CheckedHandle::Failed),
            HandleChecker::Network(clients) => {
                match threaded_auth::flow::check_handle_of(clients, did).await {
                    threaded_auth::flow::HandleCheck::Verified(h) => CheckedHandle::Verified(h),
                    threaded_auth::flow::HandleCheck::Unverified => CheckedHandle::Unverified,
                    threaded_auth::flow::HandleCheck::Failed => CheckedHandle::Failed,
                }
            }
        }
    }
}

/// Starts background checks for whichever of these accounts are due.
pub(crate) async fn refresh(st: &AppState, user_ids: &[i64]) {
    let now = (st.clock)();
    let due = match st.store.handle_checks_due(user_ids, now).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "finding handles to check");
            return;
        }
    };
    for (id, did) in due.into_iter().take(CHECKS_PER_REQUEST) {
        // Claiming moves the account's next check forward, so concurrent
        // requests start one check, not several.
        if !matches!(st.store.claim_handle_check(id, now).await, Ok(true)) {
            continue;
        }
        let st = st.clone();
        tokio::spawn(async move {
            let found = st.handles.check(&did).await;
            if let Err(e) = st
                .store
                .record_handle_check(&did, &found, (st.clock)())
                .await
            {
                tracing::warn!(error = %e, did, "storing a handle check");
            }
        });
    }
}
