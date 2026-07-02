# RHP Swarm Family (6)

Status: **Wave 5 in progress** — the Warren's coordinator surface:
advertise (list-without-upload), find-sources, TTL soft state. The peer
wire (Have/RequestRange with Bao proofs), capability tokens, and the
multi-source scheduler land in later slices; manifests and `rabbit://`
links live in the `rabbithole-swarm` crate.

| type | name | direction | payload |
|---|---|---|---|
| 1/2 | AdvertiseFiles → AdvertiseAck | Request/Reply | entries {root, size, name, mime} + requested ttl; ack reports accepted / granted ttl / account total; needs SWARM_ADVERTISE |
| 3 | AdvertWithdraw | Request → ack | roots (empty = everything this session advertised) |
| 4/5 | FindSources → SourceList | Request/Reply | root → advertising peers + whether the origin's blob store has it; needs FILE_LIST |

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
