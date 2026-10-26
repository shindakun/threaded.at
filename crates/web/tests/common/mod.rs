use std::sync::Arc;

use threaded_auth::client::{build, Mode};
use threaded_auth::store::SqliteAuthStore;
use threaded_store::Store;
use threaded_web::auth::Auth;

pub const COOKIE_KEY: [u8; 64] = [7; 64];

pub fn auth(store: &Store) -> Arc<Auth> {
    let auth_store = SqliteAuthStore::new(store.pool().clone());
    let client = build(
        Mode::Localhost {
            callback: "http://127.0.0.1:8080/oauth/callback".into(),
        },
        auth_store.clone(),
    )
    .unwrap();
    Arc::new(Auth::new(client, auth_store, &COOKIE_KEY, false))
}
