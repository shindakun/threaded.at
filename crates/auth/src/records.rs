use std::sync::{Arc, Mutex};

use jacquard_common::bos::BosStr;
use jacquard_common::types::did::Did;
use jacquard_common::xrpc::{GenericError, XrpcClient, XrpcMethod, XrpcRequest, XrpcResp};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::client::{Clients, Flow};

/// A signed-in user's OAuth session, as held in the session cookie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRef {
    pub did: String,
    pub session_id: String,
    pub flow: Flow,
}

/// A record's AT-URI and CID.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RecordRef {
    pub uri: String,
    pub cid: String,
}

impl RecordRef {
    /// The record key: the last segment of the AT-URI.
    pub fn rkey(uri: &str) -> &str {
        uri.rsplit('/').next().unwrap_or("")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WriteError {
    #[error("the session has ended; sign in again")]
    SignedOut,
    #[error("the server refused the write: {0}")]
    Pds(String),
}

#[derive(Serialize)]
struct CreateRecord<'a> {
    repo: &'a str,
    collection: &'a str,
    record: &'a serde_json::Value,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PutRecord<'a> {
    repo: &'a str,
    collection: &'a str,
    rkey: &'a str,
    record: &'a serde_json::Value,
}

#[derive(Serialize)]
struct DeleteRecord<'a> {
    repo: &'a str,
    collection: &'a str,
    rkey: &'a str,
}

struct RefResp;
impl XrpcResp for RefResp {
    const NSID: &'static str = "com.atproto.repo.createRecord";
    const ENCODING: &'static str = "application/json";
    type Output<S: BosStr> = RecordRef;
    type Err = GenericError;
}

struct EmptyResp;
impl XrpcResp for EmptyResp {
    const NSID: &'static str = "com.atproto.repo.deleteRecord";
    const ENCODING: &'static str = "application/json";
    type Output<S: BosStr> = serde_json::Value;
    type Err = GenericError;
}

impl XrpcRequest for CreateRecord<'_> {
    const NSID: &'static str = "com.atproto.repo.createRecord";
    const METHOD: XrpcMethod = XrpcMethod::Procedure("application/json");
    type Response = RefResp;
}

impl XrpcRequest for PutRecord<'_> {
    const NSID: &'static str = "com.atproto.repo.putRecord";
    const METHOD: XrpcMethod = XrpcMethod::Procedure("application/json");
    type Response = RefResp;
}

impl XrpcRequest for DeleteRecord<'_> {
    const NSID: &'static str = "com.atproto.repo.deleteRecord";
    const METHOD: XrpcMethod = XrpcMethod::Procedure("application/json");
    type Response = EmptyResp;
}

/// One write the in-memory writer received.
#[derive(Debug, Clone, PartialEq)]
pub struct Written {
    pub op: &'static str,
    pub did: String,
    pub collection: String,
    pub rkey: String,
    pub record: Option<serde_json::Value>,
}

#[derive(Default)]
pub struct MemoryRepo {
    pub writes: Mutex<Vec<Written>>,
    /// When set, every write fails with this message.
    pub fail: Mutex<Option<String>>,
    /// When set, writes to this collection fail with the message.
    pub fail_collection: Mutex<Option<(String, String)>>,
}

/// Writes records to the signed-in user's repo. Tests use `Memory`.
#[derive(Clone)]
pub enum Writer {
    Pds(Arc<Clients>),
    Memory(Arc<MemoryRepo>),
}

impl Writer {
    async fn session(
        clients: &Clients,
        s: &SessionRef,
    ) -> Result<crate::client::Session, WriteError> {
        let did = Did::new(SmolStr::new(&s.did)).map_err(|_| WriteError::SignedOut)?;
        clients
            .get(s.flow)
            .oauth
            .restore(&did, &s.session_id)
            .await
            .map_err(|_| WriteError::SignedOut)
    }

