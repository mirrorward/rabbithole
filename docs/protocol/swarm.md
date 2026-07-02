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

## Multi-source scheduling

`fetch_swarm` splits a file into 1 MiB work units and runs one worker
per source. Scheduling is work-stealing: a worker pulls the next unit
the moment it finishes, so faster peers carry more — that is the speed
assignment, with no rate estimator to go stale. A failing source hands
its unit back and retires; when the queue drains, idle workers enter
endgame and duplicate units still in flight (verified writes are
idempotent, first-done wins), so one stalled peer can't hold the tail.
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
