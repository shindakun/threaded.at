-- When each account's handle was last checked against its DID document, so
-- handles are re-checked in the background: daily, or hourly while an
-- account has no verified handle.
ALTER TABLE users ADD COLUMN handle_checked_at INTEGER;
