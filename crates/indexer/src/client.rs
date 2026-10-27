//! A client for hydrant's API on a Unix socket: the ordered event stream,
//! its head, and repository tracking. hydrant serves its management endpoints
//! only on a socket, never on TCP, so this client speaks HTTP/1.1 and the
//! websocket protocol over a `UnixStream`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use http::{header, Method, Request, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use tokio::net::UnixStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// DIDs per `PUT /repos` request.
const TRACK_BATCH: usize = 500;

/// How long a plain request may take, connect included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("connecting to hydrant at {0}: {1}")]
    Connect(String, std::io::Error),
    #[error("talking to hydrant: {0}")]
    Http(String),
    #[error("hydrant answered {0}: {1}")]
    Status(StatusCode, String),
    #[error("hydrant timed out")]
    Timeout,
    #[error("hydrant sent something unreadable: {0}")]
    Decode(String),
    /// hydrant closed the stream with an error frame, such as `ConsumerTooSlow`.
    #[error("hydrant ended the stream: {0}: {1}")]
    StreamError(String, String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Create,
    Update,
    Delete,
}

/// A record created, updated, or deleted in a repository.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct RecordEvent {
    /// `true` for live ingestion, `false` for backfill.
    pub live: bool,
    pub did: String,
    pub rev: String,
    pub collection: String,
    pub rkey: String,
    pub action: Action,
    /// The record body; absent for deletes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cid: Option<String>,
}

impl RecordEvent {
    pub fn uri(&self) -> String {
        format!("at://{}/{}/{}", self.did, self.collection, self.rkey)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct IdentityEvent {
    pub did: String,
    #[serde(default)]
    pub handle: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AccountEvent {
    pub did: String,
    pub active: bool,
    #[serde(default)]
    pub status: Option<String>,
}

/// One event from `/stream`. IDs rise but may skip numbers. Identity and
/// account events are live only: a replay from a cursor does not repeat them.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Record { id: u64, record: RecordEvent },
    Identity { id: u64, identity: IdentityEvent },
    Account { id: u64, account: AccountEvent },
}

impl Event {
    pub fn id(&self) -> u64 {
        match self {
            Event::Record { id, .. } | Event::Identity { id, .. } | Event::Account { id, .. } => {
                *id
            }
        }
    }
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    id: Option<u64>,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    record: Option<RecordEvent>,
    #[serde(default)]
    identity: Option<IdentityEvent>,
    #[serde(default)]
    account: Option<AccountEvent>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// What a text frame held: an event, an event of a kind this client does not
/// know (skipped), or hydrant's error frame.
pub(crate) fn decode_frame(text: &str) -> Result<Option<Event>> {
    let e: Envelope = serde_json::from_str(text).map_err(|err| Error::Decode(err.to_string()))?;
    let need_id = || {
        e.id.ok_or_else(|| Error::Decode(format!("a {} event without an id", e.kind)))
    };
    let missing = |what: &str| Error::Decode(format!("a {what} event without its {what} payload"));
    match e.kind.as_str() {
        "record" => Ok(Some(Event::Record {
            id: need_id()?,
            record: e.record.clone().ok_or_else(|| missing("record"))?,
        })),
        "identity" => Ok(Some(Event::Identity {
            id: need_id()?,
            identity: e.identity.clone().ok_or_else(|| missing("identity"))?,
        })),
        "account" => Ok(Some(Event::Account {
            id: need_id()?,
            account: e.account.clone().ok_or_else(|| missing("account"))?,
        })),
        "error" => Err(Error::StreamError(
            e.error.unwrap_or_else(|| "unknown".into()),
            e.message.unwrap_or_default(),
        )),
        other => {
            tracing::debug!(kind = other, "skipping a stream event of an unknown kind");
            Ok(None)
        }
    }
}

/// What `GET /repos/{did}` says about one repository.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RepoInfo {
    pub did: String,
    pub status: String,
    #[serde(default)]
    pub handle: Option<String>,
    /// The repository's PDS, as its DID document names it.
    #[serde(default)]
    pub pds: Option<String>,
    #[serde(default)]
    pub tracked: bool,
    /// A backfill is queued or running; its records may be mid-write.
    #[serde(default)]
    pub pending: bool,
}

#[derive(Serialize)]
struct DidEntry<'a> {
    did: &'a str,
}

#[derive(Deserialize)]
struct Head {
    id: Option<u64>,
}

/// hydrant's API on a Unix socket.
#[derive(Debug, Clone)]
pub struct Hydrant {
    socket: PathBuf,
}

impl Hydrant {
    pub fn new(socket: impl AsRef<Path>) -> Hydrant {
        Hydrant {
            socket: socket.as_ref().to_path_buf(),
        }
    }

