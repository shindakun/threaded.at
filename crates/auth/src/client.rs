use jacquard_identity::JacquardResolver;
use jacquard_oauth::atproto::{atproto_client_metadata, AtprotoClientMetadata};
use jacquard_oauth::client::OAuthClient;
use jacquard_oauth::keyset::Keyset;
use jacquard_oauth::scopes::Scopes;
use jacquard_oauth::session::ClientData;
use smol_str::SmolStr;

use crate::store::SqliteAuthStore;

pub type Oauth = OAuthClient<JacquardResolver<reqwest::Client>, SqliteAuthStore>;
pub type Session =
    jacquard_oauth::client::OAuthSession<JacquardResolver<reqwest::Client>, SqliteAuthStore>;

/// The record collections Threaded writes. Production writes
/// `at.threaded.*`; local builds write `at.threaded.dev.*` so test records on a
/// real PDS never reach the production index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Namespace(&'static str);

impl Namespace {
    pub const PRODUCTION: Namespace = Namespace("at.threaded");
    pub const DEV: Namespace = Namespace("at.threaded.dev");

    pub fn submission(self) -> String {
        format!("{}.submission", self.0)
    }

    pub fn comment(self) -> String {
        format!("{}.comment", self.0)
    }

    pub fn vote(self) -> String {
        format!("{}.vote", self.0)
    }
}

/// Which client configuration a sign-in used. jacquard-oauth sends a
/// configuration's whole scope list, so each scope set is its own
/// configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Submissions and comments.
    Base,
    /// The base scopes plus public upvotes.
    Votes,
}

impl Flow {
    pub fn as_str(self) -> &'static str {
        match self {
            Flow::Base => "base",
            Flow::Votes => "votes",
        }
    }

    pub fn parse(s: &str) -> Flow {
        if s == "votes" {
            Flow::Votes
        } else {
            Flow::Base
        }
    }
}

pub fn scopes(ns: Namespace, flow: Flow) -> String {
    let base = format!("atproto repo:{} repo:{}", ns.submission(), ns.comment());
    match flow {
        Flow::Base => base,
        Flow::Votes => format!("{base} repo:{}?action=create&action=delete", ns.vote()),
    }
}

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

/// Both client configurations, sharing one client ID, key, and store.
pub struct Clients {
    pub ns: Namespace,
    pub base: Client,
    pub votes: Client,
}

impl Clients {
    pub fn get(&self, flow: Flow) -> &Client {
        match flow {
            Flow::Base => &self.base,
            Flow::Votes => &self.votes,
        }
    }

    /// The metadata document declares every scope a sign-in may request, so it
    /// comes from the configuration with the largest scope set.
    pub fn metadata_json(&self) -> serde_json::Value {
        self.votes.metadata_json()
    }

    pub fn jwks_json(&self) -> Option<serde_json::Value> {
        self.base.jwks_json()
    }
}

pub fn build(mode: Mode, store: SqliteAuthStore) -> Result<Clients, ClientError> {
    let ns = match mode {
        Mode::Hosted { .. } => Namespace::PRODUCTION,
        Mode::Localhost { .. } => Namespace::DEV,
    };
    Ok(Clients {
        ns,
        base: build_one(&mode, store.clone(), &scopes(ns, Flow::Base))?,
        votes: build_one(&mode, store, &scopes(ns, Flow::Votes))?,
    })
}

fn build_one(mode: &Mode, store: SqliteAuthStore, scope: &str) -> Result<Client, ClientError> {
    let scopes: Scopes<SmolStr> =
        Scopes::new(SmolStr::new(scope)).map_err(|e| ClientError::Metadata(format!("{e:?}")))?;
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
            (config, Some(keyset.clone()))
        }
        Mode::Localhost { callback } => (
            AtprotoClientMetadata::new_localhost(Some(vec![uri(callback)?]), Some(scopes)),
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

/// Whether every scope in `wanted` was granted.
pub fn has_scopes(granted: &str, wanted: &str) -> bool {
    let have: Vec<&str> = granted.split_whitespace().collect();
    wanted.split_whitespace().all(|w| have.contains(&w))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_sets_by_namespace() {
        assert_eq!(
            scopes(Namespace::PRODUCTION, Flow::Base),
            "atproto repo:at.threaded.submission repo:at.threaded.comment"
        );
        assert_eq!(
            scopes(Namespace::DEV, Flow::Votes),
            "atproto repo:at.threaded.dev.submission repo:at.threaded.dev.comment \
             repo:at.threaded.dev.vote?action=create&action=delete"
        );
        assert!(has_scopes(
            &scopes(Namespace::DEV, Flow::Votes),
            &scopes(Namespace::DEV, Flow::Base)
        ));
        assert!(!has_scopes("atproto", &scopes(Namespace::DEV, Flow::Base)));
    }

    #[test]
    fn atproto_scope_check() {
        assert!(has_atproto_scope("atproto"));
        assert!(has_atproto_scope("repo:x atproto"));
        assert!(!has_atproto_scope(""));
        assert!(!has_atproto_scope("atproto2 transition:generic"));
    }
}
