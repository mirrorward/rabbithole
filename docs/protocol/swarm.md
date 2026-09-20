# RHP Swarm Family (6)

Status: **Wave 5** — the coordinator surface (advertise, find-sources,
TTL soft state), capability tokens, the Bao-verified peer wire, and the
multi-source work-stealing scheduler are live; NAT traversal (hole
punching + server relay fallback) is still pending. Manifests,
`rabbit://` links, `CapToken`, and the peer wire itself live in the
`rabbithole-swarm` crate.

| type | name | direction | payload |
|---|---|---|---|
| 1/2 | AdvertiseFiles → AdvertiseAck | Request/Reply | entries {root, size, name, mime} + requested ttl; ack reports accepted / granted ttl / account total; needs SWARM_ADVERTISE |
| 3 | AdvertWithdraw | Request → ack | roots (empty = everything this session advertised) |
| 4/5 | FindSources → SourceList | Request/Reply | root → advertising peers + whether the origin's blob store has it; needs FILE_LIST |
| 6 | PeerContact | Request → ack | this session's peer-wire port + cert fingerprint; needs SWARM_ADVERTISE |
| 7/8 | SourceTicketRequest → SourceTicket | Request/Reply | root → server-signed capability token (opaque `CapToken` bytes + expiry); needs FILE_DOWNLOAD |
| 9/2 | AdvertisePartial → AdvertiseAck | Request/Reply | as AdvertiseFiles, for files this session holds only part of so far (0.229); advertising a root whole later replaces it, and a partial re-announce never demotes a whole one. Older burrows answer `Unsupported` |
| 10/5 | FindAllSources → SourceList | Request/Reply | as FindSources, partial seeds included (0.229), for a fetcher that asks each source which part it holds. Older burrows answer `Unsupported`; the client then asks FindSources |

FindAllSources never lists the asking session as a source for itself (a
person sharing a download as it comes in has an advert of their own).
FindSources never lists a partial seed, so a client from before them never
meets a peer that lacks units.

## List-without-upload

A peer **advertises** files it holds locally — just the blake3 root and
catalog metadata, no bytes. The server keeps the who-has-what map as
**soft state**:

- Every advert carries a TTL. The client's `ttl_secs` is capped at the
  server's `swarm_advert_ttl_secs`; a request of `0` asks for the server
  default (the configured max, or 3600 s when the server sets no max).
  A configured max of `0` means "no maximum" — the client's request is
  granted as-is. Peers re-announce before the granted TTL lapses; a
  lapsed advert is pruned on the next touch of its root.
- All of a session's adverts vanish the moment the session closes — the
  catalog never names a source that can't currently serve.
- Re-advertising a root the same session already holds refreshes its TTL
  and metadata without consuming another slot. Persona switches/renames
  update the catalog's names live.
- `swarm_adverts_max` caps an account's live adverts (across all its
  sessions); entries past the cap are refused, reported via `accepted`.
- Request bounds: at most **256 entries** per `AdvertiseFiles` (batch
  bigger sets), names ≤ 255 bytes, mime ≤ 127 bytes — oversize requests
  are refused with `TooLarge`.
- Nothing persists. After a restart the catalog is empty until peers
  re-announce — by design.

## Finding sources

`FindSources(root)` returns the live peers advertising that root (screen
name + metadata; wire endpoints arrive with the peer-wire slice) and
whether **this server's own blob store** holds the full file
(`server_has`/`server_size`) so a fetcher can always fall back to the
origin via the Wave 4.2 transfer engine. `sources.len()` doubles as the
root's rarity signal until per-chunk rarity arrives with the scheduler.

