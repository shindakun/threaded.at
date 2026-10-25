#!/bin/sh
# Run on the droplet as the deploy user from /opt/threaded/src.
# Pulls, builds, backs up the database, installs the new binary, restarts.
set -eu

cd /opt/threaded/src
git pull --ff-only
cargo build --release --locked
sudo systemctl start threaded-backup.service
sudo install -m 0755 target/release/threaded /opt/threaded/bin/threaded.new
sudo mv /opt/threaded/bin/threaded.new /opt/threaded/bin/threaded
sudo install -m 0755 deploy/backup.sh /opt/threaded/bin/threaded-backup
sudo systemctl restart threaded
sleep 1
curl -fsS http://127.0.0.1:8787/healthz && echo " threaded is up"
