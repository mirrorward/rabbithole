# Federated board Edit/Tombstone propagation ("board follow-ups")

Status: **approved, implementing** (2026-07-03). Extends the Wave 9 board-event
flood-fill (which is `Post`-only today) to carry `Edit` and `Tombstone`
follow-up events over S2S, with an authorization gate that stops a peer from
forging a correction/retraction of someone else's post.

## Goal

An `Edit` or `Tombstone` board event minted on one burrow propagates to every
subscribed peer the same way a `Post` does, so corrections and retractions
converge — without letting a malicious peer edit or retract a post that isn't
theirs.

## Current gap

- `EventBody` already has `Edit { target, subject, body, mime }` and
  `Tombstone { target }`.
- But they were never persisted as events: `BoardService::edit()` minted an
  `Edit` and **dropped** it; `tombstone()` minted nothing — both only mutated
  the projection (`apply_edit`/`apply_tombstone`).
- The flood serves events from `posts.event_blob` via `post_by_id`, and
  `ingest_fed_event` filtered to `EventBody::Post`. So follow-ups had nowhere
  to live and were rejected on ingest.
- The **wire is already kind-agnostic** (`FedEvent` carries opaque signed
  bytes; `IHave`/`PullRequest`/`PushEvents` don't inspect kind), so this needs
  **no proto/registry/version change** — it is server + store internal.

## Authorization policy (the security crux)

Authenticity and permission are separate. Every ingested event is already
verified — `SignedEvent::verify(&origin_key)` checks the content id + author
signature + origin signature under the peer's *pinned* origin key. On top of
that, a federated `Edit`/`Tombstone` targeting a local post `P` is **applied
only if EITHER**:

1. `event.author_key == P.author_key` — the **same author** acting on **their
   own** post. Forging this needs the author's private key.
2. `event.origin == P.origin` — the post's **home server** moderating **its
   own** content. A server is sovereign over content it originated, and its
   origin signature can't be forged by another peer.

Otherwise **refuse**: a third party can't edit/retract a post that isn't theirs
and didn't originate on the acting server (blocks the forge-someone-else's-edit
attack and cross-origin reach — server S can't redact server T's posts).

`P`'s `author_key`/`origin` come from decoding `P.event_blob` (the reserved
`posts` columns are written blank today), so both sides of the comparison are
`SignedEvent`s.

## Store (migration `0011_board_followups.sql`)

A dedicated `board_followups` table (kept separate from `posts` so the working
`Post` path is untouched):

```
event_id  BLOB PK    -- content id of the Edit/Tombstone event
target_id BLOB       -- the post it edits/retracts
root_id   BLOB       -- target's thread root (retention cascade)
board_slug TEXT
kind      INTEGER    -- 1 edit, 2 tombstone
origin    TEXT       -- origin server name
applied   INTEGER    -- 0 until the projection is caught up (out-of-order)
created_at INTEGER
event_blob BLOB       -- full signed event (flood source of truth)
```

`FollowupsRepo`: `insert` (idempotent on id), `by_id` (flood pull-serve),
`pending_for(target)` (reconciliation), `mark_applied`, and
`delete_for_root(root)` (retention cascade, called from `delete_thread`).

## Core (`BoardService`)

- `edit()` — additionally **store** its minted `Edit` event as an applied
  follow-up.
- `tombstone()` — **signature change**: takes the actor's identity
  (`actor_display`, `actor_seed`, `now_ms`) so it can mint + store a signed
  `Tombstone`, then `apply_tombstone`.
- `ingest()` — accept `Edit`/`Tombstone`: store the follow-up; **apply now**
  if the target post is present, else store `applied = 0` (out-of-order). On a
  **Post** ingest, reconcile: apply any pending follow-ups targeting it, in
  `created_at` order. Returns an enum (`Posted` | `Applied` | `Pending`).
- New accessor `followup_by_id`.

Reconciliation is a **local projection catch-up only** — it never re-floods (a
follow-up re-fires its bus event when it is first ingested, regardless of
whether its target was present yet).

## Bus + flood wiring

- New `ServerEvent::BoardEvent { board, id }` — distinct from `BoardPost`
  (which stays post-only so follow-ups don't bump unread counts). Local
  `edit()`/`tombstone()` publish it.
- Offer path listens for `BoardEvent` too, offers the id under the resolved
  board (same `IHave`).
- `handle_pull`: when an id isn't a post, fall back to `followup_by_id`.
- `ingest_fed_event`: allow `Edit`/`Tombstone`; verify, run the authorization
  gate, apply-or-pend + store, then re-fire `BoardEvent` for multi-hop relay.

## Out of scope (separate follow-ups)

- `rabbithole-federation::redaction::Redaction` — the cross-community
  moderation signal ("I no longer serve this hash," applied per receiver
  policy). Different, opt-in mechanism.
- Cross-origin moderator redaction (deliberately refused here).
- Periodic re-offer cadence for followups (out-of-order is handled by
  reconciliation; a re-offer sweep is a later refinement).

## Tests

- Repo: followup insert/dedup, `pending_for`, retention cascade.
- Core: edit/tombstone produce stored re-verifiable events; ingest applies each
  kind; out-of-order (edit before post) reconciles on post arrival.
- Authorization: author-edit applies; same-origin moderator applies;
  third-party forge refused; cross-origin refused.
- Extend `e2e_w9_floodfill`: an edit and a tombstone made on node A reach B and
  C (multi-hop), forged/cross-origin cases refused, loop-safe under a re-flood
  storm.
