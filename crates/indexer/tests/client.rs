//! The client against a fake hydrant on a Unix socket, shaped like the real
//! API: JSON over HTTP/1.1, and `/stream` as a websocket with ping frames.

use std::sync::{Arc, Mutex};

use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use serde_json::{json, Value};
use threaded_indexer::client::{Action, Error, Event, Hydrant};

#[derive(Default)]
struct Seen {
    cursors: Vec<Option<String>>,
    tracked: Vec<Vec<String>>,
}

type Shared = Arc<Mutex<Seen>>;

const KNOWN: &str = "did:plc:known";

async fn head() -> Json<Value> {
    Json(json!({"id": 7}))
}

async fn repo(Path(did): Path<String>) -> Response {
    if did == KNOWN {
        Json(json!({
            "did": KNOWN, "status": "synced", "tracked": true, "pending": false,
            "handle": "known.example", "pds": "https://pds.example/", "signing_key": "zQ3sh"
        }))
        .into_response()
    } else {
        (StatusCode::NOT_FOUND, "repo not found").into_response()
    }
}

async fn track(State(seen): State<Shared>, Json(body): Json<Vec<Value>>) -> Json<Vec<String>> {
    let dids: Vec<String> = body
        .iter()
        .map(|v| v["did"].as_str().unwrap().to_string())
        .collect();
    seen.lock().unwrap().tracked.push(dids.clone());
    Json(dids.into_iter().filter(|d| d != KNOWN).collect())
}

async fn stream(
    State(seen): State<Shared>,
    Query(q): Query<std::collections::HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    seen.lock().unwrap().cursors.push(q.get("cursor").cloned());
    ws.on_upgrade(|mut socket| async move {
        let frames = [
            json!({"id": 3, "type": "record", "record": {
                "live": false, "did": KNOWN, "rev": "3k", "collection": "at.threaded.submission",
                "rkey": "3ka", "action": "create", "record": {"title": "Hello"}, "cid": "bafy1"}}),
            json!({"id": 5, "type": "labels", "labels": {}}),
            json!({"id": 6, "type": "identity", "identity": {"did": KNOWN, "handle": "new.example"}}),
            json!({"id": 9, "type": "record", "record": {
                "live": true, "did": KNOWN, "rev": "3l", "collection": "at.threaded.submission",
                "rkey": "3ka", "action": "delete"}}),
            json!({"type": "error", "error": "ConsumerTooSlow", "message": "stream socket send blocked"}),
        ];
        let _ = socket.send(Message::Ping(vec![1, 2].into())).await;
        for f in frames {
            if socket.send(Message::Text(f.to_string().into())).await.is_err() {
                return;
            }
        }
        // Like hydrant, keep the connection open until the client leaves.
        while let Some(Ok(_)) = socket.recv().await {}
    })
}

/// Starts a fake hydrant and returns a client for it and what it saw.
async fn fake() -> (Hydrant, Shared, tempfile_dir::Dir) {
    let dir = tempfile_dir::Dir::new();
    let socket = dir.path().join("api.sock");
    let seen: Shared = Arc::default();
    let app = Router::new()
        .route("/stream/head", get(head))
        .route("/repos/{did}", get(repo))
        .route("/repos", put(track))
        .route("/stream", get(stream))
        .with_state(seen.clone());
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (Hydrant::new(&socket), seen, dir)
}

/// A temporary directory, removed when dropped.
mod tempfile_dir {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    pub struct Dir(std::path::PathBuf);
    impl Dir {
        pub fn new() -> Dir {
            let p = std::env::temp_dir().join(format!(
                "threaded-indexer-{}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
        pub fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[tokio::test]
async fn plain_requests() {
    let (h, seen, _dir) = fake().await;
    assert_eq!(h.stream_head().await.unwrap(), Some(7));

    let r = h.repo(KNOWN).await.unwrap().unwrap();
    assert_eq!(r.handle.as_deref(), Some("known.example"));
    assert_eq!(r.pds.as_deref(), Some("https://pds.example/"));
    assert_eq!(r.status, "synced");
    assert_eq!(h.repo("did:plc:nobody").await.unwrap(), None);
    assert!(
        h.repo("did:plc:a/../x").await.is_err(),
        "a DID with a path is refused before sending"
    );

    let queued = h.track(&[KNOWN, "did:plc:new"]).await.unwrap();
    assert_eq!(queued, vec!["did:plc:new".to_string()]);
    assert_eq!(h.track(&[]).await.unwrap(), Vec::<String>::new());
    assert_eq!(
        seen.lock().unwrap().tracked.len(),
        1,
        "an empty list sends nothing"
    );
}

#[tokio::test]
async fn tracking_is_sent_in_batches() {
    let (h, seen, _dir) = fake().await;
    let dids: Vec<String> = (0..1_201).map(|n| format!("did:plc:n{n}")).collect();
    let refs: Vec<&str> = dids.iter().map(String::as_str).collect();
    assert_eq!(h.track(&refs).await.unwrap().len(), 1_201);
    let sizes: Vec<usize> = seen.lock().unwrap().tracked.iter().map(Vec::len).collect();
    assert_eq!(sizes, vec![500, 500, 201]);
}

#[tokio::test]
async fn the_stream_yields_events_in_order_and_ends_on_an_error_frame() {
    let (h, seen, _dir) = fake().await;
    let mut s = h.stream(Some(3)).await.unwrap();

    let first = s.next().await;
    let Some(Ok(Event::Record { id, record })) = first else {
        panic!("first event: {first:?}")
    };
    assert_eq!((id, record.action, record.live), (3, Action::Create, false));
    assert_eq!(record.record.unwrap()["title"], "Hello");

    // The unknown "labels" event is skipped.
    let Some(Ok(Event::Identity { id, identity })) = s.next().await else {
        panic!("identity")
    };
    assert_eq!((id, identity.handle.as_deref()), (6, Some("new.example")));

    let Some(Ok(Event::Record { id, record })) = s.next().await else {
        panic!("delete")
    };
    assert_eq!(
        (id, record.action, record.record),
        (9, Action::Delete, None)
    );

    match s.next().await {
        Some(Err(Error::StreamError(kind, _))) => assert_eq!(kind, "ConsumerTooSlow"),
        other => panic!("expected the error frame, got {other:?}"),
    }
    assert_eq!(seen.lock().unwrap().cursors, vec![Some("3".to_string())]);

    let _live = h.stream(None).await.unwrap();
    assert_eq!(
        seen.lock().unwrap().cursors[1],
        None,
        "no cursor means live only"
    );
}

#[tokio::test]
async fn a_missing_socket_says_where_it_looked() {
    let h = Hydrant::new("/nonexistent/hydrant.sock");
    let e = h.stream_head().await.unwrap_err();
    assert!(e.to_string().contains("/nonexistent/hydrant.sock"), "{e}");
    assert!(h.stream(Some(0)).await.is_err());
}
