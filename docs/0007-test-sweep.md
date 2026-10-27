# Test sweep

Before M5, a pass over what the test suite does not exercise. Coverage was
measured with [cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov)
(`cargo llvm-cov --workspace`) after M4: 82.9% of lines, 80.2% of functions.
Line coverage finds code that never runs; this list is the behavior behind
those lines, grouped by what it would take to test.

Every item is either done in this sweep or has a named place where it will be
done. Nothing is dropped.

## Done when

- Every item under "In this sweep" has a test that fails without the
  behavior it covers.
- `make check` and `make a11y` pass.
- Coverage is measured again and the new numbers are recorded below.

## In this sweep

### Website actions

| Behavior | Test file |
|---|---|
| Editing a comment: the form, saving, the record rewritten with its subject and parent, "edited" shown | `crates/web/tests/write.rs` |
| Only the author can edit a comment; a deleted comment cannot be edited | `crates/web/tests/write.rs` |
| Upvoting, withdrawing, and flagging a comment | `crates/web/tests/write.rs` |
| Flagging an item you upvoted in public deletes the vote record | `crates/web/tests/write.rs` |
| Story edit errors: empty or long title, unusable link, no link and no text, bad tags; the form keeps what was typed | `crates/web/tests/write.rs` |
| The reply page and both edit pages as routes: who may open them, deleted items | `crates/web/tests/write.rs` |

### Moderation

| Behavior | Test file |
|---|---|
| Comment review at `/mod/c/{id}`: removed and held states, remove and restore | `crates/web/tests/staff.rs` |
| Unban; removing a badge, and refusing a badge the account does not hold | `crates/web/tests/staff.rs` |
| Dismissing a suggested edit, logged | `crates/web/tests/staff.rs` |
| Merge into an unknown story; moderator edit with an unusable link, an unknown tag, or no tags | `crates/web/tests/staff.rs` |
| Resetting the triage circuit breaker from `/admin` | `crates/web/tests/admin.rs` |
| Granting a role the account already holds is refused | `crates/web/tests/admin.rs` |
| Triage of comments: queueing, a held comment, deciding its verdict | `crates/web/tests/triage.rs` |

### Indexer

| Behavior | Test file |
|---|---|
| A vote on a comment | `crates/indexer/tests/apply.rs` |
| A vote from an account that never signed in, or may not upvote, is ignored | `crates/indexer/tests/apply.rs` |
| A vote whose subject is not indexed yet waits for it | `crates/indexer/tests/apply.rs` |
| A vote on a locked story, or from an account that flagged it, is ignored | `crates/indexer/tests/apply.rs` |
| Comment updates: not indexed, deleted, unchanged CID, refused | `crates/indexer/tests/apply.rs` |
| A late link change on a story a moderator edited detaches it and logs the author's own old link | `crates/indexer/tests/apply.rs` |
| A network edit with an unusable link is refused | `crates/indexer/tests/apply.rs` |

### Limits on comments and tags

| Behavior | Test file |
|---|---|
| Comment refusals: the hourly limit, empty and long text, a reply to a deleted comment or one on another story, a known record | `crates/store/tests/accept.rs` |
| Privileged tags and tags closed to new accounts | `crates/store/tests/accept.rs` |

### Command line

Run against the built `threaded` binary, as an operator would.

| Behavior | Test file |
|---|---|
| `role grant` and `role revoke` by DID; a handle is refused | `tests/cli.rs` |
| `tag add` creates and updates; `tag list` | `tests/cli.rs` |
| `seed` fills an empty database and refuses a full one | `tests/cli.rs` |
| `keygen` writes keys and refuses to overwrite them | `tests/cli.rs` |
| An unknown command prints the usage | `tests/cli.rs` |

### Migrations

| Behavior | Test file |
|---|---|
| Migration 0007 keeps every `mod_log` row when it rebuilds the table | `crates/store/tests/store.rs` |

## Not in this sweep

| Item | Why not now | Where |
|---|---|---|
| The PLC directory lookup on user review | Its address is fixed; a test needs it configurable to point at a fake server | M5, with the other identity work |
| The handle typeahead on `/login` | Calls the Bluesky AppView; needs a configurable address and a fake | M5 |
| OAuth sign-in, the callback, and token refresh | Needs a real authorization server; covered by signing in by hand | By hand, on every release that touches `crates/auth` |
| Record writes to a real PDS (`records.rs`, the `Pds` writer) | Tests use the in-memory writer by design (AGENTS.md); a fake PDS would test the fake | By hand: the M4 checks wrote, edited, and deleted real records |
| The live handle checker (`HandleChecker::Network`) | Resolves real handles over DNS and HTTPS | By hand; its store side is tested |
| `vote.js` and `login.js` | No browser test runner; adding one is a tooling decision | Open question for M5 |
| The systemd units and `build-hydrant.sh` on Linux | No Linux host in CI | By hand on the droplet; `docs/0002-deploy.md`, section 12 |
| `serve` start-up wiring in `src/main.rs` | Runs the whole server; exercised every time it starts | By hand; `make a11y` starts it |
| `triage check` | Calls the Anthropic API | By hand when a key is configured |

## M4 checks still to do by hand

| Check | Status |
|---|---|
| A late link change detaches the story; reattach from the tab | Waiting for the test story to pass the one-hour edit window |
| Deleting a record deletes the story | After the link change test |
| A reply written before its parent | Not yet run |
| Deactivating and reactivating an account | Needs a spare account |

Done by hand so far: a submission and a comment written straight to a repo,
an edit, Threaded's own story not indexed twice, public votes published and
counted once, a fresh database rebuilt by replay, tracking on sign-in, and
identity on user review.

## Found along the way

- **Moderator edits were recognized by who made them.** A story counted as
  moderator-edited when `override_by` was set, and that column is empty for
  an edit by the console or triage. Such an edit would have replaced what
  readers see without counting as a moderator's edit, so the author's next
  edit would overwrite it and a late one could detach the story wrongly. No
  path made such an edit yet. `mod_edited` now reads `override_at`, which is
  set and cleared with every override whoever makes it; the indexer test for
  a late link change under a moderator's edit covers it.
- **"Your account cannot comment" cannot happen.** Commenting is allowed at
  every level, and a banned account is refused earlier with its own message.
  The check stays as a guard and has no test.

## Coverage after the sweep

Measured with `cargo llvm-cov --workspace`.

| | Before | After |
|---|---|---|
| Lines | 82.9% | 88.7% |
| Functions | 80.2% | 83.7% |
| Tests | 176 | 199 |

| File | Before | After |
|---|---|---|
| `crates/indexer/src/apply.rs` | 86.3% | 96.3% |
| `crates/store/src/accept.rs` | 91.1% | 98.5% |
| `crates/store/src/moderation.rs` | 91.4% | 95.4% |
| `crates/store/src/triage.rs` | 86.2% | 94.2% |
| `crates/web/src/admin.rs` | 82.2% | 88.6% |
| `crates/web/src/staff.rs` | 89.9% | 98.9% |
| `crates/web/src/write.rs` | 82.0% | 94.7% |
| `src/main.rs` | 0% | 41.7% |

What stays low is the "Not in this sweep" list: `crates/auth/src/flow.rs`,
`crates/web/src/auth.rs`, `crates/web/src/identity.rs`, and the `serve`
start-up in `src/main.rs`.

Every new test was run and passes. Two were also run against a deliberately
broken version and failed as they should: the sign-in confirmation name, and
migration 0007 losing a row. The others were written against the behavior
as it is; a test that passes against a broken version would not show up
this way.