    fn memory(repo: &MemoryRepo, w: Written) -> Result<RecordRef, WriteError> {
        if let Some(msg) = repo.fail.lock().unwrap().clone() {
            return Err(WriteError::Pds(msg));
        }
        if let Some((collection, msg)) = repo.fail_collection.lock().unwrap().clone() {
            if collection == w.collection {
                return Err(WriteError::Pds(msg));
            }
        }
        let uri = format!("at://{}/{}/{}", w.did, w.collection, w.rkey);
        let mut writes = repo.writes.lock().unwrap();
        let cid = format!("bafymem{}", writes.len());
        writes.push(w);
        Ok(RecordRef { uri, cid })
    }

    pub async fn create(
        &self,
        s: &SessionRef,
        collection: &str,
        record: &serde_json::Value,
    ) -> Result<RecordRef, WriteError> {
        match self {
            Writer::Pds(clients) => {
                let session = Self::session(clients, s).await?;
                let req = CreateRecord {
                    repo: &s.did,
                    collection,
                    record,
                };
                let resp = session
                    .send(req)
                    .await
                    .map_err(|e| WriteError::Pds(e.to_string()))?;
                resp.into_output()
                    .map_err(|e| WriteError::Pds(e.to_string()))
            }
            Writer::Memory(repo) => {
                let rkey = format!("m{}", repo.writes.lock().unwrap().len());
                Self::memory(
                    repo,
                    Written {
                        op: "create",
                        did: s.did.clone(),
                        collection: collection.to_string(),
                        rkey,
                        record: Some(record.clone()),
                    },
                )
            }
        }
    }

    pub async fn put(
        &self,
        s: &SessionRef,
        collection: &str,
        rkey: &str,
        record: &serde_json::Value,
    ) -> Result<RecordRef, WriteError> {
        match self {
            Writer::Pds(clients) => {
                let session = Self::session(clients, s).await?;
                let req = PutRecord {
                    repo: &s.did,
                    collection,
                    rkey,
                    record,
                };
                let resp = session
                    .send(req)
                    .await
                    .map_err(|e| WriteError::Pds(e.to_string()))?;
                resp.into_output()
                    .map_err(|e| WriteError::Pds(e.to_string()))
            }
            Writer::Memory(repo) => Self::memory(
                repo,
                Written {
                    op: "put",
                    did: s.did.clone(),
                    collection: collection.to_string(),
                    rkey: rkey.to_string(),
                    record: Some(record.clone()),
                },
            ),
        }
    }

