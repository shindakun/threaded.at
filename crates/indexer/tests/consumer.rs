//! The stream reader: cursor, waiting records, and reconnecting, directly
//! and against a fake hydrant on a Unix socket.

use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use serde_json::{json, Value};
use threaded_core::policy::Policy;
use threaded_indexer::apply::Collections;
use threaded_indexer::client::{Action, Event, Hydrant, RecordEvent};
use threaded_indexer::consumer::PENDING_TTL_SECS;
use threaded_indexer::Indexer;
use threaded_store::Store;

const NOW: i64 = 1_790_000_000;
const SUB: &str = "at.threaded.submission";
const COM: &str = "at.threaded.comment";

fn record(
    id: u64,
    did: &str,
    collection: &str,
    rkey: &str,
    action: Action,
    body: Option<Value>,
) -> Event {
    Event::Record {
        id,
        record: RecordEvent {
            live: true,
            did: did.into(),
            rev: "3k".into(),
            collection: collection.into(),
            rkey: rkey.into(),
            action,
            cid: body.as_ref().map(|_| format!("cid-{rkey}")),
            record: body,
        },
    }
}

fn story_body() -> Value {
    json!({"$type": SUB, "title": "A story", "url": "https://e.example/s", "tags": ["rust"],
           "createdAt": "2026-09-01T00:00:00Z"})
}

fn comment_body(subject: &str, parent: Option<&str>, text: &str) -> Value {
    let mut b = json!({"$type": COM, "subject": {"uri": subject, "cid": "x"}, "text": text,
                       "createdAt": "2026-09-01T00:00:00Z"});
    if let Some(p) = parent {
        b["parent"] = json!({"uri": p, "cid": "x"});
    }
    b
}

const STORY: &str = "at://did:plc:alice/at.threaded.submission/s1";
const PARENT: &str = "at://did:plc:bob/at.threaded.comment/c1";

async fn store() -> Store {
    let s = Store::open("sqlite::memory:").await.unwrap();
    s.upsert_tag("rust", "", 0.0).await.unwrap();
    s
}

fn indexer(store: &Store, socket: &std::path::Path, clock: Arc<AtomicI64>) -> Indexer {
    Indexer::new(
        store.clone(),
        Hydrant::new(socket),
        Collections::under("at.threaded"),
        Arc::new(Policy::default),
        Arc::new(|| false),
        Arc::new(move || clock.load(Ordering::Relaxed)),
    )
}

async fn count(s: &Store, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(s.pool()).await.unwrap()
}

#[tokio::test]
async fn the_cursor_moves_past_every_event() {
    let s = store().await;
    let ix = indexer(
        &s,
        std::path::Path::new("/unused"),
        Arc::new(AtomicI64::new(NOW)),
    );
    assert_eq!(s.stream_cursor().await.unwrap(), None);
    ix.handle(&record(
        7,
        "did:plc:alice",
        SUB,
        "s1",
        Action::Create,
        Some(story_body()),
    ))
    .await
    .unwrap();
    assert_eq!(s.stream_cursor().await.unwrap(), Some(8));
    let identity = Event::Identity {
        id: 12,
        identity: threaded_indexer::client::IdentityEvent {
            did: "did:plc:alice".into(),
            handle: None,
        },
    };
    ix.handle(&identity).await.unwrap();
    assert_eq!(s.stream_cursor().await.unwrap(), Some(13));
}

#[tokio::test]
async fn records_that_arrive_early_wait_and_then_apply_in_a_chain() {
    let s = store().await;
    let ix = indexer(
        &s,
        std::path::Path::new("/unused"),
        Arc::new(AtomicI64::new(NOW)),
    );
    // The worst order: the reply, then its parent, then the story.
    ix.handle(&record(
        1,
        "did:plc:alice",
        COM,
        "c2",
        Action::Create,
        Some(comment_body(STORY, Some(PARENT), "Reply")),
    ))
    .await
    .unwrap();
    ix.handle(&record(
        2,
        "did:plc:bob",
        COM,
        "c1",
        Action::Create,
        Some(comment_body(STORY, None, "Parent")),
    ))
    .await
    .unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM pending_records").await, 2);
    assert_eq!(count(&s, "SELECT count(*) FROM comments").await, 0);

    ix.handle(&record(
        3,
        "did:plc:alice",
        SUB,
        "s1",
        Action::Create,
        Some(story_body()),
    ))
    .await
    .unwrap();
    assert_eq!(
        count(&s, "SELECT count(*) FROM comments").await,
        2,
        "both apply once the story arrives"
    );
    assert_eq!(count(&s, "SELECT count(*) FROM pending_records").await, 0);
    assert_eq!(
        count(&s, "SELECT depth FROM comments WHERE body = 'Reply'").await,
        1
    );
}

