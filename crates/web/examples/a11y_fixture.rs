//! Adds a signed-in admin with a story, a thread, votes, flags, and
//! moderation history to a development database, then prints the session
//! cookie (`name=value`) for the accessibility audit of signed-in pages.
//!
//! usage: a11y_fixture DB_URL COOKIE_KEY_FILE

use threaded_auth::client::{scopes, Flow, Namespace};
use threaded_store::{By, NewComment, NewStory, NewVerdict, Store, StoryEdit, Target, VoteChange};

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
    sqlx::query("UPDATE users SET role = 'admin' WHERE id = ?")
        .bind(me)
        .execute(store.pool())
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

    // Enough flags to reach the queue, a removed reply, a badge, and log rows.
    for name in ["flagger1", "flagger2"] {
        let id = store
            .record_sign_in(
                &format!("did:plc:{name}"),
                &format!("{name}.example"),
                true,
                now,
            )
            .await?;
        store
            .set_vote(
                id,
                Target::Story(story_id),
                VoteChange::Flag { reason: "spam" },
                now,
            )
            .await?;
    }
    let removed = store
        .insert_comment(&NewComment {
            story_id,
            user_id: other,
            parent_id: Some(top_id),
            body: "A removed reply.",
            created_at: now - 300,
            record: None,
        })
        .await?;
    let removed_id = store.comment(&removed).await?.ok_or("comment missing")?.id;
    // One of the signed-in user's own posts, removed, so their profile lists
    // it under "Hidden from others".
    let mine_removed = store
        .insert_comment(&NewComment {
            story_id,
            user_id: me,
            parent_id: None,
            body: "My removed comment.",
            created_at: now - 250,
            record: None,
        })
        .await?;
    let mine_removed_id = store
        .comment(&mine_removed)
        .await?
        .ok_or("comment missing")?
        .id;
    store
        .remove(
            Target::Comment(mine_removed_id),
            By::Moderator(me),
            "Off-topic",
            now,
        )
        .await?
        .map_err(|e| e.to_string())?;
    store
        .remove(
            Target::Comment(removed_id),
            By::Moderator(me),
            "Unkind",
            now,
        )
        .await?
        .map_err(|e| e.to_string())?;
    store
        .grant_badge(
            other,
            "Maintainer",
            Some("https://example.com/project"),
            By::Moderator(me),
            "Runs the project",
            now,
        )
        .await?
        .map_err(|e| e.to_string())?;
    store
        .add_note(other, me, "A private note.", now)
        .await?
        .map_err(|e| e.to_string())?;

    // A story by someone else to suggest an edit to, and an open suggestion.
    let link2 = threaded_core::link::parse("https://example.com/a11y-other")?;
    let theirs = store
        .insert_story(&NewStory {
            user_id: other,
            title: "Their story",
            link: Some(&link2),
            body: "",
            user_is_author: false,
            tags: &["rust"],
            created_at: now - 200,
            record: None,
        })
        .await?;
    let flagger: i64 = sqlx::query_scalar("SELECT id FROM users WHERE did = 'did:plc:flagger1'")
        .fetch_one(store.pool())
        .await?;
    store
        .suggest(story_id, flagger, Some("A clearer title"), None, now)
        .await?;
    // Site rules, a story triage held, and a triage suggestion.
    store
        .save_rules(
            "No spam: no promotional links without discussion.",
            By::Moderator(me),
            "First rules",
            now,
        )
        .await?
        .map_err(|e| e.to_string())?;
    let link3 = threaded_core::link::parse("https://shop.example/a11y-held")?;
    let held = store
        .insert_story(&NewStory {
            user_id: other,
            title: "A held story",
            link: Some(&link3),
            body: "",
            user_is_author: false,
            tags: &["rust"],
            created_at: now - 100,
            record: None,
        })
        .await?;
    let held_id = store.story(&held).await?.ok_or("story missing")?.id;
    let theirs_id = store.story(&theirs).await?.ok_or("story missing")?.id;
    for (id, verdict, confidence, hold) in [
        (held_id, "spam", "high", true),
        (theirs_id, "off-topic", "medium", false),
    ] {
        sqlx::query("INSERT INTO triage_queue (story_id, created_at) VALUES (?, ?)")
            .bind(id)
            .bind(now)
            .execute(store.pool())
            .await?;
        let q = store.triage_next().await?.ok_or("queue empty")?;
        store
            .record_verdict(
                q.id,
                Target::Story(id),
                &NewVerdict {
                    verdict,
                    rule: "No spam",
                    confidence,
                    explanation: "A fixture verdict.",
                    model: "claude-opus-5-5",
                    rules_version_id: Some(1),
                    error: None,
                },
                hold,
                now,
            )
            .await?;
    }
    // A story whose author changed its link after the edit window.
    let link4 = threaded_core::link::parse("https://e.example/a11y-before")?;
    let moved = store
        .insert_story(&NewStory {
            user_id: other,
            title: "A detached story",
            link: Some(&link4),
            body: "",
            user_is_author: true,
            tags: &["rust"],
            created_at: now - 2 * DAY,
            record: None,
        })
        .await?;
    let moved_row = store.story(&moved).await?.ok_or("story missing")?;
    let link5 = threaded_core::link::parse("https://e.example/a11y-after")?;
    store
        .detach_story(
            &moved_row,
            &StoryEdit {
                title: "A detached story",
                link: Some(&link5),
                body: "",
                tags: &["rust"],
                cid: None,
            },
            now,
        )
        .await?;
    sqlx::query("UPDATE users SET karma = 50 WHERE id = ?")
        .bind(me)
        .execute(store.pool())
        .await?;
    // Something on /saved and /hidden, so the audit sees rows, not the
    // empty state: the other account's story saved, a seeded story hidden.
    let theirs_row = store.story(&theirs).await?.ok_or("story missing")?;
    store.set_saved(me, theirs_row.id, true, now).await?;
    let seeded: i64 = sqlx::query_scalar(
        "SELECT id FROM stories WHERE user_id NOT IN (?, ?) ORDER BY id LIMIT 1",
    )
    .bind(me)
    .bind(other)
    .fetch_one(store.pool())
    .await?;
    store.set_hidden(me, seeded, true, now).await?;

    let key = cookie::Key::from(&std::fs::read(&key_file)?);
    let mut jar = cookie::CookieJar::new();
    jar.private_mut(&key).add(cookie::Cookie::new(
        "threaded",
        format!("{did} {sid} votes"),
    ));
    println!(
        "threaded={} {story} {top} {theirs} {moved}",
        jar.get("threaded").ok_or("cookie")?.value()
    );
    Ok(())
}
