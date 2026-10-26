use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use threaded_core::link;
use threaded_store::{NewStory, Store};

const USAGE: &str = "usage: threaded [serve | seed | keygen DIR | tag add NAME --description TEXT [--boost N] | tag list
                 | role grant DID moderator|admin | role revoke DID]

  serve       run the web server (default)
  seed        load demo tags and stories into the database
  keygen DIR  write a new OAuth signing key and cookie key into DIR
  tag add     create or update a tag
  tag list    list tags
  role grant  make a signed-in account a moderator or admin; DID, not handle
  role revoke return an account to a regular user

environment:
  THREADED_DB          database URL (default sqlite:threaded.db)
  THREADED_ADDR        listen address (default 127.0.0.1:8080)
  THREADED_BLOG_DIR    blog posts directory (default blog)
  THREADED_PUBLIC_URL  public origin, e.g. https://threaded.at; unset runs
                       the localhost development OAuth client
  THREADED_OAUTH_KEY   OAuth signing key file; required with THREADED_PUBLIC_URL
  THREADED_COOKIE_KEY  cookie key file; required with THREADED_PUBLIC_URL";

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let db = std::env::var("THREADED_DB").unwrap_or_else(|_| "sqlite:threaded.db".into());
    let cmd = std::env::args().nth(1);

    let result = match cmd.as_deref() {
        None | Some("serve") => serve(&db).await,
        Some("seed") => seed(&db).await,
        Some("tag") => match tag(&db, std::env::args().skip(2).collect()).await {
            Err(e) if e.to_string() == "usage" => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
            r => r,
        },
        Some("role") => match role(&db, std::env::args().skip(2).collect()).await {
            Err(e) if e.to_string() == "usage" => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
            r => r,
        },
        Some("keygen") => match std::env::args().nth(2) {
            Some(dir) => keygen(&dir),
            None => {
                eprintln!("{USAGE}");
                return ExitCode::from(2);
            }
        },
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e}");
            ExitCode::FAILURE
        }
    }
}

type BoxError = Box<dyn std::error::Error>;

fn keygen(dir: &str) -> Result<(), BoxError> {
    threaded_auth::keys::generate(std::path::Path::new(dir))?;
    println!(
        "wrote {dir}/{} and {dir}/{}",
        threaded_auth::keys::OAUTH_KEY_FILE,
        threaded_auth::keys::COOKIE_KEY_FILE
    );
    Ok(())
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn auth(store: &Store, addr: &str) -> Result<threaded_web::auth::Auth, BoxError> {
    use threaded_auth::client::{build, Mode};
    use threaded_auth::keys;

    let auth_store = threaded_auth::store::SqliteAuthStore::new(store.pool().clone());
    match env("THREADED_PUBLIC_URL") {
        Some(public_url) => {
            let key_path = env("THREADED_OAUTH_KEY").ok_or(
                "THREADED_PUBLIC_URL is set, so THREADED_OAUTH_KEY must name the OAuth key file",
            )?;
            let cookie_path = env("THREADED_COOKIE_KEY").ok_or(
                "THREADED_PUBLIC_URL is set, so THREADED_COOKIE_KEY must name the cookie key file",
            )?;
            let keyset = keys::load_keyset(std::path::Path::new(&key_path))?;
            let cookie_key = keys::load_cookie_key(std::path::Path::new(&cookie_path))?;
            let clients = build(
                Mode::Hosted {
                    public_url: public_url.clone(),
                    keyset,
                },
                auth_store.clone(),
            )?;
            tracing::info!("OAuth client {public_url}/oauth-client-metadata.json");
            Ok(threaded_web::auth::Auth::new(
                std::sync::Arc::new(clients),
                auth_store,
                &cookie_key,
                true,
            ))
        }
        None => {
            let port = addr.rsplit(':').next().unwrap_or("8080");
            let callback = format!("http://127.0.0.1:{port}/oauth/callback");
            let clients = build(Mode::Localhost { callback }, auth_store.clone())?;
            let cookie_key = match env("THREADED_COOKIE_KEY") {
                Some(p) => keys::load_cookie_key(std::path::Path::new(&p))?,
                None => {
                    tracing::warn!(
                        "THREADED_COOKIE_KEY unset: sessions end when the server restarts"
                    );
                    keys::random_cookie_key()
                }
            };
            tracing::info!("OAuth localhost development client; open http://127.0.0.1:{port}");
            Ok(threaded_web::auth::Auth::new(
                std::sync::Arc::new(clients),
                auth_store,
                &cookie_key,
                false,
            ))
        }
    }
}

async fn serve(db: &str) -> Result<(), BoxError> {
    let addr = std::env::var("THREADED_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let store = Store::open(db).await?;
    let blog_dir = std::env::var("THREADED_BLOG_DIR").unwrap_or_else(|_| "blog".into());
    let blog = threaded_web::blog::Blog::new(blog_dir, std::time::Duration::from_secs(1));
    let auth = auth(&store, &addr)?;

    let pruner = auth.store.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            tick.tick().await;
            if let Err(e) = pruner.prune().await {
                tracing::warn!(error = %e, "pruning expired sign-in requests");
            }
        }
    });

    let stored = store.settings().await?;
    let policy = threaded_core::policy::Policy::from_settings(
        stored.iter().map(|(k, v)| (k.as_str(), v.as_str())),
    );
    let state = threaded_web::AppState::new(store, blog, auth);
    state.set_policy(policy);
    let app = threaded_web::router(state);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("listening on http://{addr}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}

const DEMO_TAGS: &[(&str, &str, f64)] = &[
    ("ask", "Questions for the community", 0.0),
    (
        "databases",
        "Storage engines, query languages, data modelling",
        0.0,
    ),
    ("meta", "About this site", -0.5),
    ("networking", "Protocols and the wire", 0.0),
    ("rust", "The Rust language", 0.0),
    ("show", "Something you made", 0.0),
    ("web", "Browsers, HTML, CSS, HTTP", 0.0),
];

const DEMO_STORIES: &[(&str, Option<&str>, &[&str], i64)] = &[
    (
        "Writing a B-tree from scratch",
        Some("https://example.com/btree"),
        &["databases", "rust"],
        5_400,
    ),
    (
        "How the firehose sequences events",
        Some("https://example.org/firehose"),
        &["networking"],
        9_000,
    ),
    (
        "Ask: what do you use for full-text search?",
        None,
        &["ask", "databases"],
        14_000,
    ),
    (
        "Show: a terminal RSS reader",
        Some("https://example.net/rss-reader"),
        &["show", "rust"],
        30_000,
    ),
    (
        "Proposal: a weekly off-topic thread",
        None,
        &["meta"],
        40_000,
    ),
    (
        "CSS anchor positioning in practice",
        Some("https://blog.example.com/anchor"),
        &["web"],
        60_000,
    ),
];

async fn seed(db: &str) -> Result<(), BoxError> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
    let store = Store::open(db).await?;
    let any = store
        .list(threaded_store::Listing::New, &Default::default(), 1, 0)
        .await?;
    if !any.is_empty() {
        return Err("the database already has stories; seed only an empty one".into());
    }
    for (name, desc, m) in DEMO_TAGS {
        store.upsert_tag(name, desc, *m).await?;
    }
    let user = store
        .upsert_user("did:plc:demo", "demo.example", now)
        .await?;
    for (title, url, tags, age) in DEMO_STORIES {
        let l = url.map(link::parse).transpose()?;
        store
            .insert_story(&NewStory {
                user_id: user,
                title,
                link: l.as_ref(),
                body: if l.is_none() { "Demo text post." } else { "" },
                user_is_author: false,
                tags,
                created_at: now - age,
                record: None,
            })
            .await?;
    }
    tracing::info!(
        "seeded {} tags and {} stories",
        DEMO_TAGS.len(),
        DEMO_STORIES.len()
    );
    Ok(())
}

