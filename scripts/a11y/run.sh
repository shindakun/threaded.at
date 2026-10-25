#!/bin/sh
set -eu

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
port=${A11Y_PORT:-18181}
base="http://127.0.0.1:$port"

export THREADED_DB="sqlite:$tmp/a11y.db"
export THREADED_ADDR="127.0.0.1:$port"
export THREADED_BLOG_DIR="$tmp/blog"

mkdir "$tmp/blog"
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
bin=target/debug/threaded
"$bin" seed >/dev/null 2>&1
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
node "$here/audit.mjs" "$base" / /newest /active /top /t/rust /domain/example.com /tags /search "/search?q=rust" "$story" /s/nope00 /blog /blog/sample
