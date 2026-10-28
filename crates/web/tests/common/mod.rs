pub mod world;

use std::sync::Arc;

use threaded_auth::records::{MemoryRepo, Writer};
use threaded_web::blog::Blog;
use threaded_web::AppState;

use threaded_auth::client::{build, Mode};
use threaded_auth::store::SqliteAuthStore;
use threaded_store::Store;
use threaded_web::auth::Auth;

pub const COOKIE_KEY: [u8; 64] = [7; 64];

/// App state on an in-memory database, with writes going to `repo`.
#[allow(dead_code)]
pub fn state(
    store: &Store,
    repo: Arc<MemoryRepo>,
    now: i64,
    blog_dir: &std::path::Path,
) -> AppState {
    AppState {
        auth: auth(store),
        store: store.clone(),
        blog: Arc::new(Blog::new(blog_dir, std::time::Duration::ZERO)),
        writer: Writer::Memory(repo),
        // Tests never look handles up on the network; with no answer set, a
        // check fails and keeps the stored handle.
        handles: threaded_web::handles::HandleChecker::Fixed(Default::default()),
        hydrant: None,
        // Nor the PLC directory: every DID is unknown unless a test says.
        plc: threaded_web::identity::Plc::Fixed(Default::default()),
        origin: threaded_web::origin::Origin::parse("https://threaded.test").unwrap(),
        bsky: true,
        clock: Arc::new(move || now),
        policy: Default::default(),
        triage_key: false,
        rules_ready: Default::default(),
    }
}

pub fn auth(store: &Store) -> Arc<Auth> {
    let auth_store = SqliteAuthStore::new(store.pool().clone());
    let client = build(
        Mode::Localhost {
            callback: "http://127.0.0.1:8080/oauth/callback".into(),
        },
        auth_store.clone(),
    )
    .unwrap();
    Arc::new(Auth::new(Arc::new(client), auth_store, &COOKIE_KEY, false))
}
