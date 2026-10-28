-- Read ribbons: when a reader last opened a story page. A listing row shows
-- "N new" for comments since then. Rows for stories older than the lock age
-- are dropped, since those stories take no new comments.

CREATE TABLE story_reads (
    user_id  INTEGER NOT NULL REFERENCES users(id),
    story_id INTEGER NOT NULL REFERENCES stories(id),
    read_at  INTEGER NOT NULL,
    PRIMARY KEY (user_id, story_id)
) WITHOUT ROWID;
CREATE INDEX story_reads_story ON story_reads(story_id);