#[tokio::test]
async fn a_waiting_record_deleted_before_its_subject_is_dropped() {
    let s = store().await;
    let ix = indexer(
        &s,
        std::path::Path::new("/unused"),
        Arc::new(AtomicI64::new(NOW)),
    );
    ix.handle(&record(
        1,
        "did:plc:bob",
        COM,
        "c1",
        Action::Create,
        Some(comment_body(STORY, None, "Parent")),
    ))
    .await
    .unwrap();
    ix.handle(&record(2, "did:plc:bob", COM, "c1", Action::Delete, None))
        .await
        .unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM pending_records").await, 0);
    ix.handle(&record(
        3,
        "did:plc:alice",
        SUB,
        "s1",
        Action::Create,
        Some(story_body()),
    ))
    .await
    .unwrap();
    assert_eq!(
        count(&s, "SELECT count(*) FROM comments").await,
        0,
        "the deleted comment never appears"
    );
}

#[tokio::test]
async fn records_that_wait_a_day_are_dropped() {
    let s = store().await;
    let clock = Arc::new(AtomicI64::new(NOW));
    let ix = indexer(&s, std::path::Path::new("/unused"), clock.clone());
    ix.handle(&record(
        1,
        "did:plc:bob",
        COM,
        "c1",
        Action::Create,
        Some(comment_body(STORY, None, "Parent")),
    ))
    .await
    .unwrap();
    clock.store(NOW + PENDING_TTL_SECS + 1_000, Ordering::Relaxed);
    ix.handle(&record(
        2,
        "did:plc:x",
        "app.bsky.feed.post",
        "p",
        Action::Create,
        Some(json!({})),
    ))
    .await
    .unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM pending_records").await, 0);
}

// ---------- against a fake hydrant ----------

#[derive(Clone)]
struct Fake {
    events: Arc<Vec<Event>>,
    cursors: Arc<Mutex<Vec<u64>>>,
    connections: Arc<AtomicUsize>,
}

fn frame(e: &Event) -> String {
    match e {
        Event::Record { id, record } => {
            json!({"id": id, "type": "record", "record": record}).to_string()
        }
        _ => unreachable!(),
    }
}

async fn stream(
    State(f): State<Fake>,
    Query(q): Query<std::collections::HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    let cursor: u64 = q.get("cursor").and_then(|c| c.parse().ok()).unwrap_or(0);
    f.cursors.lock().unwrap().push(cursor);
    let first = f.connections.fetch_add(1, Ordering::SeqCst) == 0;
    ws.on_upgrade(move |mut socket| async move {
        for e in f.events.iter().filter(|e| e.id() >= cursor) {
            if socket.send(Message::Text(frame(e).into())).await.is_err() {
                return;
            }
        }
        if first {
            // Drop the first connection, as a restart of hydrant would.
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
        while let Some(Ok(_)) = socket.recv().await {}
    })
}

#[tokio::test]
async fn the_reader_applies_the_stream_and_reconnects_from_its_cursor() {
    let dir = std::env::temp_dir().join(format!("threaded-consumer-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("api.sock");
    let _ = std::fs::remove_file(&socket);
    let fake = Fake {
        events: Arc::new(vec![
            record(
                1,
                "did:plc:alice",
                SUB,
                "s1",
                Action::Create,
                Some(story_body()),
            ),
            record(
                3,
                "did:plc:bob",
                COM,
                "c1",
                Action::Create,
                Some(comment_body(STORY, None, "Hello")),
            ),
        ]),
        cursors: Arc::default(),
        connections: Arc::default(),
    };
    let app = Router::new()
        .route("/stream", get(stream))
        .with_state(fake.clone());
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let s = store().await;
    let task = tokio::spawn(indexer(&s, &socket, Arc::new(AtomicI64::new(NOW))).run());
    for _ in 0..150 {
        if fake.connections.load(Ordering::SeqCst) >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    task.abort();
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(count(&s, "SELECT count(*) FROM stories").await, 1);
    assert_eq!(count(&s, "SELECT count(*) FROM comments").await, 1);
    assert_eq!(
        *fake.cursors.lock().unwrap(),
        vec![0, 4],
        "a fresh database starts at 0; after the drop it resumes past the last event"
    );
}