Cheshire mode is respected: sources whose session is invisible are
omitted for sub-moderator callers (naming an advert's holder would also
confirm they're online). Replies list at most 200 sources.

## Peer contact cards

A peer that wants to *serve* (not just be listed) registers a
`PeerContact`: the QUIC port its peer-wire listener is on plus its
self-signed cert fingerprint. The server pairs the port with the
connection's **observed** remote IP — a client cannot point fetchers at
an arbitrary host — and joins the card into that session's entries in
`SourceList` (`endpoint`/`cert_fp`, `None` for coordinator-only
sources). The card dies with the session, like the adverts themselves.

## Capability tokens

The origin server is the swarm's trust anchor. Before fetching from a
peer, a client asks it for a `SourceTicket`: a `rabbithole-swarm`
`CapToken` — `{root, fetcher screen name, expiry}` signed by the
server's ed25519 identity key with the domain-separated context
`rhp-swarm-cap-v1`. Serving peers verify the token against the server
key they learned at hello (no round trip), check the root matches what
is being requested, and refuse expired tokens. Tickets are short-lived
(10 minutes) — fetchers re-request rather than hoard. Issuance is gated
by `FILE_DOWNLOAD`, since a ticket authorizes moving file bytes.

A burrow a file is sent to (see "Pull streams between burrows" in
[`file.md`](file.md)) gets an `S2sCapToken` instead: `{root, the fetching
burrow's server key, expiry}` under its own context `rhp-swarm-cap-s2s-v1`,
so a person's token is never taken as a burrow's or the reverse. Peers
accept either (`token_allows`), each checked under its own context; peers
from before 0.228 refuse the burrow's, and the burrow fetches from the
source instead. As with a person's token, a peer cannot check who presents
it: the peer wire has no client authentication.

## The peer wire

A sharing peer runs a QUIC endpoint (same `rabbithole-net` stack,
self-signed cert pinned via the contact card's fingerprint). One
bi-stream per request:

1. Fetcher writes a framed `PeerRequest { token, root, offset, len }`
   (postcard, `len` ≤ 4 MiB) and closes its side.
2. Peer verifies the capability (signature against its own server's
   key, root match, expiry), then replies with a framed
   `PeerResponseHeader { status, size }` followed by the **Bao stream**
   for the requested ranges, rounded to 16 KiB chunk groups.
3. The fetcher decodes the stream against the root: every block is
   verified before a byte is accepted. A lying peer (wrong size, stale
   bytes, truncation) produces a verification error, never bad data.
   Seeds are validated at both ends — `SeedStore::add` refuses a file
   that doesn't hash to its declared root, and the peer's encoder
   re-validates against the outboard at serve time.

Whole files loop 4 MiB range requests (`fetch_file`). Bytes never
transit the origin server. `rabbit swarm share` seeds exactly this way;
`rabbit swarm fetch <root|link> <out>` does find → ticket → swarm fetch.

### Burrows as sources (0.230)

The burrow a file is stored on is one of a fetch's sources too, sending
each range with its proof like any peer: `ProvedRangeRequest` under a
download ticket (FILE 43, half a unit per message, see
[`file.md`](file.md)), its Bao outboard computed once and kept beside the
blob. The outboard is built in the background, once per file at a time (two
files at most at once), and written to disk as it is computed. An ask that
comes before it is ready is told to ask again (`Unavailable`), so nothing
waits on it. The desktop app adds the burrow beside the peers
whenever the person has not asked for peers only and the burrow reports
0.230 or later, as one more source taking units: whatever the peers do not
hold comes from it, unit by unit, instead of the download going all to the
peers or all to the burrow. The download ticket is opened only when the
burrow is first asked and closed as soon as the burrow is let go. One range
may take it at most a minute, and a burrow still making the proofs is asked
again for a minute plus the file at 64 MiB/s. Between burrows, the sending
burrow does the same on the pull link (`PullStreamAsk::Range`) when it
names seeders for the file, with four ranges in flight at once. In the
engine, every source (a peer, a burrow) only hands over Bao streams
(`RangeSource`), and may take several units at once (its lanes, never two
for one unit); the fetch checks each piece against the root itself
(`fetch_proved`), so no source is trusted with the checking.

### Other burrows as sources (0.233)

A download in the app can also take chunks from **the other burrows the
person is signed in to**. Before the fetch starts, each is asked whether it
holds the content and whether this person may download it there
(`FileByContentRequest`, FILE 45/46, see [`file.md`](file.md)); the ones
that say yes give a download ticket and join as sources, taking units like
any peer, every one proved against the root. A burrow that says nothing, says
no, or is too old to be asked is simply not a source, and costs the download
one bounded round trip.

Only when the person left the choice to the app ("Best available"), never
the burrow the download is from, never the same burrow reached two ways (they
are told apart by the burrow's own key, not by the address dialled), and at
most four of them. Each burrow session does one thing at a time, so a source
that is busy hands the unit back rather than holding it: what this buys is
another place to get the file, not more speed from one.

The command line does the same with the burrow it is signed in to:
`rabbit swarm fetch` asks which of its files holds the content and takes
units from it beside any peers, so a fetch with nobody seeding works rather
than sending the person to `rabbit file get`.

**What another burrow lends is never offered on.** A download that took any
chunk from another burrow is not advertised to the burrow it came from, in
part or whole (`RangeSource::shareable`): that content was lent to this
person, not given to this burrow to hand around.

### Partial seeds (0.229)

A peer need not hold a whole file to serve it. A fetch keeps the proof of
every unit it verifies: the Bao parent hashes the decoder checked, saved
into a pre-order outboard file beside the download (`<dest>.obao`, about
64 bytes per 16 KiB). The units that have landed, with their proofs, can
then be served on while the rest is still coming: a **partial seed**.
When the file is whole and hashed, it is seeded whole from the proofs
already kept, with no second pass over it.

- **Which units a peer holds.** A stream that opens with an **empty
  frame** asks a question instead of a range: the next frame is
  `PeerAsk::Have { token, root }` (the same capability a range request
  carries). The peer answers a framed `PeerResponseHeader { status, size }`
  and, on OK, a framed `HaveMap { unit, bits }`: one bit per unit (1 MiB),
  least significant bit first. A whole seed answers every bit set. A peer
  from before this reads the empty frame as a malformed request and closes
  the stream; the fetcher takes it to hold the whole file, which such peers
  always did.
- **A range not held** is answered with status 4, `NOT_HELD`, rather than
  a dropped stream: the fetcher asks someone else for that unit and keeps
  the peer for the rest. A peer also answers `NOT_HELD` for a range it
  cannot prove from what is on its disk (a changed file, a gap), and stops
  offering it.
- **Nothing unproven moves.** The serving peer encodes every range with
  `encode_ranges_validated` against its outboard, checked up to the root,
  so a block that does not match its proof is never sent; the fetcher
  decodes every block against the root before a byte lands, and hashes the
  whole file at the end. A bogus block costs the fetcher that one request.
- A partial seed is advertised with `AdvertisePartial` and found only
  with `FindAllSources` (above). The desktop app, when the person has
  opted in to seeding, advertises a download that way once its first
  verified unit lands, serves what it has from then on, and advertises it
  whole when it is done. Its own endpoint is never a source for its own
  download.
- A seed that can no longer serve what it claimed stops claiming it: a
  whole seed whose file is gone or changed is dropped (it then answers
  `NOT_FOUND`), and a partial seed stops offering units it cannot prove.

## Multi-source scheduling

`fetch_swarm` splits a file into 1 MiB work units and runs one worker
per source. Scheduling is work-stealing: a worker pulls the next unit
the moment it finishes, so faster peers carry more — that is the speed
assignment, with no rate estimator to go stale. A failing source hands
its unit back and retires; when the queue drains, idle workers enter
endgame and duplicate units still in flight (verified writes are
idempotent, first-done wins), so one stalled peer can't hold the tail.

Each worker first asks its source for its `HaveMap` and takes only units
the source holds. A source holding none of what is left (a partial seed
still fetching) is asked again every 3 seconds and let go after a minute
(by the clock) without anything new; a known partial seed that does not
answer a re-ask keeps its last map. One that answers `NOT_HELD` loses
that unit, not its place (after eight such answers against its own map it
is let go). A fetch can so be completed from peers that each hold only
parts of the file. A unit that cannot be written on this machine (a full
disk) fails the fetch with that error, not as if no source could serve,
and the desktop app then keeps the verified progress for a retry instead
of falling back to the burrow.
Rarest-first ordering applies across files (from coordinator source
counts) and arrives with manifest-set fetching.

Fetches are interruption-proof: `fetch_swarm_resumable` records each
completed unit in `<dest>.rhstate` (written atomically as units land),
resumes by skipping recorded units, hash-verifies the assembled file
whole against the root (a stale or lying partial fails closed), and
removes the state file on success. A state file for a different
root/size is ignored. `rabbit swarm fetch` resumes automatically.

## Transport decision (the spike)

The peer wire stays on **quinn + custom** coordination rather than
adopting iroh: the stack already runs quinn everywhere (server listener,
client transport, bulk streams), certificates are already pinned by
fingerprint, and the coordinator gives us discovery. Hole punching and
the server relay fallback (this wave, later slice) are tractable on raw
quinn; iroh remains the documented fallback if real-world NAT traversal
proves harder than expected.

## Permissions

The whole surface lives under the `swarm` resource: advertising needs
`SWARM_ADVERTISE` (User+ by default; guests don't have it), looking up
sources needs `FILE_LIST` (everyone by default). Operators can ACL
`swarm` like any other resource path.

## CLI

```
rabbit swarm share <files…> [--ttl SECS]   # hash + advertise, then serve until Ctrl-C
rabbit swarm find <root-hex | rabbit://…>  # who has it?
rabbit swarm unshare [roots…]              # withdraw (nothing = all)
```

Because adverts are session-scoped, `share` keeps its session open
(re-announcing at ~⅔ TTL) until interrupted; `--no-wait` advertises and
exits, which only makes sense against a separately held session. `find`
accepts a bare hex root or any root-pinned `rabbit://` link (`blob`,
`manifest`, or `files/…?root=`).