    pub async fn delete(
        &self,
        s: &SessionRef,
        collection: &str,
        rkey: &str,
    ) -> Result<(), WriteError> {
        match self {
            Writer::Pds(clients) => {
                let session = Self::session(clients, s).await?;
                let req = DeleteRecord {
                    repo: &s.did,
                    collection,
                    rkey,
                };
                let resp = session
                    .send(req)
                    .await
                    .map_err(|e| WriteError::Pds(e.to_string()))?;
                resp.into_output()
                    .map(|_| ())
                    .map_err(|e| WriteError::Pds(e.to_string()))
            }
            Writer::Memory(repo) => Self::memory(
                repo,
                Written {
                    op: "delete",
                    did: s.did.clone(),
                    collection: collection.to_string(),
                    rkey: rkey.to_string(),
                    record: None,
                },
            )
            .map(|_| ()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rkey_is_the_last_segment() {
        assert_eq!(
            RecordRef::rkey("at://did:plc:a/at.threaded.submission/3mabc"),
            "3mabc"
        );
    }

    #[tokio::test]
    async fn memory_writer_records_and_fails_on_demand() {
        let repo = Arc::new(MemoryRepo::default());
        let w = Writer::Memory(repo.clone());
        let s = SessionRef {
            did: "did:plc:a".into(),
            session_id: "s".into(),
            flow: Flow::BASE,
        };
        let r = w
            .create(
                &s,
                "at.threaded.dev.submission",
                &serde_json::json!({"title": "T"}),
            )
            .await
            .unwrap();
        assert_eq!(r.uri, "at://did:plc:a/at.threaded.dev.submission/m0");
        w.delete(&s, "at.threaded.dev.submission", "m0")
            .await
            .unwrap();
        assert_eq!(repo.writes.lock().unwrap().len(), 2);
        *repo.fail.lock().unwrap() = Some("down".into());
        assert_eq!(
            w.create(&s, "x", &serde_json::json!({})).await,
            Err(WriteError::Pds("down".into()))
        );
    }
}

/// RFC 3339 UTC, as the lexicons' `datetime` fields require.
pub fn datetime(unix: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp(unix)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .format(&time::format_description::well_known::Rfc3339)
        .expect("RFC 3339 formats")
}

fn strong_ref(r: &RecordRef) -> serde_json::Value {
    serde_json::json!({ "uri": r.uri, "cid": r.cid })
}

pub fn submission_record(
    ns: crate::client::Namespace,
    title: &str,
    url: Option<&str>,
    text: &str,
    tags: &[&str],
    authored: bool,
    created_at: i64,
) -> serde_json::Value {
    let mut r = serde_json::json!({
        "$type": ns.submission(),
        "title": title,
        "tags": tags,
        "authored": authored,
        "createdAt": datetime(created_at),
    });
    if let Some(u) = url {
        r["url"] = u.into();
    }
    if !text.is_empty() {
        r["text"] = text.into();
    }
    r
}

pub fn comment_record(
    ns: crate::client::Namespace,
    subject: &RecordRef,
    parent: Option<&RecordRef>,
    text: &str,
    created_at: i64,
) -> serde_json::Value {
    let mut r = serde_json::json!({
        "$type": ns.comment(),
        "subject": strong_ref(subject),
        "text": text,
        "createdAt": datetime(created_at),
    });
    if let Some(p) = parent {
        r["parent"] = strong_ref(p);
    }
    r
}

pub fn vote_record(
    ns: crate::client::Namespace,
    subject: &RecordRef,
    created_at: i64,
) -> serde_json::Value {
    serde_json::json!({
        "$type": ns.vote(),
        "subject": strong_ref(subject),
        "createdAt": datetime(created_at),
    })
}

/// A Bluesky post for a story: the title as its text, and a link card to
/// the story's page on Threaded. Bluesky allows 300 graphemes of text; a
/// grapheme is at least one character, so 300 characters always fit.
pub fn bsky_post_record(
    title: &str,
    page_url: &str,
    description: &str,
    created_at: i64,
) -> serde_json::Value {
    let text: String = title.chars().take(300).collect();
    serde_json::json!({
        "$type": crate::client::BSKY_POST,
        "text": text,
        "createdAt": datetime(created_at),
        "embed": {
            "$type": "app.bsky.embed.external",
            "external": {
                "uri": page_url,
                "title": title,
                "description": description,
            },
        },
    })
}

#[cfg(test)]
mod record_tests {
    use super::*;
    use crate::client::Namespace;

    #[test]
    fn a_bsky_post_keeps_its_text_within_the_limit() {
        let long = "é".repeat(400);
        let r = bsky_post_record(&long, "https://threaded.at/s/abc", "", 0);
        assert_eq!(r["text"].as_str().unwrap().chars().count(), 300);
        assert_eq!(r["embed"]["external"]["title"], long.as_str());
        assert_eq!(r["$type"], "app.bsky.feed.post");
    }

    #[test]
    fn datetimes_are_rfc3339_utc() {
        assert_eq!(datetime(1_790_000_000), "2026-09-21T14:13:20Z");
    }

    #[test]
    fn optional_fields_are_omitted() {
        let r = submission_record(Namespace::DEV, "T", None, "", &["rust"], false, 0);
        assert_eq!(r["$type"], "at.threaded.dev.submission");
        assert!(r.get("url").is_none() && r.get("text").is_none());
        let c = comment_record(
            Namespace::PRODUCTION,
            &RecordRef {
                uri: "at://a/b/c".into(),
                cid: "x".into(),
            },
            None,
            "hi",
            0,
        );
        assert!(c.get("parent").is_none());
        assert_eq!(c["subject"]["uri"], "at://a/b/c");
    }
}
