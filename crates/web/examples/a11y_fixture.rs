//! Adds a signed-in user with a story, a thread, a vote, and a flag to a
//! development database, then prints the session cookie (`name=value`) for
//! the accessibility audit of signed-in pages.
//!
//! usage: a11y_fixture DB_URL COOKIE_KEY_FILE

use threaded_auth::client::{scopes, Flow, Namespace};
use threaded_store::{NewComment, NewStory, Store, Target, VoteChange};

const DAY: i64 = 86_400;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (db, key_file) = match (args.next(), args.next()) {
        (Some(d), Some(k)) => (d, k),
        _ => return Err("usage: a11y_fixture DB_URL COOKIE_KEY_FILE".into()),
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as i64;
    let store = Store::open(&db).await?;

    let (did, handle, sid) = ("did:plc:a11y", "a11y.example", "a11y-session");
    let me = store.record_sign_in(did, handle, true, now).await?;
    let other = store
        .record_sign_in("did:plc:a11yother", "other.example", true, now)
        .await?;
    sqlx::query("UPDATE users SET created_at = ?, public_votes = 1 WHERE id IN (?, ?)")
        .bind(now - 200 * DAY)
        .bind(me)
        .bind(other)
        .execute(store.pool())
        .await?;
    sqlx::query("INSERT OR REPLACE INTO oauth_sessions (did, session_id, data, updated_at) VALUES (?, ?, ?, ?)")
        .bind(did)
        .bind(sid)
        .bind(threaded_auth::testing::session_json(did, sid, &scopes(Namespace::DEV, Flow::Votes)))
        .bind(now)
        .execute(store.pool())
        .await?;

    let link = threaded_core::link::parse("https://example.com/a11y-fixture")?;
    let story = store
        .insert_story(&NewStory {
            user_id: me,
            title: "A story with a thread",
            link: Some(&link),
            body: "Some *notes* on the link.",
            user_is_author: true,
            tags: &["rust"],
            created_at: now - 600,
            record: Some((
                "at://did:plc:a11y/at.threaded.dev.submission/3mfixture",
                "bafyfixture",
            )),
        })
        .await?;
    let story_id = store.story(&story).await?.ok_or("story missing")?.id;
    let top = store
        .insert_comment(&NewComment {
            story_id,
            user_id: other,
            parent_id: None,
            body: "A top-level comment with `code`.",
            created_at: now - 500,
            record: Some((
                "at://did:plc:a11yother/at.threaded.dev.comment/3mfixture",
                "bafyfixture",
            )),
        })
        .await?;
    let top_id = store.comment(&top).await?.ok_or("comment missing")?.id;
    store
        .insert_comment(&NewComment {
            story_id,
            user_id: me,
            parent_id: Some(top_id),
            body: "A reply.",
            created_at: now - 400,
            record: None,
        })
        .await?;
    store
        .set_vote(
            other,
            Target::Story(story_id),
            VoteChange::Flag {
                reason: "off-topic",
            },
            now,
        )
        .await?;
    store
        .set_vote(
            me,
            Target::Comment(top_id),
            VoteChange::Up { at_uri: None },
            now,
        )
        .await?;

    let key = cookie::Key::from(&std::fs::read(&key_file)?);
    let mut jar = cookie::CookieJar::new();
    jar.private_mut(&key).add(cookie::Cookie::new(
        "threaded",
        format!("{did} {sid} votes"),
    ));
    println!(
        "threaded={} {story} {top}",
        jar.get("threaded").ok_or("cookie")?.value()
    );
    Ok(())
}
