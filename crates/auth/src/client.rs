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

    /// The namespace prefix, such as `at.threaded`.
    pub fn prefix(self) -> &'static str {
        self.0
    }

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

/// Bluesky posts, the one collection outside Threaded's namespace that it
/// writes, and only for a user who turns cross-posting on.
pub const BSKY_POST: &str = "app.bsky.feed.post";

/// Which client configuration a sign-in used: the base scopes for
/// submissions and comments, and any of the optional ones. jacquard-oauth
/// sends a configuration's whole scope list, so each combination is its own
/// configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Flow {
    /// Public upvotes: create and delete vote records.
    pub votes: bool,
    /// Cross-posting: create Bluesky posts.
    pub bsky: bool,
}

impl Flow {
    pub const BASE: Flow = Flow {
        votes: false,
        bsky: false,
    };
    pub const VOTES: Flow = Flow {
        votes: true,
        bsky: false,
    };
    pub const BSKY: Flow = Flow {
        votes: false,
        bsky: true,
    };
    pub const ALL: Flow = Flow {
        votes: true,
        bsky: true,
    };

    pub fn as_str(self) -> &'static str {
        match (self.votes, self.bsky) {
            (false, false) => "base",
            (true, false) => "votes",
            (false, true) => "bsky",
            (true, true) => "votes-bsky",
        }
    }

    /// Anything unknown is the base flow, which grants the least.
    pub fn parse(s: &str) -> Flow {
        match s {
            "votes" => Flow::VOTES,
            "bsky" => Flow::BSKY,
            "votes-bsky" => Flow::ALL,
            _ => Flow::BASE,
        }
    }

    /// The scopes of both.
    pub fn with(self, other: Flow) -> Flow {
        Flow {
            votes: self.votes || other.votes,
            bsky: self.bsky || other.bsky,
        }
    }

    fn index(self) -> usize {
        usize::from(self.votes) | usize::from(self.bsky) << 1
    }
}

pub fn scopes(ns: Namespace, flow: Flow) -> String {
    let mut s = format!("atproto repo:{} repo:{}", ns.submission(), ns.comment());
    if flow.votes {
        s.push_str(&format!(" repo:{}?action=create&action=delete", ns.vote()));
    }
    if flow.bsky {
        s.push_str(&format!(" repo:{BSKY_POST}?action=create"));
    }
    s
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

/// Every client configuration, one per `Flow`, sharing one client ID, key,
/// and store.
pub struct Clients {
    pub ns: Namespace,
    /// By `Flow::index`.
    flows: [Client; 4],
}

impl Clients {
    pub fn get(&self, flow: Flow) -> &Client {
        &self.flows[flow.index()]
    }

    /// The configuration for lookups that need no particular scope.
    pub fn base(&self) -> &Client {
        self.get(Flow::BASE)
    }

    /// The metadata document declares every scope a sign-in may request, so it
    /// comes from the configuration with the largest scope set.
    pub fn metadata_json(&self) -> serde_json::Value {
        self.get(Flow::ALL).metadata_json()
    }

    pub fn jwks_json(&self) -> Option<serde_json::Value> {
        self.base().jwks_json()
    }
}

pub fn build(mode: Mode, store: SqliteAuthStore) -> Result<Clients, ClientError> {
    let ns = match mode {
        Mode::Hosted { .. } => Namespace::PRODUCTION,
        Mode::Localhost { .. } => Namespace::DEV,
    };
    let one = |flow| build_one(&mode, store.clone(), &scopes(ns, flow));
    let flows = [
        one(Flow::BASE)?,
        one(Flow::VOTES)?,
        one(Flow::BSKY)?,
        one(Flow::ALL)?,
    ];
    debug_assert!([Flow::BASE, Flow::VOTES, Flow::BSKY, Flow::ALL]
        .iter()
        .enumerate()
        .all(|(i, f)| f.index() == i));
    Ok(Clients { ns, flows })
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
            scopes(Namespace::PRODUCTION, Flow::BASE),
            "atproto repo:at.threaded.submission repo:at.threaded.comment"
        );
        assert_eq!(
            scopes(Namespace::DEV, Flow::VOTES),
            "atproto repo:at.threaded.dev.submission repo:at.threaded.dev.comment \
             repo:at.threaded.dev.vote?action=create&action=delete"
        );
        assert_eq!(
            scopes(Namespace::PRODUCTION, Flow::ALL),
            "atproto repo:at.threaded.submission repo:at.threaded.comment \
             repo:at.threaded.vote?action=create&action=delete \
             repo:app.bsky.feed.post?action=create"
        );
        assert!(has_scopes(
            &scopes(Namespace::DEV, Flow::VOTES),
            &scopes(Namespace::DEV, Flow::BASE)
        ));
        assert!(!has_scopes(
            &scopes(Namespace::DEV, Flow::VOTES),
            &scopes(Namespace::DEV, Flow::BSKY)
        ));
        assert!(!has_scopes("atproto", &scopes(Namespace::DEV, Flow::BASE)));
    }

    #[test]
    fn flows_round_trip_and_combine() {
        for f in [Flow::BASE, Flow::VOTES, Flow::BSKY, Flow::ALL] {
            assert_eq!(Flow::parse(f.as_str()), f);
        }
        assert_eq!(Flow::parse(""), Flow::BASE);
        assert_eq!(Flow::parse("everything"), Flow::BASE);
        assert_eq!(Flow::VOTES.with(Flow::BSKY), Flow::ALL);
        assert_eq!(Flow::BASE.with(Flow::VOTES), Flow::VOTES);
    }

    #[test]
    fn atproto_scope_check() {
        assert!(has_atproto_scope("atproto"));
        assert!(has_atproto_scope("repo:x atproto"));
        assert!(!has_atproto_scope(""));
        assert!(!has_atproto_scope("atproto2 transition:generic"));
    }
}
