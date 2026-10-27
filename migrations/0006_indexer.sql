-- Reading records from hydrant: the stream position, records that arrived
-- before what they refer to, account state from the network, and stories
-- whose link changed after the edit window.

-- Where Threaded is in hydrant's stream: the next event ID to read.
CREATE TABLE stream_cursor (
    id         INTEGER PRIMARY KEY CHECK (id = 1),
    next_id    INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

-- A comment whose story or parent is not indexed yet waits here, keyed by
-- the AT-URI it waits for, and is applied when that arrives.
CREATE TABLE pending_records (
    id         INTEGER PRIMARY KEY,
    uri        TEXT NOT NULL UNIQUE,
    waits_for  TEXT NOT NULL,
    event      TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX pending_records_waits ON pending_records(waits_for);
CREATE INDEX pending_records_age ON pending_records(created_at);

-- An account the network reports inactive (deactivated, taken down) is
-- hidden until it is active again.
ALTER TABLE users ADD COLUMN inactive_at INTEGER;
ALTER TABLE users ADD COLUMN inactive_status TEXT;

-- A story whose link changed after the edit window: hidden for moderators.
ALTER TABLE stories ADD COLUMN detached_at INTEGER;

-- Handles an account has held, from identity events and checks.
CREATE TABLE handle_history (
    id      INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users(id),
    handle  TEXT NOT NULL,
    seen_at INTEGER NOT NULL
);
CREATE INDEX handle_history_user ON handle_history(user_id, seen_at);
