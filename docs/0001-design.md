# Threaded design

Threaded (`threaded.at`) is a link aggregator. People share links and text
posts, tag them, vote, and discuss them in threaded comments. Anyone with an
atproto account can sign in: no passwords, no local accounts.

Every threshold is a setting.

## 1. Principles

- Reading never needs an account. Every listing, story, and search renders
  without JavaScript.
- A person is a DID. Handles are labels that change; nothing keys on them.
- Content belongs to its author. Submissions and comments are records in the
  author's repo, in site-neutral lexicons any aggregator can read. Upvotes are
  private unless the voter makes them public. Flags, saves, and filters stay
  private.
- Trust is earned here. Levels come from tenure and karma on this site, not
  from follower counts or account age elsewhere.
- Moderation is visible. Every removal, merge, edit, ban, and automated hold
  lands in a public log with a reason.
- Machines triage, people decide. AI acts alone only on clear spam, and a
  moderator reviews every one of those holds.

## 2. Identity and sign-in

### 2.1 OAuth client

Threaded is a confidential atproto OAuth client. The spec requires PAR, PKCE,
and DPoP with server-issued nonces for every client; confidential clients add a
signed client assertion (`private_key_jwt`, ES256). Confidential clients may
receive sessions with no overall lifetime cap and refresh tokens that last up to
180 days. Untrusted public clients get two weeks.

| Endpoint | Purpose |
|---|---|
| `/oauth-client-metadata.json` | Client metadata. Its URL is the `client_id`. |
| `/oauth/jwks.json` | Public half of the client assertion keys. |
| `/login` | Handle or PDS URL form. Starts PAR. |
| `/oauth/callback` | Exchanges the code, checks `sub`, creates the session. |
| `/logout` | Revokes the session and clears the cookie. |

Client metadata:

```json
{
  "client_id": "https://threaded.at/oauth-client-metadata.json",
  "client_name": "Threaded",
  "client_uri": "https://threaded.at",
  "application_type": "web",
  "grant_types": ["authorization_code", "refresh_token"],
  "response_types": ["code"],
  "redirect_uris": ["https://threaded.at/oauth/callback"],
  "scope": "atproto repo:at.threaded.submission repo:at.threaded.comment repo:at.threaded.vote?action=create&action=delete repo:app.bsky.feed.post?action=create",
  "token_endpoint_auth_method": "private_key_jwt",
  "token_endpoint_auth_signing_alg": "ES256",
  "jwks_uri": "https://threaded.at/oauth/jwks.json",
  "dpop_bound_access_tokens": true
}
```

### 2.2 Scopes

Threaded asks for the least it needs. Metadata declares every scope a sign-in
might request; each sign-in requests only the scopes for features that exist
and that the user has turned on. The metadata grows with the milestones: in M1
it declares `atproto` alone.

| Scope | Requested | Grants | From |
|---|---|---|---|
| `atproto` | Every sign-in | Identity only. Required by the spec. | M1 |
| `repo:at.threaded.submission` | Every sign-in | Create, update (edits), and delete the user's submissions | M2 |
| `repo:at.threaded.comment` | Every sign-in | The same for comments | M2 |
| `repo:at.threaded.vote?action=create&action=delete` | When the user makes upvotes public | Create and delete public upvotes; votes are never edited | M2 |
| `repo:app.bsky.feed.post?action=create` | When the user turns on cross-posting | Create Bluesky posts. It cannot be narrowed further, so it stays opt-in. | M5 |

Threaded never requests `transition:generic` (full account access), or any
`blob`, `rpc`, `account`, or `identity` scope. Turning on public upvotes or
cross-posting runs a second authorization with the larger scope set.

### 2.3 Rules the callback enforces

- The token response must carry a `scope` containing `atproto`, and `sub` must
  equal the DID resolved at the start of the flow. A mismatch aborts sign-in.
- Identity resolution during auth is cached for under ten minutes.
- Refresh tokens are single use. Refreshes for one session are serialized.
- The browser cookie is private (encrypted and signed) and holds only the
  session key. Tokens and DPoP keys stay in the database.
- Handles are shown only when they resolve back to the same DID. Otherwise the
  UI shows the DID.
- A verified handle belongs to one DID at a time: when a DID verifies a handle,
  any other account holding it loses the verification.
- Handles are checked at sign-in and again in the background, without making
  a request wait: when a page shows an account (as the signed-in user or as
  an author) whose check is over a day old, or over an hour old while it has
  no verified handle. A failed lookup keeps the stored handle and is tried
  again in an hour. From M4, hydrant's identity events update a handle within
  seconds. Bluesky's AppView works the same way: identity events, plus a daily
  re-check, hourly without a valid handle.

### 2.4 Library

