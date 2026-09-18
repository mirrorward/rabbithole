# Server-to-server transfers

> Design for review. Nothing here is built. Request 7 of
> [`2026-09-requests.md`](2026-09-requests.md): choose files or folders on one
> burrow and send them to another burrow you are connected to, optionally with
> the swarm so it goes faster; permissions on both sides; clear errors; and
> swarm participants holding pieces may contribute them **without needing access
> to the destination server, and without exposing it**.

## What exists to build on

- **Federation** (`apps/server/src/federation.rs`, `crates/federation`): QUIC
  links between admin-approved peers (`PeerRegistry`, `approved_peers.json`),
  a signed handshake, signed **catalogs**, cross-burrow search, blake3 dedupe and
  `fanout::plan_fetch`, which orders sources and fetches nothing. **No file
  bytes move between burrows today.**
- **The swarm** (`crates/swarm`, `crates/server-core/src/swarm.rs`): content is
  its blake3 root; peers advertise roots to their origin; a fetcher asks the
  origin for a `CapToken{root, fetcher, expires}` that the origin signs, and a
  peer serves only a fetcher presenting one, verified against the origin's key.
  A token is honoured only inside the burrow that signed it.
- **Permission hooks**: at a source, `FILE_LIST` and `FILE_DOWNLOAD` per area and
  node; at a destination, `FILE_UPLOAD`, `upload_quota_bytes`, the deny-hash
  list, quarantine and dropbox rules (`apps/server/src/handlers8.rs`,
  `filelib.rs`). Errors are typed (`Forbidden`, `TooLarge`, `RateLimited`).
- **The desktop shell** holds an authenticated native session per burrow and can
  already fetch from peers or the origin (`swarm::run_download`) and upload with
  the resumable ticketed transfer (`Client::transfer_upload`).

## Two ways to do it

### A. Client relay. Buildable now, no protocol change

The app downloads from the source (swarm or origin, as any download) into a
temporary file, then uploads it to the destination with the normal ticketed
upload. Both burrows see an ordinary download and an ordinary upload by the
same person, so **every existing permission, quota and deny rule applies
unchanged**, and every existing error is already typed.

- Cost: the bytes cross the person's own link twice, and the app must stay open.
- Swarm: the download leg uses the swarm, so it is as fast as any swarm download.
  Participants serve the *person*, never the destination.
- Exposure: none new. Neither burrow learns about the other.

This satisfies "choose files or folders, send them to another burrow, optional
swarm, permissions on both sides, clear errors". It does **not** satisfy
"participants upload their portions to the destination directly".

### B. Destination-initiated pull. The real thing

The destination burrow fetches the file itself. The person only authorises it.

1. **Grant.** The app asks the *source* for a pull grant for nodes it may
   download: `{roots, sizes, names, fetcher = the destination burrow's identity
   key, expires (minutes), nonce}`, signed by the source. The source checks the
   person's `FILE_DOWNLOAD` exactly as for a download. The grant names the
   destination as the only party that can use it, so it is worthless if leaked.
2. **Request.** The app hands the grant to the *destination* with a target area
   and folder. The destination checks the person's `FILE_UPLOAD`, their quota
   against the granted sizes, and the deny-hash list against the granted roots,
   **before moving a byte**.
3. **Fetch.** The destination dials *out* to the source as a client, presents
   the grant, and receives the file by the ticketed transfer, plus, when the
   swarm is wanted, a `CapToken` naming the destination's key, with which it
   fetches pieces from the source's swarm peers like any other fetcher.
4. **Land.** Verified against the root, scanned against the deny list again,
   filed under the requesting person's account with its provenance recorded
   (source burrow and root). Progress and the outcome are pushed to the app.

How this meets the two hard requirements:

- **Participants need no access to the destination.** They serve a fetcher that
  presents a capability *their own origin* signed. That is already all they
  check. They never authenticate to, list, or upload to the destination.
- **The destination is not exposed.** It opens **no new listener** and accepts
  **no inbound connection** for this: every connection is one it dials. What a
  participant learns is the source address of an inbound QUIC connection, which
  is what any fetch reveals. An operator who will not accept even that sets
  `s2s_swarm = false`, and the destination pulls from the source only.

## Risks, and what answers each

| Risk | Answer |
|---|---|
| **SSRF.** A grant names an endpoint; a destination that dials whatever it is told can be aimed at its own network. | The destination dials only burrows in its admin-approved federation `PeerRegistry` by default. `s2s_pull_from_any` is an explicit opt-in, and even then loopback, link-local and private ranges are refused unless separately allowed. The endpoint comes from the registry entry for the grant's signing key, never from the grant's own text. |
| **Confused deputy.** The destination acts at the source on someone's behalf. | It acts only with a grant the source itself signed for that person's permitted nodes. The destination gains no standing at the source; the grant is single-use (nonce), short-lived, and names one fetcher. |
| **Storage and bandwidth abuse** at the destination. | The person's existing upload quota and rate limits apply, plus per-account caps on concurrent pulls and a per-pull size ceiling. Sizes are known from the grant, so refusal happens before the fetch. |
| **Unwanted content.** | Deny-hash check on the granted roots before fetching and on the verified file after; quarantine rules apply as for an upload. |
| **Grant replay or theft.** | Bound to the destination's key, expiring, single-use. Useless to anyone else, useless twice. |
| **Token scope creep.** Today a `CapToken` names a fetcher *screen name*. | A distinct token variant naming a *server key*, under its own signing context (`rhp-swarm-cap-s2s-v1`), so a person's token can never be replayed as a server's or the reverse. |
| **A slow or hostile source** holding a destination's connection open. | Pulls run on a bounded worker pool with idle and total timeouts; one bad source cannot starve the rest. |

## Wire and config, in outline

- FILE family, new messages (new types, not appended fields, per
  `docs/protocol/versioning.md`): `PullGrantRequest` / `PullGrant` at the
  source; `RemotePull` / `RemotePullAccepted`, and pushes `RemotePullProgress` /
  `RemotePullDone` / `RemotePullFailed` at the destination. Failure reasons are
  an enum the client turns into sentences: not permitted at the source, not
  permitted at the destination, over quota, denied content, source unreachable,
  source not an approved peer, grant expired, too large, cancelled.
- Config, all default off: `s2s_grants_enabled` (source), `s2s_pull_enabled`,
  `s2s_pull_from_any`, `s2s_swarm`, `s2s_max_bytes`, `s2s_max_concurrent`.
- Capabilities: reuse `FILE_DOWNLOAD` and `FILE_UPLOAD`. A separate
  `FILE_REMOTE_PULL` cap lets an operator allow uploads without allowing pulls.

## Proposal

Build **A** first. It delivers the feature people will see, touches no
protocol and no trust boundary, and its errors and permissions are the ones
already tested. Build **B** after this document has been reviewed, because it
is the first time a burrow acts on a person's behalf at another burrow.

## Questions for Kevin

1. Is approved-federation-peers-only the right default for pulls, with
   "from any burrow" as an explicit operator opt-in?
2. Should a pulled file be filed under the requesting person (proposed), or
   under a system "pulled from <burrow>" uploader with the person in the
   provenance?
3. Folders: recreate the tree under the chosen destination folder (proposed),
   and on a name clash number the newcomer, as downloads do?
4. For A, is a temporary file in the system temp folder acceptable, or should
   it stage inside the download folder?
