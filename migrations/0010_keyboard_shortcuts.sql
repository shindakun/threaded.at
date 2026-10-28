-- Single-key shortcuts (j, k, o) are off until a reader turns them on in
-- /settings: WCAG 2.2 success criterion 2.1.4 asks that they can be turned
-- off, and off by default means a stray key never moves anyone's focus.

ALTER TABLE users ADD COLUMN keyboard_shortcuts INTEGER NOT NULL DEFAULT 0;
