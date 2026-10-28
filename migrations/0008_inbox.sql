-- The replies inbox. A reply newer than inbox_read_at is unread; opening
-- /inbox sets it to now. Existing accounts start with everything read, so
-- the first count after this migration is not every reply ever received.
-- An account created later starts from its own created_at.

ALTER TABLE users ADD COLUMN inbox_read_at INTEGER;
UPDATE users SET inbox_read_at = CAST(strftime('%s', 'now') AS INTEGER);
