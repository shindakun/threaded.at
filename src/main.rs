use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use threaded_core::link;
use threaded_store::{NewStory, Store};

const USAGE: &str = "usage: threaded [serve | seed]

  serve   run the web server (default)
  seed    load demo tags and stories into the database

environment:
  THREADED_DB        database URL (default sqlite:threaded.db)
  THREADED_ADDR      listen address (default 127.0.0.1:8080)
  THREADED_BLOG_DIR  blog posts directory (default blog)";

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

async fn serve(db: &str) -> Result<(), BoxError> {
    let addr = std::env::var("THREADED_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());
    let store = Store::open(db).await?;
    let blog_dir = std::env::var("THREADED_BLOG_DIR").unwrap_or_else(|_| "blog".into());
    let blog = threaded_web::blog::Blog::new(blog_dir, std::time::Duration::from_secs(1));
    let app = threaded_web::router(threaded_web::AppState::new(store, blog));
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("listening on http://{addr}");
    axum::serve(listener, app)
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