[jacquard-oauth](https://tangled.org/nonbinary.computer/jacquard) 0.12.2 (MPL-2.0) implements the confidential client
(`Keyset`, `private_key_jwt`) and DPoP nonce handling, and builds on the stable
toolchain. Threaded supplies a SQLite-backed `ClientAuthStore` in
`crates/auth`.

Threaded builds `jacquard-common` and `jacquard-oauth` from patched copies in
`third_party/` on the [RustCrypto](https://github.com/RustCrypto/elliptic-curves) 0.14 curves, which keeps [crypto-bigint](https://github.com/RustCrypto/crypto-bigint) past
RUSTSEC-2026-0387. `third_party/README.md` lists the changes.

The pushed authorization request sends the client configuration's whole scope
list. Each scope set in §2.2 is therefore its own client configuration, all
published under the one metadata document.

### 2.5 Local development

The spec's localhost exception gives a public client with no hosted metadata:

```text
client_id = http://localhost/?redirect_uri=http%3A%2F%2F127.0.0.1%3A8080%2Foauth%2Fcallback&scope=atproto
```

The redirect host must be `127.0.0.1` or `[::1]`; ports are not matched. Dev
sessions are public-client sessions with public-client lifetimes.

Local builds write `at.threaded.dev.submission`, `.comment`, and `.vote`
instead of `at.threaded.*`, so test records on a real PDS never reach the
production index. Automated tests use an in-memory record writer.

### 2.6 Sign-in form

Sign-in happens on the server that hosts the account, never on Threaded. The
pages say so before the user leaves, name that server, and send the user to it
in the same tab, so the browser's address bar shows the server's own domain.

**Step 1: `/login`.** One field, "Handle or server", with a visible label. It
accepts a handle (a leading `@` is dropped), a DID, or the `https://` address
of a PDS. Under the field:

> You sign in on the server that hosts your account, not on Threaded. Threaded
> never sees your password.

The form posts to `/login` and works without JavaScript. Errors render above
the field, are tied to it with `aria-describedby`, and keep the typed value. A
second button, "Create an account on Bluesky", starts the flow with
`https://bsky.social` as the server.

**Step 2: `/login/confirm`.** Threaded resolves the identifier, finds the
account's Authorization Server, and sends the pushed authorization request.
Before redirecting, it shows where the user is going:

> **Sign in at bsky.social**
>
> Your account @alice.example.com is hosted at **bsky.social**. You will sign
> in there, and it will show what Threaded is asking permission for. Threaded
> never sees your password. When you finish, you come back to threaded.at.
>
> Before you type your password, check that your browser's address bar shows
> **bsky.social**.
>
> [Continue to bsky.social] · Use a different account

- The named host is the Authorization Server's: the host of its
  `authorization_endpoint`. For accounts on Bluesky's hosting that is
  `bsky.social` while the PDS is a host such as
  `morel.us-east.host.bsky.network`, and the address bar will show
  `bsky.social`. A self-hosted PDS is usually its own Authorization Server.
- When the PDS host differs from the Authorization Server's, a second line
  reads "Account data stored at morel.us-east.host.bsky.network".
- Hosts with non-ASCII characters show in their ASCII (`xn--`) form beside the
  Unicode form, so a look-alike domain is visible.
- The button is a real link to the authorization URL. The user must press it;
  the page never redirects on its own. The button's text names the host, so a
  screen reader announces "Continue to bsky.social".
- The PDS sign-in never opens in a pop-up, frame, or new tab.
- The identifier goes to the Authorization Server as `login_hint` exactly as
  entered or picked.

**Step 3: `/oauth/callback`.** On success the user lands back on the page they
started from. On failure the page names the host that refused and why, in plain
words ("bsky.social declined the sign-in" or "You chose not to allow
Threaded"), with a link to try again.

### 2.7 Handle lookahead

Typing in the field suggests accounts. It is a progressive enhancement: with
scripts off, or when suggestions fail, the field is a plain text input.

**Data.** `GET /login/typeahead?q=` proxies `app.bsky.actor.searchActorsTypeahead`
on the public AppView (`https://public.api.bsky.app`) with `limit=6`. The
endpoint needs no auth and treats `q` as a prefix. Threaded caches each answer
for 30 seconds, the AppView's own `max-age`, and rate-limits per IP. Proxying
keeps the page at `connect-src 'self'` and keeps visitors' IP addresses off the
AppView. Avatars load from `cdn.bsky.app`.

**When it queries.** After two characters, 150 ms after the last keystroke. An
`AbortController` cancels the request in flight, and a response applies only if
it matches the current input. No query runs when the input starts with `did:`
or `http`, or contains `/`.

**Coverage.** Suggestions come from the Bluesky AppView's index. An account it
does not index signs in by typing the full handle.

**Markup.** The WAI-ARIA combobox pattern with a list popup, in the light DOM
next to the input:

| Element | Attributes |
|---|---|
| `<input>` | `role="combobox"`, `aria-autocomplete="list"`, `aria-expanded`, `aria-controls` (the listbox), `aria-activedescendant` (the highlighted option) |
| `<ul>` | `role="listbox"`, labelled "Suggested accounts" |
| `<li>` | `role="option"`, `id`, `aria-selected`; accessible name "Display Name, @handle"; avatar `alt=""` |
| Status line | `role="status"`: "6 accounts found", "No matching accounts" |

**Keys.** Down and Up move the highlight and open the list. Enter picks the
highlighted account; with nothing highlighted it submits the form. Escape closes
the list, and a second Escape clears the field. Tab leaves without picking.
Focus stays in the input throughout.

**Prior art.** `actor-typeahead` (Jake Lazaroff; fork `@tijs/actor-typeahead`)
is the component kipclip.com and dropanchor.app use for this, and Coral's atproto
sign-in pull request adopts it. It keeps
the list in a closed shadow root while the input stays in the page. ARIA ID
references (`aria-controls`, `aria-activedescendant`) do not cross a shadow
boundary, so a screen reader hears no suggestions; cross-root reference targets
are experimental in browsers. Its requests also have no debounce or
cancellation. Threaded uses the same endpoint with its own markup.

## 3. Records and lexicons

### 3.1 What Threaded writes

| Data | Stored as | Visibility |
|---|---|---|
| Submissions | `at.threaded.submission` | Public, in the author's repo |
| Comments | `at.threaded.comment` | Public, in the author's repo |
| Upvotes | Threaded database, and `at.threaded.vote` when the voter opts in | Private by default; public per user |
| Flags and reasons | Threaded database | Private |
| Saved, hidden, tag filters | Threaded database | Private |
| Moderator notes | Threaded database | Moderators only |
| Mod log | Threaded database, published as a page | Public |

The schemas are in `lexicons/at/threaded/`. They are written to be site-neutral:
nothing in them names Threaded or assumes its tag set.

```json
{
  "$type": "at.threaded.submission",
  "title": "Writing a B-tree from scratch",
  "url": "https://example.com/btree",
  "tags": ["databases", "rust"],
  "authored": false,
  "createdAt": "2026-10-25T18:00:00Z"
}
```

```json
{
  "$type": "at.threaded.comment",
  "subject": { "uri": "at://did:plc:abc/at.threaded.submission/3m...", "cid": "bafy..." },
  "parent": { "uri": "at://did:plc:def/at.threaded.comment/3m...", "cid": "bafy..." },
  "text": "Markdown body",
  "createdAt": "2026-10-25T18:05:00Z"
}
```

`parent` is absent on top-level comments. `authored` is the submitter's claim to
be the author of the linked page. Text fields cap at 5,000 graphemes.

Flags are never published: a visible flag is a target.

### 3.2 Public upvotes

Upvotes are private by default. A setting, "Make my upvotes public", publishes
them as `at.threaded.vote` records in the voter's repo:

```json
{
  "$type": "at.threaded.vote",
  "subject": { "uri": "at://did:plc:abc/at.threaded.submission/3m...", "cid": "bafy..." },
  "createdAt": "2026-10-25T18:06:00Z"
}
```

- The `votes` table stays the source of truth for score and rank. Turning the
  setting on or off changes no number.
- With the setting on, an upvote writes the record and stores its URI on the
  vote row; removing the upvote deletes the record.
- Turning the setting on asks whether to publish past upvotes or only new ones.
- Turning it off deletes every published vote record. The settings page states
  that copies made by indexers and other apps before then cannot be recalled.
- Public upvotes show on the voter's profile as "Liked". Private upvotes never
  appear anywhere.
- Vote-ring and abuse checks read every vote, public or private.

### 3.3 Publishing the schemas

The `at.threaded.*` schemas publish under the `threaded.at` domain authority:
a `_lexicon.threaded.at` TXT record names the publishing DID, and
`goat lex publish` writes them as `com.atproto.lexicon.schema` records. Lint
with `goat lex lint`.

### 3.4 Indexing

The database is an index over records, plus the private tables in §3.1.

1. **Write-through.** Submitting from Threaded writes the record to the user's
   PDS, then inserts the index row with the returned URI and CID in the same
   request.
2. **hydrant.** A [hydrant](https://hydrant.klbr.net) indexer runs beside
   Threaded in `filter` mode. `FILTER_SIGNALS` and `collections` list the
   exact NSIDs `at.threaded.submission`, `at.threaded.comment`, and
   `at.threaded.vote`. Votes are a signal because hydrant drops an account
   with no signal record, so an account that only votes would never arrive. The list is exact, not `at.threaded.*`:
   a wildcard matches by prefix and would also store `at.threaded.dev.*`
   records. hydrant finds accounts two ways: live commits that touch a
   signal collection, and its crawler's `com.atproto.sync.listReposByCollection`
   queries for accounts that already hold such records. The crawler rejects
   wildcard signals, so the signals stay exact. Every discovered account is
   backfilled, so records written before Threaded started, or while it was
   down, still arrive. Threaded reads hydrant's ordered `/stream` from its
   stored cursor and applies creates, edits, deletes, and identity and account
   events. Record events replay; identity and account events do not, so a new
   Threaded database replays records from the start and reads each author's
   handle and status from `GET /repos/:did`.

A record from any app passes the same checks as the web form: level, rate
limits, tag rules, duplicate detection, triage. Tags outside Threaded's set are
ignored; a submission left with no known tag is not indexed. Records from
banned DIDs are not indexed.

An `at.threaded.vote` record counts only when its author has signed in to
Threaded and may upvote, and only once per account and subject: a published
vote and the same user's vote row are one vote. Vote records from accounts
that have never signed in are ignored. Threaded also adds every signed-in DID to
hydrant through its `/repos` endpoint, so an account hydrant's crawler has
not listed yet is backfilled.

### 3.5 Edits and deletes

- A submission's URL is fixed one hour after posting. A later URL change
  detaches the story: it is hidden and the change is logged for moderators.
- Title, text, and tag edits are accepted and the story shows "edited".
  Moderators can see prior versions.
- An author deleting a record removes the content here. A comment with replies
  leaves a "[deleted]" placeholder so the thread holds together.
- A moderator removal hides content here and logs it. Threaded cannot delete a
  record from someone's PDS, and the UI never says it has.
- An account deactivation or takedown event hides that DID's content until a
  reactivation event arrives.

## 4. Stories

- A story is a link, a text post, or both. Title is 1 to 150 characters.
- Link canonicalization for duplicate detection: lowercase host, drop `www.`,
  drop the fragment and trailing slash, drop `utm_*` and known tracking params,
  sort the remaining query. A duplicate within 30 days redirects the submitter
  to the existing story.
- `show` and `ask` are ordinary tags. `show` requires `authored`; `ask` is
  text-only.
- Moderators merge duplicates. The merged story's comments move under the
  surviving story, and its score feeds the survivor's rank.
- Stories lock against new comments after 90 days or by moderator action.
- Role badges: a moderator grants a user a verified role, such as "Maintainer
  of quilldb", with an optional link. The user chooses per comment whether to
  show it. Grants and removals are logged.

## 5. Tags

- Every story carries 1 to 5 tags. Names are lowercase ASCII letters, digits,
  `-`, `_`, `+`, up to 25 characters.
- Admins create tags. Users cannot invent them.
- Tag attributes:

| Attribute | Effect |
|---|---|
| `category` | Groups tags on `/tags`. |
| `description` | Shown on hover and on `/tags`. |
| `rank_boost` | Added to rank, -10 to 10. `meta` uses -0.5. |
| `privileged` | Only moderators may apply it (`announce`). |
| `new_users_ok` | When off, New users cannot apply it. |
| `is_media` | Marks video, audio, PDF. Shown as a badge. |
| `active` | Inactive tags stay on old stories but cannot be applied. |

- Users filter tags out of every listing. Filters are private.
- Trusted users suggest tag and title changes. A change applies when two
  Trusted users suggest the same edit, and the mod log records it as a
  community edit.
- Each tag has `/t/{tag}` and an Atom feed.

## 6. Sorting and ranking

| Listing | Order |
|---|---|
| `/` front page | `rank` descending |
| `/newest` | Submission time descending |
| `/active` | Last comment time descending |
| `/top?t=day\|week\|month\|year\|all` | Score descending within the window |
| `/t/{tag}` | Rank, filtered to the tag |
| `/domain/{domain}` | Newest, domain and its subdomains |
| `/u/{handle}` | A user's stories and comments, newest first; a DID works too |

### 6.1 Rank

```text
base    = sum(tag.rank_boost) + (authored && is_link ? 0.25 : 0)
cpoints = base < 0 ? 0 : min(0.5 * sum(comment.score + 1), max(score, 0))
order   = log10(max(|score + 1| + cpoints, 1))
rank    = order * sign(score) + base + created_at / 79200
```

Comments by the submitter are excluded from `cpoints`. 79,200 seconds is 22
hours: a story needs ten times the score to hold its place against one 22 hours
newer. Comment points cap at `max(score, 0)`: a cap at the raw score would let
a negative score cancel its own weight, so a story at -10 would rank level with
one at 0. With the cap at zero, flagged stories sink.

Rank is stored on the row and recomputed on every vote, flag, comment, tag
change, and merge. Every listing is an index scan.

### 6.2 Score

`score = 1 + upvotes - flags`. The submitter's implicit vote is the 1. Comment
score works the same way. A user's karma is the sum of their stories' and
comments' scores minus one per item.

## 7. Search

SQLite FTS5 over title, text, URL, and domain (porter stemmer, `unicode61`).
Ranking is BM25 with title weighted 10, text 1, URL and domain 2.

| Syntax | Meaning |
|---|---|
| `word` | Term must match |
| `"exact phrase"` | Phrase must match |
| `-word` | Term must not match |
| `title:word` | Term in the title |
| `tag:rust` / `-tag:job` | Has or lacks the tag |
| `domain:example.com` / `-domain:...` | Domain or subdomain |
| `submitter:handle` | Submitted by |
| `commenter:handle` | Has a comment by |
| `sort:new` / `sort:score` | Order other than relevance |

Every term reaches FTS5 quoted, so query text never becomes FTS5 operator
syntax. Comment search uses a second FTS5 table with the same grammar.

## 8. User levels

Levels below Moderator are computed on each request from two numbers: tenure
(days since first sign-in here) and karma. They rise and fall without anyone
acting. Moderator and Admin are granted roles.

| Level | Criteria |
|---|---|
| New | Tenure under 70 days, not vouched |
| Member | Past probation, karma under 50 |
| Trusted | Past probation, karma 50 or more |
| Moderator | Granted by an Admin |
| Admin | Granted by an Admin |

| Capability | New | Member | Trusted | Moderator | Admin |
|---|---|---|---|---|---|
| Read, upvote, comment | yes | yes | yes | yes | yes |
| Submit (karma -4 or more) | yes | yes | yes | yes | yes |
| Use tags with `new_users_ok` off | no | yes | yes | yes | yes |
| Flag stories | no | yes | yes | yes | yes |
| Flag comments (karma 50 or more) | no | no | yes | yes | yes |
| Suggest title and tag edits (karma 10 or more) | no | yes | yes | yes | yes |
| Vouch for a New user | no | no | yes | yes | yes |
| Use privileged tags, moderate | no | no | no | yes | yes |
| Manage tags, grant roles | no | no | no | no | yes |

**Vouching.** Sign-in is open to any DID. A
Trusted user who vouches for a New user ends that user's probation, and the
voucher's name appears on the vouched profile. Moderators see vouch trees when
reviewing a ban.

**New-user limits:**

| Limit | New | Member and up |
|---|---|---|
| Stories per day | 2 | 10 |
| Comments per hour | 6 | 30 |

New users' stories carry a "new user" marker in listings for their first 70
days.

## 9. Moderation

### 9.1 Flags

A flag is a downvote with a reason:

| On | Reasons |
|---|---|
| Stories | Off-topic, Already posted, Broken link, Spam |
| Comments | Off-topic, Me-too, Troll, Unkind, Spam |

- A flag subtracts one from score and recomputes rank.
- Stories are flaggable for 14 days, comments for 7. Items at -5 (stories) or
  -10 (comments) take no more flags.
- An item with 3 or more flags enters the moderation queue.
- A user sees that their own item was flagged and with which reasons, never by
  whom.

### 9.2 AI triage

Every new submission and comment, from the web form or from hydrant, goes
through triage after it is indexed. Triage never delays posting.

Triage runs only when an Anthropic API key is configured. Without one, no
model is called, nothing is held, and moderation runs on flags and moderators
alone.

| Verdict | Action |
|---|---|
| `spam` at `high` confidence | Held: hidden from listings, shown to its author as "held for review: spam (automated)", logged publicly, placed at the top of the queue |
| Any other rule violation | Suggested verdict attached to the queue item; nothing visible changes |
| `ok` | Nothing |
| `needs_human` | Queued for a moderator |

A moderator confirms or releases every hold. Releasing restores the item and
logs the release. Bans, merges, and edits are never automated.

**Input.** The written site rules, the item (title, URL, domain, tags, text),
the parent comment for replies, flag reasons if any, and the author's level,
tenure, and count of prior removals. Never votes, private notes, or IP
addresses.

**Output.** Structured, validated against a JSON schema:

```json
{
  "verdict": "spam",
  "rule": "No promotional links without discussion",
  "confidence": "high",
  "explanation": "Fourth link to the same storefront today; no text."
}
```

`verdict` is `ok`, `needs_human`, or one of the flag reasons in §9.1.

**Model.** `claude-opus-5-5` at `low` effort through the Messages API, called
over HTTPS with `reqwest` (Anthropic ships no Rust SDK). The response format is
set with `output_config.format`. The rules document is the cached system
prompt, so each call pays for the item alone. A `refusal` stop reason, a
timeout, or an invalid response becomes `needs_human`. The model ID is a
setting.

**Consent.** A user setting, "Keep my posts out of AI triage", sends that
user's items to the human queue without calling the model. While triage is
off (no API key, or no site rules yet), the settings page says so, and that
triage may become opt-in rather than opt-out; a choice made now still
counts.

**Accountability.** Each verdict stores the model ID, the rules version, and
the moderator's final decision. `/moderations/stats` publishes how often
moderators confirmed holds and agreed with suggestions. When confirmed holds
fall below 95% of holds over the last 100, once at least 20 are decided,
automatic holds switch off and every spam verdict becomes a suggestion until
an admin resets the breaker.

### 9.3 Moderator actions

Every moderator action writes a `mod_log` row with moderator, target, and reason.
`/moderations` lists the log publicly; automated holds appear with "triage" as
the actor and the rule cited.

| Action | Target |
|---|---|
| Remove, restore | Story, comment |
| Confirm, release | Triage hold |
| Edit title, tags, URL | Story |
| Merge | Story into story |
| Lock, unlock | Story |
| Ban, unban | DID |
| Grant, revoke | Role badge, role |
| Create, edit, deactivate | Tag |

A moderator's edit is stored as an override that the author's later edits do
not replace. A merge moves comments and votes to the target story and can be
undone. A lock stops comments, votes, and flags. A role badge is a label with
an optional link, shown after the handle; moderators grant badges, admins
grant roles. The M3 plan, `0005-m3.md`, has the details.

Moderators write private notes on users (`mod_notes`). §9.7 and §9.8 describe
the pages where all of this happens.

### 9.4 Bans

A ban is keyed on the DID. It hides nothing retroactively unless the moderator
also removes content. A banned DID can read and sign in, and cannot write. Its
new records are not indexed.

### 9.5 atproto labels

Threaded reads account-level labels from the labelers named in its settings and
hides content from accounts labelled `!takedown` or `!hide`. Users can add
labelers they trust; labels from those apply only to that user's view.

### 9.6 Mod mail

Users message the moderator team from `/modmail`. Threads are visible to all
moderators. Appeals of holds and removals go there.

### 9.7 Moderator views

Moderator pages live under `/mod` and need the Moderator level. Every action
is a form post that requires a reason, and the reason goes to the public mod
log. Pages follow the accessibility rules in §13; queue rows are a list of
`<article>` elements with real buttons.

**Queue, `/mod`.** One page with a tab per kind of work, each tab showing its
count:

| Tab | Holds | Default order |
|---|---|---|
| Holds | Items triage hid for spam | Oldest first, so nothing waits long |
| Flagged | Items with 3 or more flags | Most flags first |
| Suggestions | Triage verdicts other than `ok` | Highest confidence first |
| Detached | Stories whose URL changed after the one-hour window | Oldest first |
| Mod mail | Open threads | Oldest unanswered first |

A row shows the item, its author with level and tenure, the flags grouped by
reason with counts, and the triage verdict with its explanation. The row's
buttons act on the verdict (confirm or release a hold; remove or dismiss a
suggestion). A Detached row shows the old and new link, and its buttons
reattach the story with the new link or remove it. Optional keys, off by default: `j`/`k` move between rows, `o`
opens the item review.

**Item review, `/mod/s/{id}` and `/mod/c/{id}`.**

- The item as readers see it, and its thread for comments.
- Prior versions with what changed between them.
- Every flag with its reason and the flagger. Moderators see who flagged;
  nobody else does.
- The triage verdict, its explanation, the model, and the rules version.
- Every action from §9.3 that applies to the item, each with a reason field.
- The item's entries in the mod log.

**User review, `/mod/u/{did}`.** A handle in the address finds the account and
redirects to its DID; actions on the page name the account by DID only.

- Identity: the verified handle, DID, PDS host from hydrant, every verified
  handle the account has held, and, for a `did:plc`, when the PLC directory
  first saw it. hydrant and the directory are asked when the page loads, with
  a short wait; the page says when either does not answer.
- Standing: level, tenure, karma, who vouched for them, and whom they vouched
  for, as a tree.
- Activity: recent stories and comments, removals, flags received by reason,
  and flags given with how often moderators agreed with them.
- Moderator notes, newest first, with a form to add one. Notes are private.
- Actions: ban or unban with a reason, grant or remove a role badge.

### 9.8 Admin views

Admin pages live under `/admin` and need the Admin level. Changes are logged
in the mod log; settings changes record the old and new value.

| Page | What it holds |
|---|---|
| `/admin` | Health: hydrant stream lag (its newest event against Threaded's cursor) and records waiting for their story, triage queue depth and provider errors, queue counts per tab, hold precision and the circuit breaker's state with a reset button |
| `/admin/tags` | Every tag with its category, description, rank boost, and flags (`privileged`, `new_users_ok`, `is_media`, `active`); create and edit forms |
| `/admin/people` | Moderators and admins, with grant and revoke; role badges in use |
| `/admin/settings` | Level thresholds, rate limits, flag windows and limits, labelers, hold-precision floor, triage model ID |
| `/admin/rules` | The site rules that triage reads, with version history; once 50 moderator decisions on triage verdicts exist, a new version must pass an eval run against them before it takes effect |

## 10. Atmosphere features

| Feature | Description |
|---|---|
| Project submissions | `show` + `atproto` tags. A tangled.org repo URL renders a repo card; an `at://` URI renders the record. |
| Lexicon search | `/lexicons?q=` resolves an NSID through its `_lexicon` DNS record and shows the published schema, with the Threaded stories that mention it. |

## 11. Reader features

| Feature | Description |
|---|---|
| Replies inbox | Replies to your stories and comments, unread count in the header |
| Read ribbons | "3 new" on stories with comments since your last visit |
| Liked | Public upvotes on the profile, for users who opt in (§3.2) |
| Saved stories | Private list |
| Hidden stories | Removed from your listings, profiles, and search |
| Tag filters | A filtered tag's stories leave your listings, except the tag's own page and a search for `tag:` that tag |
| Feeds | Atom for the front page, newest, each tag, each domain, each user, and the blog |
| Bluesky cross-post | Opt-in per story; posts the title and story link to Bluesky |
| Archive links | `archive.org` and `archive.today` links on every link story's page, each to the newest copy |
| Comment folding | Collapse a comment and its replies; the folded comments are kept in the URL fragment |
| Keyboard navigation | `j`/`k` through stories and comments, `o` to open a story; off until turned on in settings (WCAG 2.1.4) |

## 12. Blog

Site updates are Markdown files in a directory (`THREADED_BLOG_DIR`, default
`./blog`). Adding, editing, or deleting a file takes effect within a second,
with no restart.

| Route | Page |
|---|---|
| `/blog` | Published posts, newest first, 10 per page; `?page=2` and on |
| `/blog/{slug}` | One post |
| `/blog/feed` | Atom feed of the newest 30 posts |

A post is YAML front matter between `---` lines, then the body:

```markdown
---
title: Sign-in is live
date: 2026-11-02
summary: Any atproto account can now sign in.
author: Threaded
---
Body in CommonMark, with tables, strikethrough, and task lists.
```

| Field | Required | Rule |
|---|---|---|
| `title` | yes | Not empty |
| `date` | yes | `YYYY-MM-DD`, a real calendar date. Sets the index order. |
| `slug` | no | Lowercase letters, digits, and hyphens, and not `feed`. Defaults to the file name without `.md`. |
| `summary` | no | Shown on the index |
| `author` | no | Shown under the title |
| `draft` | no | `true` keeps the post off the site |

Unknown fields are an error, so a misspelled field fails loudly. A file that
fails to parse is skipped and logged with the reason, and the rest of the blog
keeps serving. When two files claim one slug, the one whose file name sorts
first wins and the other is logged. Files that start with `.` or do not end in
`.md` are ignored.

The directory is rescanned at most once per second, on the first request after
that second; only files whose size or modification time changed are read
again. Markdown headings render one level down, so the post title stays the
page's only `<h1>`. The rendered HTML goes through `ammonia` like all other
Markdown. Slugs are looked up in the loaded index, never joined to a path.

## 13. Accessibility

Threaded targets WCAG 2.2 level AA on every page, in both color schemes.

| Area | Rule |
|---|---|
| Structure | One `<h1>` per page, a distinct `<title>`, landmarks (`header`, `nav` "Primary", `main`, `nav` "Pages"), a skip link to `main`, `aria-current="page"` on the current nav item |
| Stories | An ordered list. The score reads as "48 points". Times are `<time datetime>` with the exact time available. |
| Comments | Nested lists; each comment is an `<article>` labelled by author and age, so depth comes from list nesting |
| Voting | `<button aria-pressed>` named "Upvote" plus the story title. Works as a form post without JavaScript. |
| Flagging | A disclosure button opens a group of radio buttons, one per reason, and a submit button. No hover menus. |
| Color | Text at least 4.5:1, borders and focus rings at least 3:1, in light and dark. State is never color alone: "new user", "held", and "edited" are words. |
| Focus | A 2px accent ring on `:focus-visible`. Dialogs and menus return focus to the control that opened them. |
| Targets | At least 24 by 24 CSS pixels, or spaced so a 24px circle around each touches no other (WCAG 2.5.8) |
| Reflow | No horizontal scroll at 320 CSS pixels wide; text survives WCAG 1.4.12 spacing overrides |
| Motion | No motion is required to use anything; `prefers-reduced-motion` turns transitions off |
| Shortcuts | `j`/`k` and other single-key shortcuts act only when focus is outside text fields, and a setting turns them off (WCAG 2.1.4) |
| Forms | Visible labels, errors as text tied with `aria-describedby`, no CAPTCHA |
| Language | `lang="en"` on the page; a submission's `langs` sets `lang` on its title and text |
| User content | Markdown renders no images, so no image lacks a text alternative |

### 13.1 Testing

- `make a11y` seeds a scratch database, serves it, and runs axe-core 4.14 on
  every page in light and dark with the WCAG 2.0, 2.1, and 2.2 A and AA rules
  plus best practices. CI runs it on every push.
- Route tests assert one `<h1>`, a distinct `<title>`, the skip link, and
  `aria-current` on every page.
- Automated rules find a subset of problems. Before a milestone ships, its new
  flows are walked with VoiceOver on macOS and iOS and NVDA on Windows, by
  keyboard alone, and at 200% and 400% zoom.

## 14. Architecture

```text
threaded (binary)     CLI: serve, seed. Wires store, web, stream consumer, triage.
crates/core           Pure logic: ranking, levels, search parser, links, tags.
crates/store          SQLite via sqlx. Migrations in migrations/.
crates/web            axum router, askama templates, static CSS, blog loader.
crates/triage         AI triage: the Messages API request, verdicts, the worker.
lexicons/             at.threaded.* schemas.
```

- Two processes: `threaded` with one SQLite file in WAL mode, and `hydrant`
  with its own [fjall](https://github.com/fjall-rs/fjall) database. They talk over hydrant's HTTP and WebSocket API
  on a Unix socket. hydrant builds from a pinned commit; it is not on
  crates.io, and its forked storage crates come from git.
- On disk: the SQLite file, hydrant's database, and the blog folder of
  Markdown files (§12).
- The stream consumer and the triage worker are tasks in the `threaded`
  process. Triage reads from a queue table, so a restart loses
  nothing.
- Pages render on the server. Forms work without JavaScript.
- Frontend JavaScript is vanilla: small scripts in `crates/web/static/`, served
  as written. No framework, no build step, no npm packages in the browser.
  Each script enhances a page that already works without it.
- Markdown renders with `pulldown-cmark`, then `ammonia` sanitizes the HTML.
  Rendered HTML is cached on the row.
- Times are unix seconds. Short IDs are six random base-36 characters.

### 14.1 Security

| Concern | Control |
|---|---|
| CSRF | `SameSite=Lax` cookies plus a per-session token on every POST |
| XSS | askama auto-escaping; sanitized Markdown; CSP without `unsafe-inline`, `connect-src 'self'`, `img-src 'self' https://cdn.bsky.app` |
| Link schemes | Only `https` URLs are accepted, for stories, badges, and links in user text |
| FTS injection | Every search term is quoted |
| Prompt injection | Item text reaches the model as data inside the user turn; the verdict schema allows no free-form action; the worst outcome is a wrong hold, which a moderator reviews |
| Abuse | Per-DID and per-IP rate limits; level-based limits in §8 |
| Token theft | OAuth tokens, DPoP keys, and the Anthropic API key never reach the browser |
| Key compromise | Remove the key from the JWKS; sessions bound to it end |
| Dependencies | `cargo audit` in CI fails on any vulnerability; a vulnerable dependency is upgraded or patched in `third_party/`, never ignored |

## 15. Data model

Defined in `migrations/0001_init.sql`.

| Table | Holds |
|---|---|
| `users` | DID, handle, role, karma, public-upvotes setting, vouch, ban |
| `stories` | Index of submission records, score, rank, moderation state |
| `comments` | Index of comment records, threading, moderation state |
| `tags`, `taggings` | Tag definitions and story tags |
| `votes` | Upvotes, with the published record URI when public, and private flags with reasons |
| `saved_stories`, `hidden_stories`, `tag_filters` | Private preferences |
| `role_badges` | Granted role badges |
| `mod_log`, `mod_notes` | Public log, private notes |
| `stories_fts`, `comments_fts` | FTS5 indexes kept in step by triggers |

Later migrations add `oauth_sessions` and `oauth_requests` (M1),
`story_revisions` and `comment_revisions` (M2), `triage_queue`,
`triage_verdicts`, `rules_versions`, `settings`, and `suggestions` (M3), and
`stream_cursor`, `pending_records`, and `handle_history` (M4).

## 16. Milestones

| Milestone | Scope | State |
|---|---|---|
| M0 Scaffold | Workspace, schema, ranking, levels, search parser, link canonicalization, read-only listings, search, tag and domain pages, blog, lexicon drafts, accessibility audit | Built |
| M1 Sign-in | OAuth confidential client, metadata and JWKS endpoints, SQLite auth store, sessions, handle verification, sign-in form with handle lookahead, localhost dev flow | Built |
| M2 Writing | Submit, comment, reply, upvote, flag, edit, delete; records to the PDS; public upvotes; profiles; Markdown; rate limits; levels enforced | Built |
| M3 Moderation | Mod log, moderator queue and item and user review, admin pages, actions, notes, bans, merges, role badges, suggestions, AI triage | Built |
| M4 Indexer | hydrant beside Threaded, stream consumer with cursor, backfill, edits, deletes, account events, records and votes from other apps, the Detached queue tab, hydrant lag on `/admin` | Built |
| M5 Reader features | Inbox, read ribbons, saved, hidden, filters, feeds, cross-post, labelers, archive links, comment folding, keyboard navigation | Next |
| M6 Atmosphere | Project submissions, lexicon search, network stats | |
| M7 Polish | Themes chosen by the reader (light, dark, the system's choice, and Hotdog Stand), each passing `make a11y`; WCAG work beyond AA where it helps readers, such as a high-contrast theme at 7:1 and reduced motion; a full keyboard, screen reader, and zoom pass over every flow (§13.1) | |

Not yet in a milestone:

- Mod mail and appeals (§9.6), and the queue's Mod mail tab.
- Vouching and vouch trees (§8).
- The eval gate on rules versions (§9.8), once 50 moderator decisions on
  triage verdicts exist to test against.

## 17. Open questions

- Whether to read other atproto aggregators' records (`cafe.shelf.item`,
  `at.upvote.signal`, `app.userinput.upvote`) as signals.
- Whether to publish a permission set (`include:` scope) so the consent screen
  shows one named grant.
- Whether Threaded runs its own labeler so its moderation decisions are visible
  to other atproto apps.
- Whether stories should also surface Bluesky replies to the cross-posted
  announcement. hydrant's `backlinks` feature answers "which records reference
  this post" through `blue.microcosm.links.getBacklinks`.
- Whether to embed hydrant as a library in the `threaded` binary once it is
  published to crates.io.
- Whether to add network stats (events per collection namespace over time).
  hydrant in filter mode sees only Threaded's records, so this needs a
  full-network source.
- Whether to give link previews an image (`og:image`) per story. Pages carry
  `og:title` and `og:description` now, and previews show text only. An image
  would be drawn when first requested and cached, not when the story is
  submitted, since titles change through edits and moderator overrides.
- Whether AI triage should be opt-in rather than opt-out (§9.2, consent)
  before it is turned on for threaded.at.
- Whether to move triage (§9.2) onto aimod, the standalone triage service
  designed in its own repository, once aimod ships. Triage stays in Threaded
  until then.
