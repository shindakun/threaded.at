-- Bluesky blocks. Each account's app.bsky.graph.block records, read from its
-- PDS, one row per record. A block applies both ways: neither account sees
-- the other's posts while signed in, and neither can reply to the other.
-- blocks_tried_at paces the refresh; blocks_read_at is the last read that
-- finished, shown in settings.

CREATE TABLE blocks (
    blocker_did TEXT NOT NULL,
    rkey TEXT NOT NULL,
    subject_did TEXT NOT NULL,
    PRIMARY KEY (blocker_did, rkey)
) WITHOUT ROWID;
CREATE INDEX blocks_pair ON blocks (blocker_did, subject_did);
CREATE INDEX blocks_subject ON blocks (subject_did, blocker_did);

ALTER TABLE users ADD COLUMN blocks_tried_at INTEGER;
ALTER TABLE users ADD COLUMN blocks_read_at INTEGER;
