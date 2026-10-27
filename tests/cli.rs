//! The `threaded` command line, run as an operator would run it.

use std::path::Path;
use std::process::{Command, Output};

use threaded_store::Store;

fn threaded(db: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_threaded"))
        .args(args)
        .env("THREADED_DB", format!("sqlite:{}", db.display()))
        .env("RUST_LOG", "error")
        .output()
        .unwrap()
}

/// Everything the command printed, on either stream.
fn said(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

async fn store(db: &Path) -> Store {
    Store::open(&format!("sqlite:{}", db.display()))
        .await
        .unwrap()
}

async fn text(s: &Store, sql: &str) -> String {
    sqlx::query_scalar::<_, Option<String>>(sql)
        .fetch_one(s.pool())
        .await
        .unwrap()
        .unwrap_or_default()
}

#[tokio::test]
async fn roles_are_granted_and_revoked_by_did_only() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let s = store(&db).await;

    let o = threaded(&db, &["role", "grant", "did:plc:carol", "admin"]);
    assert!(!o.status.success());
    assert!(
        said(&o).contains("no account with DID did:plc:carol has signed in yet"),
        "{}",
        said(&o)
    );

    s.record_sign_in("did:plc:carol", "carol.example", true, 0)
        .await
        .unwrap();
    let o = threaded(&db, &["role", "grant", "carol.example", "admin"]);
    assert!(!o.status.success());
    assert!(
        said(&o).contains("is not a DID; roles take a DID"),
        "{}",
        said(&o)
    );
    let o = threaded(&db, &["role", "grant", "did:plc:carol", "emperor"]);
    assert!(!o.status.success());
    assert!(
        said(&o).contains("the role is moderator or admin"),
        "{}",
        said(&o)
    );

    let o = threaded(&db, &["role", "grant", "did:plc:carol", "admin"]);
    assert!(o.status.success(), "{}", said(&o));
    assert!(said(&o).contains("did:plc:carol: user to admin"));
    let role = s.user_by_did("did:plc:carol").await.unwrap().unwrap().role;
    assert_eq!(role, "admin");
    let o = threaded(&db, &["role", "grant", "did:plc:carol", "admin"]);
    assert!(o.status.success());
    assert!(said(&o).contains("did:plc:carol is already admin"));

    let o = threaded(&db, &["role", "revoke", "did:plc:carol"]);
    assert!(o.status.success(), "{}", said(&o));
    assert!(said(&o).contains("did:plc:carol: admin to user"));
    assert_eq!(
        text(
            &s,
            "SELECT group_concat(actor || ':' || action, ',') FROM mod_log"
        )
        .await,
        "console:grant role,console:revoke role"
    );

    let o = threaded(&db, &["role"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(said(&o).contains("usage: threaded"));
}

#[tokio::test]
async fn tags_are_added_updated_and_listed() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");

    let o = threaded(
        &db,
        &["tag", "add", "rust", "--description", "The Rust language"],
    );
    assert!(o.status.success(), "{}", said(&o));
    let o = threaded(
        &db,
        &[
            "tag",
            "add",
            "rust",
            "--description",
            "Rust and its ecosystem",
            "--boost",
            "-0.5",
        ],
    );
    assert!(o.status.success(), "{}", said(&o));
    let o = threaded(&db, &["tag", "list"]);
    let out = said(&o);
    assert!(
        out.contains("rust") && out.contains("-0.5") && out.contains("Rust and its ecosystem"),
        "{out}"
    );
    assert!(!out.contains("The Rust language"), "add updates in place");

    for (args, want) in [
        (vec!["tag", "add", "Bad Name"], "is not a valid tag name"),
        (
            vec!["tag", "add", "x", "--boost", "big"],
            "--boost needs a number",
        ),
        (
            vec!["tag", "add", "x", "--boost", "11"],
            "--boost must be between -10 and 10",
        ),
    ] {
        let o = threaded(&db, &args);
        assert!(!o.status.success());
        assert!(said(&o).contains(want), "{want}: {}", said(&o));
    }
    for args in [
        vec!["tag"],
        vec!["tag", "add"],
        vec!["tag", "add", "x", "--colour", "red"],
    ] {
        let o = threaded(&db, &args);
        assert_eq!(o.status.code(), Some(2), "{args:?}");
        assert!(said(&o).contains("usage: threaded"));
    }
}

#[tokio::test]
async fn seed_fills_only_an_empty_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let o = threaded(&db, &["seed"]);
    assert!(o.status.success(), "{}", said(&o));
    let s = store(&db).await;
    let stories: i64 = sqlx::query_scalar("SELECT count(*) FROM stories")
        .fetch_one(s.pool())
        .await
        .unwrap();
    assert!(stories > 0);

    let o = threaded(&db, &["seed"]);
    assert!(!o.status.success());
    assert!(
        said(&o).contains("the database already has stories; seed only an empty one"),
        "{}",
        said(&o)
    );
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM stories")
        .fetch_one(s.pool())
        .await
        .unwrap();
    assert_eq!(after, stories);
}

#[test]
fn keygen_writes_keys_once() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    let keys = dir.path().join("keys");
    std::fs::create_dir(&keys).unwrap();
    let k = keys.to_str().unwrap();

    let o = threaded(&db, &["keygen", k]);
    assert!(o.status.success(), "{}", said(&o));
    let oauth = std::fs::read(keys.join("oauth-key.json")).unwrap();
    let cookie = std::fs::read(keys.join("cookie.key")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for f in ["oauth-key.json", "cookie.key"] {
            let mode = std::fs::metadata(keys.join(f))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "{f} is private to its owner");
        }
    }

    let o = threaded(&db, &["keygen", k]);
    assert!(!o.status.success(), "keygen never overwrites keys");
    assert_eq!(std::fs::read(keys.join("oauth-key.json")).unwrap(), oauth);
    assert_eq!(std::fs::read(keys.join("cookie.key")).unwrap(), cookie);

    let o = threaded(&db, &["keygen"]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn unknown_commands_print_the_usage() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("t.db");
    for args in [vec!["frobnicate"], vec!["triage"], vec!["triage", "nope"]] {
        let o = threaded(&db, &args);
        assert_eq!(o.status.code(), Some(2), "{args:?}");
        assert!(said(&o).contains("usage: threaded"), "{args:?}");
    }
}
