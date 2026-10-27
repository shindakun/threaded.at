# Threaded

A link aggregator for [threaded.at](https://threaded.at): tagged stories,
threaded comments, flags with reasons, user levels, AI-assisted triage, and a
public moderation log. Sign-in is atproto OAuth. Submissions and comments are
records in the author's own repo.

- [docs/0001-design.md](docs/0001-design.md): the design.
- [docs/deck.html](docs/deck.html): the design as slides, with mockups.
- [docs/0002-deploy.md](docs/0002-deploy.md): deploying to the droplet.
- [docs/0003-m1.md](docs/0003-m1.md): the M1 plan, sign-in.
- [docs/0004-m2.md](docs/0004-m2.md): the M2 plan, writing.
- [docs/0005-m3.md](docs/0005-m3.md): the M3 plan, moderation.
- [docs/0006-m4.md](docs/0006-m4.md): the M4 plan, the indexer.
- [lexicons/at/threaded/](lexicons/at/threaded/): the submission, comment, and
  public vote schemas.
- [HUMANS.md](HUMANS.md): the standard for LLM-assisted contributions.
- [AGENTS.md](AGENTS.md): the project guide for coding agents.

## Status

Working today:

- **Sign-in** with any atproto account, through its own server (OAuth).
- **Writing:** submit, comment, reply, upvote, flag, edit, and delete.
  Submissions, comments, and public upvotes are records in the author's repo.
- **Reading:** ranked, newest, active, and top listings; tags, domains,
  search, profiles, and the blog.
- **Moderation:** a queue, item and user review, removals, locks, edits,
  merges, bans, role badges, notes, edit suggestions, a public moderation log,
  and admin pages for tags, roles, settings, and the site rules.
- **AI triage**, off unless an Anthropic API key is configured.
- **The network indexer:** Threaded records written from anywhere, not only
  through threaded.at, arrive through
  [hydrant](https://tangled.org/did:plc:dfl62fgb7wtjj3fcbb72naae/hydrant):
  new posts, edits, deletes, public votes, handle changes, and deactivated
  accounts. Off unless `THREADED_HYDRANT_SOCKET` is set.

Next: **M5, reader features**. The milestone table is in the design doc,
section 16.

## Quick start

Requires a stable Rust toolchain, from [rustup](https://github.com/rust-lang/rustup).

```sh
make seed    # demo tags and stories into threaded.db
make run     # serve on http://127.0.0.1:8080
make check   # fmt-check, clippy, test, audit, markdown lint, lexicon lint and validation
make a11y    # axe-core audit of every page, light and dark
make hydrant # a local hydrant indexing at.threaded.dev.* records
```

`make help` lists every target. Some need tools beyond Rust:

| Target | Needs |
|---|---|
| `make audit` | [cargo-audit](https://github.com/rustsec/rustsec/tree/main/cargo-audit) |
| `make md-lint` | [markdownlint-cli2](https://github.com/DavidAnson/markdownlint-cli2) |
| `make lex-lint`, `make lex-validate` | [goat](https://github.com/bluesky-social/goat) |
| `make a11y` | [Node](https://github.com/nodejs/node) and Chrome or [Chromium](https://chromium.googlesource.com/chromium/src) (set `CHROME` if it is not in its default place); it installs [puppeteer-core](https://github.com/puppeteer/puppeteer) and [axe-core](https://github.com/dequelabs/axe-core) itself |
| `make hooks` | [pre-commit](https://github.com/pre-commit/pre-commit) |
| `make hydrant` | a [hydrant](https://tangled.org/did:plc:dfl62fgb7wtjj3fcbb72naae/hydrant) build at `../hydrant/target/release/hydrant` (set `HYDRANT` for another path; building it is in `docs/0006-m4.md`). It keeps its database in `./hydrant.db` and serves its API on `./hydrant.sock` |

| Variable | Default |
|---|---|
| `THREADED_DB` | `sqlite:threaded.db` |
| `THREADED_ADDR` | `127.0.0.1:8080` |
| `THREADED_BLOG_DIR` | `blog` |
| `THREADED_PUBLIC_URL` | unset: the localhost development OAuth client |
| `THREADED_OAUTH_KEY` | required with `THREADED_PUBLIC_URL` |
| `THREADED_COOKIE_KEY` | required with `THREADED_PUBLIC_URL`; unset in development means sessions end on restart |
| `THREADED_ANTHROPIC_KEY` | a file holding an Anthropic API key; unset turns AI triage off |
| `THREADED_HYDRANT_SOCKET` | hydrant's API socket, such as `./hydrant.sock` from `make hydrant`; unset reads no records from the network |
| `RUST_LOG` | `info` |

## Signing in locally

```sh
make run
```

Open `http://127.0.0.1:8080`, not `localhost`: atproto's development client
returns to `127.0.0.1`. Sign in with any atproto account. `threaded keygen DIR`
writes the production key files.

A local build writes `at.threaded.dev.submission`, `.comment`, and `.vote`
records to your real repo; production writes `at.threaded.*`. Sign-in asks to
write submissions and comments. Publishing upvotes asks again, on
`/settings`, for vote records.

## Tags

```sh
threaded tag add rust --description "The Rust language and its ecosystem"
threaded tag add meta --description "About Threaded itself" --boost -0.5
threaded tag list
```

`tag add` on an existing tag updates it. `--boost` is the tag's rank boost,
from -10 to 10. Both commands use `THREADED_DB`.

## Moderation

```sh
threaded role grant did:plc:... admin    # or moderator; the account signs in once first
threaded role revoke did:plc:...
threaded triage check                     # one test item through the triage model
```

Roles take a DID, never a handle: a handle can pass to someone else. Admins
then work from `/admin`: tags, people, settings, and the site rules that AI
triage reads. AI triage runs only with `THREADED_ANTHROPIC_KEY` set and a
rules version saved; without either, moderation runs on flags and moderators.

## Blog

Drop a Markdown file with YAML front matter into `THREADED_BLOG_DIR` and it is
live within a second, no restart. Editing or deleting it works the same way.

```markdown
---
title: Sign-in is live
date: 2026-11-02
summary: Any atproto account can now sign in.
---
Body in CommonMark.
```

`title` and `date` (`YYYY-MM-DD`) are required. `slug` defaults to the file
name, and `draft: true` hides a post. The full rules are in the design doc,
section 12. A file that fails to parse is skipped and the reason is logged.

## Routes

| Route | Page |
|---|---|
| `/` | Front page |
| `/newest`, `/active` | Newest, most recent discussion |
| `/top?t=day\|week\|month\|year\|all` | Top by score |
| `/t/{tag}` | Ranked within a tag |
| `/domain/{domain}` | Newest from a domain and its subdomains |
| `/s/{short_id}` | A story and its comments |
| `/s/{short_id}/edit`, `/s/{short_id}/delete` | Edit or delete your story |
| `/c/{short_id}/reply`, `/c/{short_id}/edit`, `/c/{short_id}/delete` | Reply to a comment; edit or delete yours |
| `/submit` | Submit a story |
| `/comment`, `/vote`, `/flag` | Form targets for comments, upvotes, and flags |
| `/u/{handle}` | A user's stories, comments, and public upvotes; a DID works too |
| `/settings` | Public upvotes; keeping your posts out of AI triage (form target `/settings/triage`) |
| `/s/{short_id}/suggest` | Suggest a story's title and tags |
| `/mod` | Moderation queue: Holds, Flagged, Suggestions, Edits, Detached; form targets `/mod/verdicts/{id}` and `/mod/suggestions/{id}` |
| `/mod/s/{short_id}`, `/mod/c/{short_id}`, `/mod/u/{did}` | Review a story, comment, or user, and act on it; a handle in `/mod/u/` redirects to the DID |
| `/moderations` | The public moderation log |
| `/moderations/stats` | How often moderators agreed with AI triage |
| `/admin`, `/admin/rules`, `/admin/tags`, `/admin/people`, `/admin/settings` | Health, site rules, tags, roles, and settings; form target `/admin/breaker` |
| `/search?q=` | Search; syntax in the design doc, section 7 |
| `/tags` | Tag list |
| `/blog` | Blog index, newest first; `?page=2` and on |
| `/blog/{slug}` | A blog post |
| `/login` | Sign in with an atproto account |
| `/oauth-client-metadata.json`, `/oauth/jwks.json` | OAuth client metadata and public key |
| `/healthz` | Liveness |

## Layout

```text
src/main.rs        CLI: serve, seed, keygen, tag, role, triage check
crates/core/       ranking, levels, policy and settings, search parser, links, tags
crates/auth/       atproto OAuth: keys, SQLite session store, sign-in flow, record writes
crates/store/      SQLite via sqlx; migrations/ at the repo root
crates/triage/     AI triage: the model request, verdicts, the worker
crates/indexer/    reading records from hydrant: the client for its socket API
crates/web/        axum router, askama templates, CSS
migrations/        forward-only SQL migrations
lexicons/          at.threaded.* lexicon schemas
scripts/a11y/      axe-core page audit
blog/              blog posts, served from ./blog
deploy/            systemd units, nginx config, backup, update, and hydrant build scripts
docs/              design doc, deploy doc, milestone plans, and deck
third_party/       jacquard crates patched onto fixed crypto; see its README
```

Built on [axum](https://github.com/tokio-rs/axum),
[askama](https://github.com/askama-rs/askama),
[sqlx](https://github.com/launchbadge/sqlx) with SQLite,
[jacquard](https://tangled.org/nonbinary.computer/jacquard) for atproto and
OAuth, [pulldown-cmark](https://github.com/pulldown-cmark/pulldown-cmark) and
[ammonia](https://github.com/rust-ammonia/ammonia) for Markdown, and
[hydrant](https://tangled.org/did:plc:dfl62fgb7wtjj3fcbb72naae/hydrant) for
reading records from the network.

## License

MIT.