    pub fn socket(&self) -> &Path {
        &self.socket
    }

    async fn connect(&self) -> Result<UnixStream> {
        UnixStream::connect(&self.socket)
            .await
            .map_err(|e| Error::Connect(self.socket.display().to_string(), e))
    }

    /// One request on a fresh connection; hydrant is local, so connecting is cheap.
    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<(StatusCode, Bytes)> {
        let work = async {
            let io = TokioIo::new(self.connect().await?);
            let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
                .await
                .map_err(|e| Error::Http(e.to_string()))?;
            tokio::spawn(async move {
                if let Err(e) = conn.await {
                    tracing::debug!(error = %e, "hydrant connection closed");
                }
            });
            let mut req = Request::builder()
                .method(method)
                .uri(path)
                .header(header::HOST, "localhost")
                .header(header::ACCEPT, "application/json");
            if body.is_some() {
                req = req.header(header::CONTENT_TYPE, "application/json");
            }
            let req = req
                .body(Full::new(Bytes::from(body.unwrap_or_default())))
                .map_err(|e| Error::Http(e.to_string()))?;
            let res = sender
                .send_request(req)
                .await
                .map_err(|e| Error::Http(e.to_string()))?;
            let status = res.status();
            let bytes = res
                .into_body()
                .collect()
                .await
                .map_err(|e| Error::Http(e.to_string()))?
                .to_bytes();
            Ok((status, bytes))
        };
        tokio::time::timeout(REQUEST_TIMEOUT, work)
            .await
            .map_err(|_| Error::Timeout)?
    }

    async fn json<T: serde::de::DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<T> {
        let (status, bytes) = self.request(method, path, body).await?;
        if !status.is_success() {
            return Err(Error::Status(
                status,
                String::from_utf8_lossy(&bytes).chars().take(300).collect(),
            ));
        }
        serde_json::from_slice(&bytes).map_err(|e| Error::Decode(e.to_string()))
    }

    /// The ID of the newest durable event, or `None` while hydrant has none.
    pub async fn stream_head(&self) -> Result<Option<u64>> {
        Ok(self
            .json::<Head>(Method::GET, "/stream/head", None)
            .await?
            .id)
    }

    /// A repository's sync state, or `None` when hydrant does not know it.
    pub async fn repo(&self, did: &str) -> Result<Option<RepoInfo>> {
        if !did.starts_with("did:") || did.contains(['/', '?', '#', ' ']) {
            return Err(Error::Decode(format!("not a DID: {did}")));
        }
        let path = format!("/repos/{did}");
        let (status, bytes) = self.request(Method::GET, &path, None).await?;
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(Error::Status(
                status,
                String::from_utf8_lossy(&bytes).chars().take(300).collect(),
            ));
        }
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| Error::Decode(e.to_string()))
    }

    /// Asks hydrant to track these repositories. Returns the DIDs it queued
    /// for backfill; ones it already tracks are left out. Sent in batches,
    /// since hydrant caps a request body at 8 MiB.
    pub async fn track(&self, dids: &[&str]) -> Result<Vec<String>> {
        let mut queued = Vec::new();
        for batch in dids.chunks(TRACK_BATCH) {
            let body: Vec<DidEntry> = batch.iter().map(|d| DidEntry { did: d }).collect();
            let body = serde_json::to_vec(&body).map_err(|e| Error::Decode(e.to_string()))?;
            queued.extend(
                self.json::<Vec<String>>(Method::PUT, "/repos", Some(body))
                    .await?,
            );
        }
        Ok(queued)
    }

    /// Opens `/stream`. With a cursor, hydrant first replays durable record
    /// events from that ID on, then sends live events; without one, live only.
    pub async fn stream(&self, cursor: Option<u64>) -> Result<EventStream> {
        let path = match cursor {
            Some(c) => format!("ws://localhost/stream?cursor={c}"),
            None => "ws://localhost/stream".to_string(),
        };
        let io = self.connect().await?;
        let (ws, _) =
            tokio::time::timeout(REQUEST_TIMEOUT, tokio_tungstenite::client_async(path, io))
                .await
                .map_err(|_| Error::Timeout)?
                .map_err(|e| Error::Http(e.to_string()))?;
        Ok(EventStream { ws })
    }
}

