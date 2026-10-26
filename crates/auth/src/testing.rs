//! A stored OAuth session for tests that run without a PDS.

use jacquard_common::types::did::Did;
use jacquard_oauth::scopes::Scopes;
use jacquard_oauth::session::{ClientSessionData, DpopClientData};
use jacquard_oauth::types::{OAuthTokenType, TokenSet};
use smol_str::SmolStr;

/// The library's session JSON for `did`, granted `scope`, as the store holds it.
pub fn session_json(did: &str, session_id: &str, scope: &str) -> String {
    let data: ClientSessionData = ClientSessionData {
        account_did: Did::new(SmolStr::new(did)).expect("valid DID"),
        session_id: SmolStr::new(session_id),
        host_url: "https://pds.example".parse().expect("URI"),
        authserver_url: SmolStr::new_static("https://pds.example"),
        authserver_token_endpoint: SmolStr::new_static("https://pds.example/oauth/token"),
        authserver_revocation_endpoint: None,
        scopes: Scopes::new(SmolStr::new(scope)).expect("valid scopes"),
        dpop_data: DpopClientData {
            dpop_key: jacquard_oauth::utils::generate_key(&["ES256"]).expect("ES256 key"),
            dpop_authserver_nonce: SmolStr::default(),
            dpop_host_nonce: SmolStr::default(),
        },
        token_set: TokenSet {
            iss: SmolStr::new_static("https://pds.example"),
            sub: Did::new(SmolStr::new(did)).expect("valid DID"),
            aud: SmolStr::new_static("https://pds.example"),
            scope: Some(SmolStr::new(scope)),
            refresh_token: None,
            access_token: SmolStr::new_static("test"),
            token_type: OAuthTokenType::DPoP,
            expires_at: None,
        },
        resolved_scopes: None,
    };
    serde_json::to_string(&data).expect("session serializes")
}
