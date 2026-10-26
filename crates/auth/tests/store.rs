use jacquard_oauth::authstore::ClientAuthStore;
use threaded_auth::store::SqliteAuthStore;

#[tokio::test]
async fn auth_requests_round_trip_and_delete() {
    let store = threaded_store::Store::open("sqlite::memory:")
        .await
        .unwrap();
    let auth = SqliteAuthStore::new(store.pool().clone());

    assert!(auth.get_auth_req_info("nope").await.unwrap().is_none());
    assert!(!auth.session_exists("did:plc:x", "s").await.unwrap());
    assert_eq!(auth.prune().await.unwrap(), 0);
    auth.delete_auth_req_info("nope").await.unwrap();
}
