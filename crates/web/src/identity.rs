//! An account's identity beyond Threaded, for the user review page: its PDS
//! host from hydrant and, for a `did:plc`, when the PLC directory first saw
//! it. Both are fetched when the page loads, with a short wait, and the page
//! says so when either does not answer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;

use crate::AppState;

/// How long the review page waits for hydrant and the PLC directory.
const WAIT: Duration = Duration::from_secs(3);

/// The PLC directory, unless `THREADED_PLC_URL` names another.
pub const PLC_DIRECTORY: &str = "https://plc.directory";

/// What a lookup found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found<T> {
    Yes(T),
    /// The source answered and has nothing for this account.
    No,
    /// The lookup does not apply, such as a PLC lookup for a `did:web`.
    NotApplicable,
    /// The source is off or did not answer; the text says which.
    Failed(String),
}

/// Where account creation times come from: the PLC directory, or fixed
/// answers in tests.
#[derive(Clone)]
pub enum Plc {
    /// A directory at this origin.
    Directory(reqwest::Client, crate::origin::Origin),
    /// Unix times by DID; a DID with no entry is unknown to the directory.
    Fixed(Arc<Mutex<HashMap<String, i64>>>),
}

impl Plc {
    pub fn directory() -> Plc {
        Plc::directory_at(
            crate::origin::Origin::parse(PLC_DIRECTORY).expect("the directory is an origin"),
        )
    }

    pub fn directory_at(url: crate::origin::Origin) -> Plc {
        Plc::Directory(
            reqwest::Client::builder()
                .user_agent("threaded")
                .timeout(WAIT)
                .build()
                .unwrap_or_default(),
            url,
        )
    }

    /// When the directory first saw a `did:plc`: its genesis operation.
    pub async fn created_at(&self, did: &str) -> Found<i64> {
        if !is_plc(did) {
            return Found::NotApplicable;
        }
        match self {
            Plc::Fixed(answers) => answers
                .lock()
                .unwrap()
                .get(did)
                .map_or(Found::No, |t| Found::Yes(*t)),
            Plc::Directory(http, url) => match genesis(http, url, did).await {
                Ok(Some(t)) => Found::Yes(t),
                Ok(None) => Found::No,
                Err(e) => Found::Failed(format!("The PLC directory did not answer: {e}.")),
            },
        }
    }
}

/// A `did:plc` is `did:plc:` and 24 base32 characters. Checked before the
/// DID goes into a URL.
fn is_plc(did: &str) -> bool {
    did.strip_prefix("did:plc:").is_some_and(|id| {
        id.len() == 24
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
    })
}

#[derive(Deserialize)]
struct AuditEntry {
    #[serde(rename = "createdAt")]
    created_at: String,
}

async fn genesis(
    http: &reqwest::Client,
    directory: &crate::origin::Origin,
    did: &str,
) -> Result<Option<i64>, String> {
    let resp = http
        .get(directory.url(&format!("/{did}/log/audit")))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(format!("status {}", resp.status()));
    }
    // The log is oldest first; the first entry is the genesis operation.
    let log: Vec<AuditEntry> = resp.json().await.map_err(|e| e.to_string())?;
    let Some(first) = log.first() else {
        return Ok(None);
    };
    time::OffsetDateTime::parse(
        &first.created_at,
        &time::format_description::well_known::Rfc3339,
    )
    .map(|t| Some(t.unix_timestamp()))
    .map_err(|e| format!("unreadable time {:?}: {e}", first.created_at))
}

/// The account's PDS host, as hydrant read it from the DID document.
pub async fn pds_host(st: &AppState, did: &str) -> Found<String> {
    let Some(h) = &st.hydrant else {
        return Found::Failed("The indexer is off.".into());
    };
    match tokio::time::timeout(WAIT, h.repo(did)).await {
        Ok(Ok(Some(r))) => match r.pds {
            Some(p) => Found::Yes(host(&p)),
            None => Found::No,
        },
        Ok(Ok(None)) => Found::No,
        Ok(Err(e)) => Found::Failed(format!("hydrant did not answer: {e}.")),
        Err(_) => Found::Failed("hydrant did not answer in time.".into()),
    }
}

/// `https://pds.example.com/` as `pds.example.com`.
fn host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split('/').next().unwrap_or(rest).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plc_dids() {
        assert!(is_plc("did:plc:z72i7hdynmk6r22z27h6tvur"));
        assert!(!is_plc("did:plc:z72i7hdynmk6r22z27h6tvu"));
        assert!(!is_plc("did:plc:z72i7hdynmk6r22z27h6tvu/"));
        assert!(!is_plc("did:plc:Z72I7HDYNMK6R22Z27H6TVUR"));
        assert!(!is_plc("did:web:example.com"));
    }

    #[test]
    fn hosts() {
        assert_eq!(host("https://pds.example.com/"), "pds.example.com");
        assert_eq!(host("https://pds.example.com:8443"), "pds.example.com:8443");
        assert_eq!(host("pds.example.com"), "pds.example.com");
    }
}
