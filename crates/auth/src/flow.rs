use jacquard_common::types::did::Did;
use jacquard_common::types::handle::Handle;
use jacquard_identity::resolver::IdentityResolver;
use jacquard_oauth::authstore::ClientAuthStore;
use jacquard_oauth::resolver::OAuthResolver;
use jacquard_oauth::types::{AuthorizeOptions, CallbackParams};
use smol_str::SmolStr;

use crate::client::{has_atproto_scope, Clients, Flow};
use crate::hosts::{host_of, HostView};

/// What the confirm page shows before the user leaves for their server.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// The host whose sign-in page the user will see.
    pub auth_host: HostView,
    /// The account's PDS host, when it differs from `auth_host`.
    pub pds_host: Option<HostView>,
    /// The handle or DID entered, when the input named an account.
    pub account: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SignedIn {
    pub did: String,
    pub session_id: String,
    /// The handle, only when it resolves back to `did`.
    pub verified_handle: Option<String>,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum FlowError {
    #[error("no account or server found for {0:?}")]
    NotFound(String),
    #[error("{0}")]
    Server(String),
    #[error("the server did not grant the atproto scope")]
    NoAtprotoScope,
}

/// Lowercased, trimmed, leading `@` removed. Server URLs keep their case.
pub fn normalize(input: &str) -> String {
    let s = input.trim();
    if s.starts_with("https://") || s.starts_with("http://") {
        return s.to_string();
    }
    s.trim_start_matches('@').to_ascii_lowercase()
}

pub async fn resolve(clients: &Clients, input: &str) -> Result<Resolved, FlowError> {
    let (server, doc) = clients
        .base
        .oauth
        .client
        .resolve_oauth(input)
        .await
        .map_err(|_| FlowError::NotFound(input.to_string()))?;
    let auth_host = host_of(server.authorization_endpoint.as_str())
        .ok_or_else(|| FlowError::Server("authorization server has no host".into()))?;
    let pds_host = doc
        .as_ref()
        .and_then(|d| d.pds_endpoint())
        .and_then(|u| host_of(u.as_str()))
        .filter(|h| h.ascii != auth_host.ascii);
    let account = doc.is_some().then(|| input.to_string());
    Ok(Resolved {
        auth_host,
        pds_host,
        account,
    })
}

/// Makes the pushed authorization request and returns the URL to send the
/// browser to.
pub async fn start(clients: &Clients, flow: Flow, input: &str) -> Result<String, FlowError> {
    clients
        .get(flow)
        .oauth
        .start_auth(input, AuthorizeOptions::default())
        .await
        .map_err(|e| FlowError::Server(e.to_string()))
}

pub async fn finish(
    clients: &Clients,
    flow: Flow,
    code: &str,
    state: Option<&str>,
    iss: Option<&str>,
) -> Result<SignedIn, FlowError> {
    let params = CallbackParams {
        code: SmolStr::new(code),
        state: state.map(SmolStr::new),
        iss: iss.map(SmolStr::new),
    };
    let client = clients.get(flow);
    let session = client
        .oauth
        .callback(params)
        .await
        .map_err(|e| FlowError::Server(e.to_string()))?;
    let (did, session_id) = session.session_info().await;

    let stored = client
        .oauth
        .registry
        .store
        .get_session(&did, &session_id)
        .await
        .map_err(|e| FlowError::Server(e.to_string()))?;
    let granted = stored
        .as_ref()
        .and_then(|s| s.token_set.scope.as_deref())
        .unwrap_or("");
    if !has_atproto_scope(granted) {
        let _ = client
            .oauth
            .registry
            .store
            .delete_session(&did, &session_id)
            .await;
        return Err(FlowError::NoAtprotoScope);
    }

    let verified_handle = verify_handle(clients, &did).await;
    Ok(SignedIn {
        did: did.to_string(),
        session_id: session_id.to_string(),
        verified_handle,
    })
}

/// The DID document's first handle, if that handle resolves back to `did`.
pub async fn verify_handle(clients: &Clients, did: &Did) -> Option<String> {
    let resolver = clients.base.oauth.client.as_ref();
    let resp = resolver.resolve_did_doc(did).await.ok()?;
    let doc = resp.parse().ok()?;
    let handle = doc.handles().into_iter().next()?.to_string();
    let parsed = Handle::new(SmolStr::new(&handle)).ok()?;
    let back = resolver.resolve_handle(&parsed).await.ok()?;
    (back.as_str() == did.as_str()).then_some(handle)
}

/// Revokes the session at its server and deletes it here. A session that no
/// longer restores is deleted locally.
pub async fn sign_out(clients: &Clients, flow: Flow, did: &str, session_id: &str) {
    let client = clients.get(flow);
    let Ok(did) = Did::new(SmolStr::new(did)) else {
        return;
    };
    match client.oauth.restore(&did, session_id).await {
        Ok(session) => {
            if let Err(e) = session.logout().await {
                tracing::warn!(error = %e, "sign-out: revoke failed; session deleted locally");
            }
        }
        Err(_) => {
            let _ = client
                .oauth
                .registry
                .store
                .delete_session(&did, session_id)
                .await;
        }
    }
}

/// The scopes granted to a stored session, or `None` when it no longer exists.
pub async fn granted_scopes(clients: &Clients, did: &str, session_id: &str) -> Option<String> {
    let did = Did::new(SmolStr::new(did)).ok()?;
    let data = clients
        .base
        .oauth
        .registry
        .store
        .get_session(&did, session_id)
        .await
        .ok()??;
    Some(data.token_set.scope.as_deref().unwrap_or("").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_input() {
        assert_eq!(normalize("  @Alice.Bsky.Social "), "alice.bsky.social");
        assert_eq!(normalize("did:plc:ABC"), "did:plc:abc");
        assert_eq!(normalize("https://PDS.example/"), "https://PDS.example/");
    }
}
