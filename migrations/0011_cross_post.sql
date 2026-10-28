-- Bluesky cross-posting. users.cross_post is the user's choice, set in
-- /settings; with it on, the submit form offers "Also post to Bluesky".
-- A story cross-posted from the web keeps the AT-URI of its post, or the
-- reason the post failed, which its author sees with a way to try again.

ALTER TABLE users ADD COLUMN cross_post INTEGER NOT NULL DEFAULT 0;
ALTER TABLE stories ADD COLUMN bsky_post_uri TEXT;
ALTER TABLE stories ADD COLUMN bsky_post_error TEXT;
