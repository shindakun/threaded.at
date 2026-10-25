#!/bin/sh
# Daily SQLite backup: a consistent copy taken while the server runs,
# checked, compressed, and pruned after 14 days.
set -eu

db=/var/lib/threaded/threaded.db
dir=/var/lib/threaded/backups
out="$dir/threaded-$(date -u +%Y%m%dT%H%M%SZ).db"

mkdir -p "$dir"
sqlite3 "$db" ".backup '$out'"
[ "$(sqlite3 "$out" 'PRAGMA integrity_check;')" = ok ] || { echo "integrity check failed: $out" >&2; exit 1; }
gzip "$out"
find "$dir" -name 'threaded-*.db.gz' -mtime +14 -delete
