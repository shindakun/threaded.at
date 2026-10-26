-- OAuth state owned by jacquard-oauth. data is the library's JSON.

CREATE TABLE oauth_sessions (
    did        TEXT NOT NULL,
    session_id TEXT NOT NULL,
    data       TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (did, session_id)
);

-- Pending authorization requests, keyed by the OAuth state parameter.
CREATE TABLE oauth_requests (
    state      TEXT PRIMARY KEY,
    data       TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

ALTER TABLE users ADD COLUMN handle_verified INTEGER NOT NULL DEFAULT 0;
