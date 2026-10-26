-- Moderation: holds, removals, locks, moderator edits, merges, triage,
-- rules, settings, and edit suggestions.

-- A hold is triage hiding an item until a moderator decides. A removal is a
-- moderator hiding it; unlike an author's delete, it can be restored.
ALTER TABLE stories ADD COLUMN held_at INTEGER;
ALTER TABLE stories ADD COLUMN removed_at INTEGER;
ALTER TABLE stories ADD COLUMN removed_by INTEGER REFERENCES users(id);
ALTER TABLE stories ADD COLUMN removed_reason TEXT;
ALTER TABLE stories ADD COLUMN locked_reason TEXT;

-- While a moderator's edit is in force, the main columns hold what readers
-- see and these hold the author's own values, which the author's later edits
-- change. Clearing the override copies them back.
ALTER TABLE stories ADD COLUMN author_title TEXT;
ALTER TABLE stories ADD COLUMN author_url TEXT;
ALTER TABLE stories ADD COLUMN author_tags TEXT;
ALTER TABLE stories ADD COLUMN override_by INTEGER REFERENCES users(id);
ALTER TABLE stories ADD COLUMN override_at INTEGER;

ALTER TABLE comments ADD COLUMN held_at INTEGER;
ALTER TABLE comments ADD COLUMN removed_at INTEGER;
ALTER TABLE comments ADD COLUMN removed_by INTEGER REFERENCES users(id);
ALTER TABLE comments ADD COLUMN removed_reason TEXT;

-- The story a comment or vote belonged to before a merge, so it can be undone.
ALTER TABLE comments ADD COLUMN merged_from INTEGER REFERENCES stories(id);
ALTER TABLE votes ADD COLUMN merged_from INTEGER REFERENCES stories(id);

ALTER TABLE users ADD COLUMN triage_opt_out INTEGER NOT NULL DEFAULT 0;

-- Who acted: a moderator, triage, or the console (`threaded role`). `detail`
-- holds what changed, such as a setting's old and new value.
ALTER TABLE mod_log ADD COLUMN actor TEXT NOT NULL DEFAULT 'moderator'
    CHECK (actor IN ('moderator', 'triage', 'console'));
ALTER TABLE mod_log ADD COLUMN detail TEXT NOT NULL DEFAULT '';
CREATE INDEX mod_log_story ON mod_log(story_id, created_at);
CREATE INDEX mod_log_comment ON mod_log(comment_id, created_at);
CREATE INDEX mod_log_user ON mod_log(user_id, created_at);

-- Items waiting for the triage worker, oldest first.
CREATE TABLE triage_queue (
    id         INTEGER PRIMARY KEY,
    story_id   INTEGER REFERENCES stories(id),
    comment_id INTEGER REFERENCES comments(id),
    created_at INTEGER NOT NULL,
    CHECK ((story_id IS NULL) <> (comment_id IS NULL))
);

CREATE TABLE rules_versions (
    id         INTEGER PRIMARY KEY,
    body       TEXT NOT NULL,
    author_id  INTEGER REFERENCES users(id),
    created_at INTEGER NOT NULL
);

-- One verdict per item. `held` is set when the verdict hid the item. A
-- moderator's decision closes it: 'confirmed' or 'released' for a hold,
-- 'removed' or 'dismissed' for a suggestion.
CREATE TABLE triage_verdicts (
    id               INTEGER PRIMARY KEY,
    story_id         INTEGER REFERENCES stories(id),
    comment_id       INTEGER REFERENCES comments(id),
    verdict          TEXT NOT NULL,
    rule             TEXT NOT NULL DEFAULT '',
    confidence       TEXT NOT NULL DEFAULT '',
    explanation      TEXT NOT NULL DEFAULT '',
    model            TEXT NOT NULL DEFAULT '',
    rules_version_id INTEGER REFERENCES rules_versions(id),
    error            TEXT,
    held             INTEGER NOT NULL DEFAULT 0,
    decision         TEXT CHECK (decision IN ('confirmed', 'released', 'removed', 'dismissed')),
    decided_by       INTEGER REFERENCES users(id),
    decided_at       INTEGER,
    created_at       INTEGER NOT NULL,
    CHECK ((story_id IS NULL) <> (comment_id IS NULL))
);
CREATE UNIQUE INDEX triage_verdicts_story ON triage_verdicts(story_id) WHERE story_id IS NOT NULL;
CREATE UNIQUE INDEX triage_verdicts_comment ON triage_verdicts(comment_id) WHERE comment_id IS NOT NULL;
CREATE INDEX triage_verdicts_open ON triage_verdicts(created_at) WHERE decision IS NULL;

-- Values on /admin/settings. A missing key means the built-in default.
CREATE TABLE settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_by INTEGER REFERENCES users(id),
    updated_at INTEGER NOT NULL
);

-- Title and tag edits proposed by users, decided by moderators.
CREATE TABLE suggestions (
    id         INTEGER PRIMARY KEY,
    story_id   INTEGER NOT NULL REFERENCES stories(id),
    user_id    INTEGER NOT NULL REFERENCES users(id),
    title      TEXT,
    tags       TEXT,
    created_at INTEGER NOT NULL,
    decision   TEXT CHECK (decision IN ('accepted', 'dismissed')),
    decided_by INTEGER REFERENCES users(id),
    decided_at INTEGER
);
CREATE INDEX suggestions_open ON suggestions(created_at) WHERE decision IS NULL;
CREATE UNIQUE INDEX suggestions_one_open ON suggestions(story_id, user_id) WHERE decision IS NULL;