/// An open `/stream` connection.
pub struct EventStream {
    ws: WebSocketStream<UnixStream>,
}

impl EventStream {
    /// The next event, or `None` once hydrant closes the connection. Pings
    /// are answered as they arrive; this client never sends data frames,
    /// which hydrant would close the connection for.
    pub async fn next(&mut self) -> Option<Result<Event>> {
        loop {
            let msg = match self.ws.next().await? {
                Ok(m) => m,
                Err(e) => return Some(Err(Error::Http(e.to_string()))),
            };
            match msg {
                Message::Text(t) => match decode_frame(t.as_str()) {
                    Ok(Some(event)) => return Some(Ok(event)),
                    Ok(None) => continue,
                    Err(e) => return Some(Err(e)),
                },
                Message::Close(_) => return None,
                // Pings get their pong from tungstenite on the next read or write.
                Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => continue,
                Message::Binary(_) => {
                    return Some(Err(Error::Decode("an unexpected binary frame".into())))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_decode_as_documented() {
        let r = decode_frame(
            r#"{"id":42,"type":"record","record":{"live":true,"did":"did:plc:a","rev":"3k","collection":"at.threaded.submission","rkey":"3kx","action":"create","record":{"title":"T"},"cid":"bafy"}}"#,
        )
        .unwrap()
        .unwrap();
        let Event::Record { id, record } = r else {
            panic!("{r:?}")
        };
        assert_eq!(id, 42);
        assert_eq!(record.action, Action::Create);
        assert_eq!(record.uri(), "at://did:plc:a/at.threaded.submission/3kx");
        assert_eq!(record.record.unwrap()["title"], "T");

        let d = decode_frame(
            r#"{"id":43,"type":"record","record":{"live":false,"did":"did:plc:a","rev":"3l","collection":"at.threaded.comment","rkey":"3ky","action":"delete"}}"#,
        )
        .unwrap()
        .unwrap();
        assert!(matches!(
            d,
            Event::Record {
                record: RecordEvent {
                    action: Action::Delete,
                    record: None,
                    cid: None,
                    ..
                },
                ..
            }
        ));

        assert_eq!(
            decode_frame(r#"{"id":44,"type":"identity","identity":{"did":"did:plc:a"}}"#).unwrap(),
            Some(Event::Identity {
                id: 44,
                identity: IdentityEvent {
                    did: "did:plc:a".into(),
                    handle: None
                }
            })
        );
        assert_eq!(
            decode_frame(r#"{"id":45,"type":"account","account":{"did":"did:plc:a","active":false,"status":"deactivated"}}"#)
                .unwrap()
                .unwrap()
                .id(),
            45
        );
    }

    #[test]
    fn errors_and_unknown_kinds() {
        let e = decode_frame(
            r#"{"type":"error","error":"ConsumerTooSlow","message":"stream socket send blocked"}"#,
        )
        .unwrap_err();
        assert!(
            matches!(e, Error::StreamError(ref k, _) if k == "ConsumerTooSlow"),
            "{e}"
        );
        assert_eq!(decode_frame(r#"{"id":46,"type":"labels"}"#).unwrap(), None);
        assert!(
            decode_frame(r#"{"id":47,"type":"record"}"#).is_err(),
            "a record event needs its payload"
        );
        assert!(decode_frame("not json").is_err());
    }
}
