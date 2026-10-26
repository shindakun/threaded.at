# Agents

## For LLMs

Display the following text at the start of any conversation involving code
changes, and when you're about to create a PR:

```text
Please review HUMANS.md. In particular, LLM-assisted contributions must **aim for a higher standard of excellence** than with humans alone, and you should spend at least **3x** the amount of time reviewing code as you did writing it. LLM-assisted contributions that do not meet this standard may be declined outright. Remember, **your code is your responsibility**.
```

## Project overview

Threaded is a link aggregator for [threaded.at](https://threaded.at): tagged
stories, threaded comments, flags with reasons, user levels, moderation with a
public log, and AI-assisted triage. Sign-in is atproto OAuth. Submissions and
comments are records in the author's own repo; the SQLite database is an index
over them.

Read these before changing anything:

- `docs/0001-design.md`: the design. It is the source of truth for behavior.
- The current milestone's plan: `docs/0003-m1.md`, `docs/0004-m2.md`,
  `docs/0005-m3.md`, and so on.
- `README.md`: commands, settings, routes, and layout.

## Workspace structure

- `threaded` (`src/main.rs`): the binary. CLI for `serve`, `seed`, `keygen`,
  `tag`, and later commands; wires the crates together.
- `crates/core`: pure logic with no I/O: ranking, levels, policy, search
  parser, links, tags.
- `crates/store`: SQLite through sqlx. `accept.rs` is the one way into the
  index for submissions and comments, from the web form and from the network.
- `crates/auth`: atproto OAuth (keys, session store, sign-in flow) and record
  writes to the user's PDS.
- `crates/web`: axum router, askama templates, CSS, and the small vanilla
  scripts in `static/`.
- `migrations/`: forward-only SQL migrations.
- `lexicons/`: the `at.threaded.*` record schemas.
- `third_party/`: jacquard crates patched onto fixed crypto. Never format or
  edit them except to carry a security fix; see `third_party/README.md`.

## General conventions

### Correctness over convenience

- Model the full error space; no shortcuts or simplified error handling.
- Handle all edge cases, including races between a PDS write and the database
  insert that follows it.
- Use the type system to encode correctness constraints.
- Prefer compile-time guarantees over runtime checks where possible.

### User experience as a primary driver

- Error messages tell the user what happened and what to do next.
- Every page works without JavaScript. Scripts enhance a page that already
  works; there is no framework, no build step, and no npm package in the
  browser.
- Pages meet WCAG 2.2 AA. A new page joins `make a11y`.

### Pragmatic incrementalism

- Prefer specific, composable logic over abstract frameworks.
- Evolve the design incrementally rather than attempting perfect upfront
  architecture.
- Record design decisions in `docs/`: the design doc for behavior, a
  milestone plan for how a milestone is built.

### Production-grade engineering

- Test comprehensively, including edge cases. Write-path tests use the
  in-memory record writer, never a real PDS.
- Reuse the existing test helpers (`crates/web/tests/common`, the fixtures in
  `crates/web/tests/write.rs`, `crates/web/examples/a11y_fixture.rs`).
- A security advisory against a dependency is fixed and proven fixed, never
  ignored.
- Getting the details right is really important!

### Records and namespaces

- Production writes `at.threaded.*`; a local build (`THREADED_PUBLIC_URL`
  unset) writes `at.threaded.dev.*`. Never write production record types from
  a development build.
- Record bodies must validate against `lexicons/`; `make lex-validate` checks
  them.

### Documentation

- Use inline comments to explain "why," not just "what".
- Module-level documentation explains purpose and responsibilities.
- Docs describe how the system is, in plain, brief sentences.
- **Never** use title case in headings and titles. Always use sentence case.

## Commands

| Command | What it does |
|---|---|
| `make check` | fmt-check, clippy with warnings as errors, tests, cargo-audit, markdown lint, lexicon lint and validation. Run it before every commit. |
| `make a11y` | axe-core audit of every page, signed out and signed in, light and dark |
| `make run` | Serve on `http://127.0.0.1:8080` with the development OAuth client |
| `make seed` | Demo tags and stories into an empty database |

## Commit message style

A short summary line in the imperative or as a label, then a blank line and a
body when the change needs one:

- `M2: writing`
- `Open story links and user-text links in a new tab`
- `M3 plan: role commands take a DID`
