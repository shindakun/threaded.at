# Deploying threaded.at

Threaded runs on one DigitalOcean droplet: a single binary under systemd,
behind nginx, with SQLite on local disk. Every installed file is in
`deploy/`.

## What runs where

| Piece | Location | Owner |
|---|---|---|
| Source checkout | `/opt/threaded/src` | deploy user |
| Blog posts | `/opt/threaded/src/blog`, the repository's `blog/` | deploy user, via git |
| Binary | `/opt/threaded/bin/threaded` | root |
| Settings | `/etc/threaded/threaded.env` | root, mode 0640, group `threaded` |
| Database and backups | `/var/lib/threaded/` | `threaded` |
| Service | `threaded.service` | runs as `threaded` |
| Web server | nginx on 80 and 443, proxying to `127.0.0.1:8787` | root |
| Certificates | `/etc/letsencrypt/live/threaded.at/` | certbot |

The service's working directory is the checkout, so its default blog folder,
`./blog`, is the repository's `blog/`. Publishing a post is a commit and a
`git pull` (see "Blog posts").

## 1. Droplet

- Ubuntu 22.04 or later, with nginx from Ubuntu's packages. Other sites on the
  same nginx are fine: Threaded's config only answers for `threaded.at`.
- Turn on droplet backups.
- A sudo user, the deploy user, for every step.
- DNS: an `A` record (and `AAAA` if the droplet has IPv6) for `threaded.at`
  pointing at the droplet.
- Firewall: a DigitalOcean Cloud Firewall allowing inbound 22, 80, and 443
  only.

## 2. Packages and Rust

```sh
sudo apt update
sudo apt install -y nginx certbot python3-certbot-nginx sqlite3 git build-essential pkg-config curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
. "$HOME/.cargo/env"
```

The release build links with LTO. If the compiler is killed for lack of memory,
add swap:

```sh
sudo fallocate -l 2G /swapfile && sudo chmod 600 /swapfile
sudo mkswap /swapfile && sudo swapon /swapfile
echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
```

## 3. Service account and directories

```sh
sudo useradd --system --home-dir /var/lib/threaded --shell /usr/sbin/nologin threaded
sudo install -d -o threaded -g threaded -m 0750 /var/lib/threaded
sudo install -d -m 0755 /opt/threaded /opt/threaded/bin
sudo install -d -o "$USER" -g "$USER" -m 0755 /opt/threaded/src
git clone <repository URL> /opt/threaded/src
```

## 4. Build and install

```sh
cd /opt/threaded/src
cargo build --release --locked
sudo install -m 0755 target/release/threaded /opt/threaded/bin/threaded
sudo install -m 0755 deploy/backup.sh /opt/threaded/bin/threaded-backup
```

## 5. Settings

```sh
sudo install -d -m 0755 /etc/threaded
sudo install -o root -g threaded -m 0640 deploy/threaded.env /etc/threaded/threaded.env
```

Generate the OAuth signing key and the cookie key once. `keygen` refuses to
overwrite existing keys.

```sh
sudo /opt/threaded/bin/threaded keygen /etc/threaded
sudo chown root:threaded /etc/threaded/oauth-key.json /etc/threaded/cookie.key
sudo chmod 0640 /etc/threaded/oauth-key.json /etc/threaded/cookie.key
```

| Variable | Value |
|---|---|
| `THREADED_DB` | `sqlite:/var/lib/threaded/threaded.db` |
| `THREADED_ADDR` | `127.0.0.1:8787`; nginx's `proxy_pass` must match |
| `THREADED_PUBLIC_URL` | `https://threaded.at`; the OAuth client ID is `https://threaded.at/oauth-client-metadata.json` |
| `THREADED_OAUTH_KEY` | `/etc/threaded/oauth-key.json` |
| `THREADED_COOKIE_KEY` | `/etc/threaded/cookie.key` |
| `THREADED_ANTHROPIC_KEY` | Unset, or `/etc/threaded/anthropic.key`; section 11 |
| `RUST_LOG` | `info` |

Back up both key files. Losing the OAuth key ends every session; losing the
cookie key signs everyone out.

`THREADED_BLOG_DIR` is not set, so the blog folder is `./blog` in the
checkout.

## 6. Service

```sh
sudo install -m 0644 deploy/threaded.service /etc/systemd/system/threaded.service
sudo systemctl daemon-reload
sudo systemctl enable --now threaded
curl -fsS http://127.0.0.1:8787/healthz
```

The unit runs as `threaded`, can write only `/var/lib/threaded`, and restarts
on failure. Migrations run when the binary starts. Logs:
`journalctl -u threaded -f`.

## 7. TLS and nginx

Two files, installed in order. The port-80 file answers certificate
challenges and redirects everything else to HTTPS; the port-443 file needs the
certificate to exist, so it goes in after certbot runs. Neither touches other
sites on the server.

