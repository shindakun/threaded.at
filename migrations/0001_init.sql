-- Times are unix seconds. Users are keyed by DID; handles change.

CREATE TABLE users (
    id            INTEGER PRIMARY KEY,
    did           TEXT NOT NULL UNIQUE,
    handle        TEXT NOT NULL COLLATE NOCASE,
    display_name  TEXT,
    about         TEXT NOT NULL DEFAULT '',
    role          TEXT NOT NULL DEFAULT 'user' CHECK (role IN ('user', 'moderator', 'admin')),
    karma         INTEGER NOT NULL DEFAULT 0,
    public_votes  INTEGER NOT NULL DEFAULT 0,
    vouched_by    INTEGER REFERENCES users(id),
    created_at    INTEGER NOT NULL,
    last_seen_at  INTEGER,
    banned_at     INTEGER,
    banned_by     INTEGER REFERENCES users(id),
    banned_reason TEXT
);
CREATE INDEX users_handle ON users(handle);

CREATE TABLE tags (
    id            INTEGER PRIMARY KEY,
    name          TEXT NOT NULL UNIQUE,
    description   TEXT NOT NULL DEFAULT '',
    category      TEXT NOT NULL DEFAULT '',
    rank_boost    REAL NOT NULL DEFAULT 0 CHECK (rank_boost BETWEEN -10 AND 10),
    privileged    INTEGER NOT NULL DEFAULT 0,
    new_users_ok  INTEGER NOT NULL DEFAULT 1,
    is_media      INTEGER NOT NULL DEFAULT 0,
    active        INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE stories (
    id             INTEGER PRIMARY KEY,
    short_id       TEXT NOT NULL UNIQUE,
    user_id        INTEGER NOT NULL REFERENCES users(id),
    title          TEXT NOT NULL,
    url            TEXT,
    canonical_url  TEXT,
    domain         TEXT,
    body           TEXT NOT NULL DEFAULT '',
    user_is_author INTEGER NOT NULL DEFAULT 0,
    score          INTEGER NOT NULL DEFAULT 1,
    flags          INTEGER NOT NULL DEFAULT 0,
    comment_count  INTEGER NOT NULL DEFAULT 0,
    rank           REAL NOT NULL DEFAULT 0,
    created_at     INTEGER NOT NULL,
    edited_at      INTEGER,
    last_comment_at INTEGER,
    locked         INTEGER NOT NULL DEFAULT 0,
    merged_into    INTEGER REFERENCES stories(id),
    deleted_at     INTEGER,
    deleted_by     INTEGER REFERENCES users(id),
    deleted_reason TEXT,
    at_uri         TEXT UNIQUE,
    at_cid         TEXT,
    CHECK (url IS NOT NULL OR body <> '')
);
CREATE INDEX stories_rank ON stories(rank DESC) WHERE deleted_at IS NULL AND merged_into IS NULL;
CREATE INDEX stories_new ON stories(created_at DESC) WHERE deleted_at IS NULL AND merged_into IS NULL;
CREATE INDEX stories_active ON stories(last_comment_at DESC) WHERE deleted_at IS NULL AND merged_into IS NULL;
CREATE INDEX stories_canonical ON stories(canonical_url);
CREATE INDEX stories_domain ON stories(domain);
CREATE INDEX stories_user ON stories(user_id, created_at DESC);

CREATE TABLE taggings (
    story_id INTEGER NOT NULL REFERENCES stories(id) ON DELETE CASCADE,
    tag_id   INTEGER NOT NULL REFERENCES tags(id),
    PRIMARY KEY (story_id, tag_id)
);
CREATE INDEX taggings_tag ON taggings(tag_id, story_id);

CREATE TABLE comments (
    id             INTEGER PRIMARY KEY,
    short_id       TEXT NOT NULL UNIQUE,
    story_id       INTEGER NOT NULL REFERENCES stories(id),
    user_id        INTEGER NOT NULL REFERENCES users(id),
    parent_id      INTEGER REFERENCES comments(id),
    depth          INTEGER NOT NULL DEFAULT 0,
    body           TEXT NOT NULL,
    score          INTEGER NOT NULL DEFAULT 1,
    flags          INTEGER NOT NULL DEFAULT 0,
    created_at     INTEGER NOT NULL,
    edited_at      INTEGER,
    deleted_at     INTEGER,
    deleted_by     INTEGER REFERENCES users(id),
    deleted_reason TEXT,
    at_uri         TEXT UNIQUE,
    at_cid         TEXT
);
CREATE INDEX comments_story ON comments(story_id, created_at);
CREATE INDEX comments_user ON comments(user_id, created_at DESC);
CREATE INDEX comments_parent ON comments(parent_id);

-- One row per user per target. value is +1 (upvote) or -1 (flag); a flag
-- carries a reason code. This table is the source of truth for scores. When
-- the voter has public_votes on, an upvote is also published as an
-- at.threaded.vote record and at_uri points at it. Flags are never published.
CREATE TABLE votes (
    id         INTEGER PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id),
    story_id   INTEGER REFERENCES stories(id),
    comment_id INTEGER REFERENCES comments(id),
    value      INTEGER NOT NULL CHECK (value IN (1, -1)),
    reason     TEXT CHECK ((value = 1 AND reason IS NULL) OR (value = -1 AND reason IS NOT NULL)),
    at_uri     TEXT UNIQUE CHECK (at_uri IS NULL OR value = 1),
    created_at INTEGER NOT NULL,
    CHECK ((story_id IS NULL) <> (comment_id IS NULL))
);
CREATE UNIQUE INDEX votes_story ON votes(user_id, story_id) WHERE story_id IS NOT NULL;
CREATE UNIQUE INDEX votes_comment ON votes(user_id, comment_id) WHERE comment_id IS NOT NULL;

