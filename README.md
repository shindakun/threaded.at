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
- [lexicons/at/threaded/](lexicons/at/threaded/): the submission, comment, and
  public vote schemas.
- [HUMANS.md](HUMANS.md): the standard for LLM-assisted contributions.
- [AGENTS.md](AGENTS.md): the project guide for coding agents.

## Status

M0 is built: the schema, ranking, levels, search, link canonicalization,
read-only listings, the blog, the lexicon drafts, and an accessibility audit
that every page passes. M1, sign-in, M2, writing, and M3, moderation, are
built. The indexer is M4. The milestone table is in the design doc, section 16.

## Quick start

Requires a stable Rust toolchain.

```sh
make seed    # demo tags and stories into threaded.db
make run     # serve on http://127.0.0.1:8080
make check   # fmt-check, clippy, test, audit, markdown lint, lexicon lint and validation
make a11y    # axe-core audit of every page, light and dark
```

`make help` lists every target. `make audit` needs `cargo-audit`, `make md-lint`
needs `markdownlint-cli2`, `make lex-lint` and `make lex-validate` need
[goat](https://github.com/bluesky-social/goat), `make a11y` needs Node and
Chrome (set `CHROME` if Chrome is not in its default place), and `make hooks`
needs `pre-commit`.

| Variable | Default |
|---|---|
| `THREADED_DB` | `sqlite:threaded.db` |
| `THREADED_ADDR` | `127.0.0.1:8080` |
| `THREADED_BLOG_DIR` | `blog` |
| `THREADED_PUBLIC_URL` | unset: the localhost development OAuth client |
| `THREADED_OAUTH_KEY` | required with `THREADED_PUBLIC_URL` |
| `THREADED_COOKIE_KEY` | required with `THREADED_PUBLIC_URL`; unset in development means sessions end on restart |
| `THREADED_ANTHROPIC_KEY` | a file holding an Anthropic API key; unset turns AI triage off |
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
| `/u/{handle}` | A user's stories, comments, and public upvotes |
| `/settings` | Public upvotes; keeping your posts out of AI triage |
| `/s/{short_id}/suggest` | Suggest a story's title and tags |
| `/mod` | Moderation queue: Holds, Flagged, Suggestions, Edits |
| `/mod/s/{short_id}`, `/mod/c/{short_id}`, `/mod/u/{handle}` | Review a story, comment, or user, and act on it |
| `/moderations` | The public moderation log |
| `/moderations/stats` | How often moderators agreed with AI triage |
| `/admin`, `/admin/rules`, `/admin/tags`, `/admin/people`, `/admin/settings` | Health, site rules, tags, roles, and settings |
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
crates/core/       ranking, levels, search parser, links, tags
crates/auth/       atproto OAuth: keys, SQLite session store, sign-in flow, record writes
crates/store/      SQLite via sqlx; migrations/ at the repo root
crates/triage/     AI triage: the model request, verdicts, the worker
crates/web/        axum router, askama templates, CSS
migrations/        forward-only SQL migrations
lexicons/          at.threaded.* lexicon schemas
scripts/a11y/      axe-core page audit
blog/              blog posts, served from ./blog
deploy/            systemd units, nginx config, backup and update scripts
docs/              design doc, deploy doc, and deck
third_party/       jacquard crates patched onto fixed crypto; see its README
```

## License

MIT.