```sh
sudo install -d -m 0755 /var/www/letsencrypt
sudo install -m 0644 deploy/nginx/threaded.at-http.conf /etc/nginx/sites-available/
sudo ln -s /etc/nginx/sites-available/threaded.at-http.conf /etc/nginx/sites-enabled/
sudo nginx -t && sudo systemctl reload nginx

sudo certbot certonly --webroot -w /var/www/letsencrypt -d threaded.at \
  --deploy-hook "systemctl reload nginx"

sudo install -m 0644 deploy/nginx/threaded.at.conf /etc/nginx/sites-available/
sudo ln -s /etc/nginx/sites-available/threaded.at.conf /etc/nginx/sites-enabled/
sudo nginx -t && sudo systemctl reload nginx
curl -fsSI https://threaded.at/healthz
```

The site serves HTTP/2 with `listen 443 ssl http2`, the form nginx 1.18
understands. It sends HSTS and forwards the client address in
`X-Forwarded-For`. Certbot's timer renews the certificate through the same
webroot and reloads nginx; check it with `sudo certbot renew --dry-run`.

## 8. Backups

```sh
sudo install -m 0644 deploy/threaded-backup.service deploy/threaded-backup.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now threaded-backup.timer
sudo systemctl start threaded-backup.service
ls /var/lib/threaded/backups/
```

Each run takes a consistent copy with SQLite's `.backup` while the server
runs, checks it with `PRAGMA integrity_check`, gzips it, and deletes copies
older than 14 days. Droplet backups carry these off the machine. To restore:

```sh
sudo systemctl stop threaded
gunzip -c /var/lib/threaded/backups/threaded-<stamp>.db.gz | sudo -u threaded tee /var/lib/threaded/threaded.db >/dev/null
sudo rm -f /var/lib/threaded/threaded.db-wal /var/lib/threaded/threaded.db-shm
sudo systemctl start threaded
```

## 9. Blog posts

Posts are Markdown files with YAML front matter in the repository's `blog/`
folder (design doc, section 12). To publish:

```sh
# on your machine
git add blog/sign-in-is-live.md && git commit -m "Post: sign-in is live" && git push
# on the droplet
cd /opt/threaded/src && git pull --ff-only
```

The running server picks the post up within a second. No rebuild, no
restart. A post with a front matter error is skipped and logged:
`journalctl -u threaded | grep blog`.

## 10. Tags

Create the starter tags once, as the service account so the database stays
its own:

```sh
t() { sudo -u threaded env THREADED_DB=sqlite:/var/lib/threaded/threaded.db /opt/threaded/bin/threaded tag add "$@"; }
t atproto --description "The AT Protocol, Bluesky, and the apps built on them"
t rust --description "The Rust language and its ecosystem"
t programming --description "Languages, tools, and practice"
t web --description "Browsers, HTML, CSS, HTTP"
t security --description "Vulnerabilities, cryptography, and defense"
t show --description "Something you made"
t ask --description "A question for the community"
t meta --description "About Threaded itself" --boost -0.5
```

Running `tag add` again on a name updates it. `tag list` shows them all.

## 11. Moderation

**The first admin.** Sign in on threaded.at once, then grant the role by DID.
A handle can pass to someone else; a DID cannot. To find the DID for a handle:

```sh
curl -s "https://public.api.bsky.app/xrpc/com.atproto.identity.resolveHandle?handle=steve.is.angstridden.net"
sudo -u threaded env THREADED_DB=sqlite:/var/lib/threaded/threaded.db \
  /opt/threaded/bin/threaded role grant did:plc:... admin
```

Further moderators and admins are granted on `/admin/people`.

**Site rules.** Write them on `/admin/rules`. AI triage reads them as its
instructions and stays off until a version exists.

**AI triage, optional.** Without a key, nothing calls the model and moderation
runs on flags and moderators. To turn it on, put the key in a file only the
service can read, name it in `threaded.env`, and restart:

```sh
sudo install -o root -g threaded -m 0640 /dev/null /etc/threaded/anthropic.key
sudo nano /etc/threaded/anthropic.key    # paste the key on one line
echo 'THREADED_ANTHROPIC_KEY=/etc/threaded/anthropic.key' | sudo tee -a /etc/threaded/threaded.env
sudo systemctl restart threaded
sudo -u threaded sh -c 'set -a; . /etc/threaded/threaded.env; /opt/threaded/bin/threaded triage check'
```

`triage check` sends one test item and prints the verdict and token use; an
error names what failed. `/admin` shows whether triage is on, its queue, and
its error rate. Back up the key file with the others.

## 12. Updating

```sh
/opt/threaded/src/deploy/update.sh
```

It pulls, builds, takes a backup, installs the new binary, restarts the
service, and checks `/healthz`. Migrations are forward-only, so the backup it
takes is the way back from a bad release.

## 13. Later milestones

| Milestone | Adds to the droplet |
|---|---|
| M4 Indexer | hydrant as a second systemd service on a Unix socket in `/run/hydrant/`, with its database in `/var/lib/hydrant` |
| Lexicons | A `_lexicon.threaded.at` TXT record naming the publishing DID |