CREATE TABLE saved_stories (
    user_id    INTEGER NOT NULL REFERENCES users(id),
    story_id   INTEGER NOT NULL REFERENCES stories(id),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, story_id)
);

CREATE TABLE hidden_stories (
    user_id    INTEGER NOT NULL REFERENCES users(id),
    story_id   INTEGER NOT NULL REFERENCES stories(id),
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, story_id)
);

CREATE TABLE tag_filters (
    user_id INTEGER NOT NULL REFERENCES users(id),
    tag_id  INTEGER NOT NULL REFERENCES tags(id),
    PRIMARY KEY (user_id, tag_id)
);

CREATE TABLE role_badges (
    id         INTEGER PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id),
    label      TEXT NOT NULL,
    link       TEXT,
    granted_by INTEGER NOT NULL REFERENCES users(id),
    created_at INTEGER NOT NULL,
    removed_at INTEGER
);

-- The public moderation log. Private notes live in mod_notes.
CREATE TABLE mod_log (
    id           INTEGER PRIMARY KEY,
    moderator_id INTEGER REFERENCES users(id),
    action       TEXT NOT NULL,
    user_id      INTEGER REFERENCES users(id),
    story_id     INTEGER REFERENCES stories(id),
    comment_id   INTEGER REFERENCES comments(id),
    tag_id       INTEGER REFERENCES tags(id),
    reason       TEXT NOT NULL DEFAULT '',
    created_at   INTEGER NOT NULL
);
CREATE INDEX mod_log_time ON mod_log(created_at DESC);

CREATE TABLE mod_notes (
    id           INTEGER PRIMARY KEY,
    user_id      INTEGER NOT NULL REFERENCES users(id),
    moderator_id INTEGER NOT NULL REFERENCES users(id),
    note         TEXT NOT NULL,
    created_at   INTEGER NOT NULL
);
CREATE INDEX mod_notes_user ON mod_notes(user_id, created_at DESC);

-- External-content FTS5 tables, kept in step by triggers.
CREATE VIRTUAL TABLE stories_fts USING fts5(
    title, body, url, domain,
    content = 'stories', content_rowid = 'id',
    tokenize = 'porter unicode61'
);

CREATE TRIGGER stories_fts_insert AFTER INSERT ON stories BEGIN
    INSERT INTO stories_fts(rowid, title, body, url, domain)
    VALUES (new.id, new.title, new.body, coalesce(new.url, ''), coalesce(new.domain, ''));
END;

CREATE TRIGGER stories_fts_delete AFTER DELETE ON stories BEGIN
    INSERT INTO stories_fts(stories_fts, rowid, title, body, url, domain)
    VALUES ('delete', old.id, old.title, old.body, coalesce(old.url, ''), coalesce(old.domain, ''));
END;

CREATE TRIGGER stories_fts_update AFTER UPDATE OF title, body, url, domain ON stories BEGIN
    INSERT INTO stories_fts(stories_fts, rowid, title, body, url, domain)
    VALUES ('delete', old.id, old.title, old.body, coalesce(old.url, ''), coalesce(old.domain, ''));
    INSERT INTO stories_fts(rowid, title, body, url, domain)
    VALUES (new.id, new.title, new.body, coalesce(new.url, ''), coalesce(new.domain, ''));
END;

CREATE VIRTUAL TABLE comments_fts USING fts5(
    body,
    content = 'comments', content_rowid = 'id',
    tokenize = 'porter unicode61'
);

CREATE TRIGGER comments_fts_insert AFTER INSERT ON comments BEGIN
    INSERT INTO comments_fts(rowid, body) VALUES (new.id, new.body);
END;

CREATE TRIGGER comments_fts_delete AFTER DELETE ON comments BEGIN
    INSERT INTO comments_fts(comments_fts, rowid, body) VALUES ('delete', old.id, old.body);
END;

CREATE TRIGGER comments_fts_update AFTER UPDATE OF body ON comments BEGIN
    INSERT INTO comments_fts(comments_fts, rowid, body) VALUES ('delete', old.id, old.body);
    INSERT INTO comments_fts(rowid, body) VALUES (new.id, new.body);
END;
