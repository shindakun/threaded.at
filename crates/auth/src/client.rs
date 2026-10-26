use jacquard_identity::JacquardResolver;
use jacquard_oauth::atproto::{atproto_client_metadata, AtprotoClientMetadata};
use jacquard_oauth::client::OAuthClient;
use jacquard_oauth::keyset::Keyset;
use jacquard_oauth::scopes::Scopes;
use jacquard_oauth::session::ClientData;
use smol_str::SmolStr;

use crate::store::SqliteAuthStore;

pub type Oauth = OAuthClient<JacquardResolver<reqwest::Client>, SqliteAuthStore>;

/// Every scope a sign-in requests. M1 asks for identity only.
pub const SCOPES: &str = "atproto";

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("bad URL {0:?}")]
    Url(String),
    #[error("client metadata: {0}")]
    Metadata(String),
}

/// Where the client is reachable. Production has a public HTTPS origin and a
/// signing key; development is the atproto localhost client with no key.
pub enum Mode {
    Hosted { public_url: String, keyset: Keyset },
    Localhost { callback: String },
}

pub struct Client {
    pub oauth: Oauth,
    pub config: AtprotoClientMetadata<SmolStr>,
    pub keyset: Option<Keyset>,
}

fn uri(s: &str) -> Result<jacquard_common::deps::fluent_uri::Uri<String>, ClientError> {
    s.parse().map_err(|_| ClientError::Url(s.to_string()))
}

pub fn build(mode: Mode, store: SqliteAuthStore) -> Result<Client, ClientError> {
    let scopes: Scopes<SmolStr> = Scopes::new(SmolStr::new_static(SCOPES))
        .map_err(|e| ClientError::Metadata(format!("{e:?}")))?;
    let (config, keyset) = match mode {
        Mode::Hosted { public_url, keyset } => {
            let base = public_url.trim_end_matches('/');
            let config = AtprotoClientMetadata::new(
                vec![uri(&format!("{base}/oauth/callback"))?],
                uri(&format!("{base}/oauth-client-metadata.json"))?,
                Some(scopes),
            )
            .with_jwks_uri(uri(&format!("{base}/oauth/jwks.json"))?)
            .with_client_name(SmolStr::new_static("Threaded"));
            (config, Some(keyset))
        }
        Mode::Localhost { callback } => (
            AtprotoClientMetadata::new_localhost(Some(vec![uri(&callback)?]), Some(scopes)),
            None,
        ),
    };
    atproto_client_metadata(&config, &keyset).map_err(|e| ClientError::Metadata(e.to_string()))?;
    let oauth = OAuthClient::new(
        store,
        ClientData {
            keyset: keyset.clone(),
            config: config.clone(),
        },
        reqwest::Client::new(),
    );
    Ok(Client {
        oauth,
        config,
        keyset,
    })
}

impl Client {
    pub fn metadata_json(&self) -> serde_json::Value {
        let m = atproto_client_metadata(&self.config, &self.keyset).expect("validated in build");
        serde_json::to_value(m).expect("metadata serializes")
    }

    pub fn jwks_json(&self) -> Option<serde_json::Value> {
        self.keyset
            .as_ref()
            .map(|k| serde_json::to_value(k.public_jwks()).expect("JWK set serializes"))
    }
}

/// The spec requires `atproto` in the granted scope; the library accepts a
/// token response without one.
pub fn has_atproto_scope(granted: &str) -> bool {
    granted.split_whitespace().any(|s| s == "atproto")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atproto_scope_check() {
        assert!(has_atproto_scope("atproto"));
        assert!(has_atproto_scope("repo:x atproto"));
        assert!(!has_atproto_scope(""));
        assert!(!has_atproto_scope("atproto2 transition:generic"));
    }
}