async fn tag(db: &str, args: Vec<String>) -> Result<(), BoxError> {
    let store = Store::open(db).await?;
    match args.first().map(String::as_str) {
        Some("list") => {
            for t in store.all_tags().await? {
                let state = if t.active { "" } else { " (inactive)" };
                println!(
                    "{:<14} {:>5} {}{state}",
                    t.name, t.rank_boost, t.description
                );
            }
            Ok(())
        }
        Some("add") => {
            let name = args.get(1).ok_or("usage")?;
            if !threaded_core::tag::valid_name(name) {
                return Err(format!(
                    "{name:?} is not a valid tag name: lowercase letters, digits, - _ +, up to 25"
                )
                .into());
            }
            let mut description = String::new();
            let mut boost = 0.0;
            let mut rest = args[2..].iter();
            while let Some(flag) = rest.next() {
                match flag.as_str() {
                    "--description" => description = rest.next().ok_or("usage")?.clone(),
                    "--boost" => {
                        boost = rest
                            .next()
                            .and_then(|v| v.parse().ok())
                            .ok_or("--boost needs a number, such as -0.5")?
                    }
                    _ => return Err("usage".into()),
                }
            }
            if !(-10.0..=10.0).contains(&boost) {
                return Err("--boost must be between -10 and 10".into());
            }
            store.upsert_tag(name, &description, boost).await?;
            println!("{name}: {description} (rank boost {boost})");
            Ok(())
        }
        _ => Err("usage".into()),
    }
}

/// `role grant DID ROLE` and `role revoke DID`. Roles are keyed on the DID,
/// which never changes hands; a handle can.
async fn role(db: &str, args: Vec<String>) -> Result<(), BoxError> {
    use threaded_store::{By, RoleChange, RoleName};

    let (did, role) = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["grant", did, role] => (
            did,
            match RoleName::parse(role) {
                Some(r @ (RoleName::Moderator | RoleName::Admin)) => r,
                _ => return Err("the role is moderator or admin".into()),
            },
        ),
        ["revoke", did] => (did, RoleName::User),
        _ => return Err("usage".into()),
    };
    if !did.starts_with("did:") {
        return Err(format!(
            "{did:?} is not a DID; roles take a DID such as did:plc:..., not a handle"
        )
        .into());
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
    let store = Store::open(db).await?;
    match store
        .set_role(did, role, By::Console, "From the console", now)
        .await?
    {
        RoleChange::NoSuchUser => Err(format!(
            "no account with DID {did} has signed in yet; sign in once, then grant the role"
        )
        .into()),
        RoleChange::Unchanged => {
            println!("{did} is already {}", role.as_str());
            Ok(())
        }
        RoleChange::Changed { from } => {
            println!("{did}: {from} to {}", role.as_str());
            Ok(())
        }
    }
}
