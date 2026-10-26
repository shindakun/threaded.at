#!/bin/sh
set -eu

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
port=${A11Y_PORT:-18181}
base="http://127.0.0.1:$port"

export THREADED_DB="sqlite:$tmp/a11y.db"
export THREADED_ADDR="127.0.0.1:$port"
export THREADED_BLOG_DIR="$tmp/blog"
export THREADED_COOKIE_KEY="$tmp/keys/cookie.key"

mkdir "$tmp/blog" "$tmp/keys"
cat >"$tmp/blog/sample.md" <<'MD'
---
title: Sample post
date: 2026-10-25
summary: Every Markdown element the blog renders.
author: Threaded
---
# Heading

Text with a [link](https://example.com), `code`, *emphasis*, and **strong**.

- one
- two

1. first
2. second

> A quote.

```rust
fn main() {}
```

| Column | Value |
|---|---|
| a | 1 |
MD

cargo build --quiet
cargo build --quiet -p threaded-web --example a11y_fixture
bin=target/debug/threaded
"$bin" keygen "$tmp/keys" >/dev/null
"$bin" seed >/dev/null 2>&1
# Signed-in session cookie, then the fixture story and comment IDs.
set -- $(target/debug/examples/a11y_fixture "$THREADED_DB" "$THREADED_COOKIE_KEY")
cookie=$1 sid=$2 mine=/s/$2 comment=$3
"$bin" serve >"$tmp/serve.log" 2>&1 &
pid=$!
trap 'rc=$?; kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null || true; rm -rf "$tmp"; exit $rc' EXIT

i=0
until curl -fs "$base/healthz" >/dev/null; do
  i=$((i + 1))
  [ "$i" -gt 50 ] && { cat "$tmp/serve.log"; exit 1; }
  sleep 0.2
done

story=$(curl -fs "$base/newest" | grep -o 'href="/s/[a-z0-9]*"' | head -1 | cut -d'"' -f2)

(cd "$here" && npm ci --silent --no-audit --no-fund)
node "$here/audit.mjs" "$base" / /newest /active /top /t/rust /domain/example.com /tags /search "/search?q=rust" "$story" /s/nope00 /blog /blog/sample /login "/oauth/callback?error=access_denied"
# A dead session would redirect every page to /login and pass vacuously.
curl -fs -H "Cookie: $cookie" "$base/settings" | grep -q 'href="/u/a11y.example"' || { echo "fixture session is not signed in"; exit 1; }
# The same for staff pages: a 403 page would pass the audit too.
curl -fs -H "Cookie: $cookie" "$base/admin" >/dev/null || { echo "fixture user cannot open /admin"; exit 1; }
curl -fs -H "Cookie: $cookie" "$base/mod" | grep -q 'class="queue-row"' || { echo "the moderation queue is empty or closed"; exit 1; }
A11Y_COOKIE="$cookie" node "$here/audit.mjs" "$base" / "$mine" "$mine/edit" /submit "/c/$comment/reply" "/c/$comment/edit" /settings /u/a11y.example /u/other.example \
  /mod "/mod/s/$sid" /mod/c/"$comment" /mod/u/other.example /moderations /admin
