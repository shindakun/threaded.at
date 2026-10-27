-- The indexer logs what it does on its own, such as detaching a story whose
-- link changed after the edit window. SQLite cannot change a CHECK
-- constraint in place, so mod_log is rebuilt with `indexer` as an actor.

CREATE TABLE mod_log_new (
    id           INTEGER PRIMARY KEY,
    moderator_id INTEGER REFERENCES users(id),
    action       TEXT NOT NULL,
    user_id      INTEGER REFERENCES users(id),
    story_id     INTEGER REFERENCES stories(id),
    comment_id   INTEGER REFERENCES comments(id),
    tag_id       INTEGER REFERENCES tags(id),
    reason       TEXT NOT NULL DEFAULT '',
    created_at   INTEGER NOT NULL,
    actor        TEXT NOT NULL DEFAULT 'moderator'
        CHECK (actor IN ('moderator', 'triage', 'console', 'indexer')),
    detail       TEXT NOT NULL DEFAULT ''
);

INSERT INTO mod_log_new (id, moderator_id, action, user_id, story_id, comment_id,
                         tag_id, reason, created_at, actor, detail)
SELECT id, moderator_id, action, user_id, story_id, comment_id,
       tag_id, reason, created_at, actor, detail
FROM mod_log;

DROP TABLE mod_log;
ALTER TABLE mod_log_new RENAME TO mod_log;

CREATE INDEX mod_log_time ON mod_log(created_at DESC);
CREATE INDEX mod_log_story ON mod_log(story_id, created_at);
CREATE INDEX mod_log_comment ON mod_log(comment_id, created_at);
CREATE INDEX mod_log_user ON mod_log(user_id, created_at);
