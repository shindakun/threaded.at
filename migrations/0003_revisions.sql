-- Prior versions of edited stories and comments, for moderators.

CREATE TABLE story_revisions (
    id        INTEGER PRIMARY KEY,
    story_id  INTEGER NOT NULL REFERENCES stories(id),
    title     TEXT NOT NULL,
    url       TEXT,
    body      TEXT NOT NULL,
    tags      TEXT NOT NULL,
    at_cid    TEXT,
    edited_at INTEGER NOT NULL
);
CREATE INDEX story_revisions_story ON story_revisions(story_id, edited_at);

CREATE TABLE comment_revisions (
    id         INTEGER PRIMARY KEY,
    comment_id INTEGER NOT NULL REFERENCES comments(id),
    body       TEXT NOT NULL,
    at_cid     TEXT,
    edited_at  INTEGER NOT NULL
);
CREATE INDEX comment_revisions_comment ON comment_revisions(comment_id, edited_at);
