# RabbitHole Swarm File Distribution — Research & Design Brief

## Overview

RabbitHole is a Rust revival of the Hotline/Haxial/BBS/AOL client-server tradition, where a **central server anchors a community** (chat, boards, file libraries) and clients dial in. Classic Hotline had a plain client→server file transfer. The ask here is to bolt on a **torrent-like swarm** so that a file can be pulled in parallel from many sources — but adapted to the client/server topology rather than the trackerless anarchy of public BitTorrent.

The central insight that shapes the whole design: in RabbitHole the **server already exists and is trusted**, so it can play the roles that BitTorrent has to solve with decentralized machinery (tracker, DHT, coordination, and NAT relay) *for free*, while still allowing peers to serve chunks directly to each other. The most important novel requirement is **"list without upload"**: a client advertises files it holds on its own disk and serves them peer-to-peer on demand, without ever pushing bytes to the server. This is essentially a **web-seed / local-seed hybrid coordinated by a private tracker.**

This brief surveys how BitTorrent and its modern Rust descendants solve each sub-problem, then specifies a concrete protocol for RabbitHole.

---

## Key Features (of the surveyed prior art)

### BitTorrent piece/chunk model
- A torrent's content is concatenated into one logical byte stream, split into fixed-size **pieces** (commonly 256 KiB–16 MiB; auto-scaled to keep the metadata small — the classic rule of thumb keeps total piece count in the low thousands to tens of thousands).
- Pieces are the unit of **hash verification and advertisement** (the bitfield). But the unit of *transfer* is a smaller **block / sub-piece**, canonically **16 KiB**. Peers `request` blocks and receive them via `piece` messages; a piece is only verified once all its blocks arrive.
- **Rarest-first**: a downloader prioritizes the piece that the fewest connected peers have, which spreads distribution and prevents any piece from going extinct.
- **Endgame mode**: near completion, the last few outstanding blocks are requested from *all* peers simultaneously (with cancels on arrival) to avoid a long tail stalled behind one slow peer.

### Hash verification: BT v1 vs BT v2 vs BLAKE3
- **BT v1**: a flat list of **SHA-1** hashes, one per piece, stored in the `.torrent`. Simple, but SHA-1 is broken, the hash list bloats metadata for big torrents, and verification granularity equals piece size (a corrupt piece means re-downloading a whole large piece).
- **BT v2 (BEP 52)**: **SHA-256 Merkle trees, one per file**, with **16 KiB leaf blocks**; only the per-file Merkle **roots** go in the torrent. This keeps metadata tiny regardless of file size, enables **per-file** addressing (identical files dedupe across torrents), and lets a peer verify an individual block against the tree without the full hash list. Hybrid torrents carry both and join both swarms.
- **BLAKE3 / Bao (the modern move)**: BLAKE3 *is internally a Merkle tree* over **1024-byte chunks**. The **Bao** encoding format interleaves the tree nodes with the data so a receiver can **verify arbitrary byte ranges / random seeks against a single 32-byte root hash** while streaming — no separate hash list at all. This is what **iroh-blobs** uses (via `bao-tree`) for verified streaming. It's dramatically faster than SHA-256 and collapses "content ID," "piece hashes," and "verification structure" into one primitive.

### Magnet links & infohash
- A **magnet URI** encodes the **infohash** (`btih` = SHA-1 of the v1 info dict; `btmh` = multihash for v2) plus optional trackers and peer hints — enough to *find* the swarm and fetch the metadata from peers, so no `.torrent` file needs to be hosted. The infohash is the swarm's identity.

### DHT (Kademlia) peer discovery
- BitTorrent's **Mainline DHT** is a Kademlia overlay: 160-bit node IDs in the same space as infohashes; `get_peers`/`announce_peer` RPCs locate peers for an infohash by walking toward the closest nodes via XOR distance. It makes swarms **trackerless**. Powerful but heavy, slow to bootstrap, and abused for scraping — largely unnecessary when a trusted coordinator exists.

### Trackers (HTTP / UDP announce)
- A **tracker** is a lightweight rendezvous: clients `announce` (infohash, peer_id, ip:port, event, bytes left) and receive a **peer list**. **HTTP(S)** announce returns a bencoded/compact peer list; the **UDP tracker protocol (BEP 15)** is a cheaper connectionless variant (connect→announce→scrape) to save bandwidth. The tracker never touches file data — it only introduces peers. **This is exactly the role RabbitHole's server plays natively.**

### Peer wire protocol & choking
- After a handshake (infohash + peer_id), peers exchange a **bitfield** then message types: `choke/unchoke`, `interested/not_interested`, `have`, `request`, `piece`, `cancel`.
- **Choking** is BitTorrent's fairness/anti-leech mechanism: each peer unchokes a small set of best reciprocating peers (tit-for-tat), rotating every ~10 s, plus one **optimistic unchoke** (~every 30 s) to try a random peer and discover better partners / bootstrap newcomers who have nothing to trade yet.

### WebRTC / WebTorrent for browser peers
- Browsers **cannot open raw TCP/UDP sockets**, so WebTorrent runs the peer protocol over **WebRTC data channels**. Consequence (confirmed): a **browser WebTorrent peer can only talk to other WebRTC peers** — it cannot connect to normal TCP/UDP BitTorrent peers. Bridging requires a "hybrid" node (e.g. `webtorrent-hybrid`) or a desktop seed. WebRTC also needs **STUN/TURN signaling** to establish connections, and a signaling server to exchange SDP offers.

### NAT traversal
- **STUN**: peer discovers its public ip:port. **UDP hole punching**: two NATed peers, coordinated by a rendezvous server, send packets simultaneously so each NAT opens a mapping for the other. **TURN**: relay server that forwards traffic when hole punching fails (works but costs bandwidth). **UPnP-IGD / NAT-PMP / PCP**: ask the router to open a port mapping proactively. Modern QUIC stacks (iroh's "noq") express hole punching **as a QUIC-level operation** so the congestion controller stays aware of it, with **relay fallback**.

### Resumable / persistent transfers
- Clients persist the **bitfield** (which pieces are complete) and store **partial files** (either sparse full-size files or a `.part` scheme). On restart, they re-hash or trust the saved bitfield, reconnect/re-announce, and resume only the missing pieces. rqbit and libtorrent do exactly this; fast-resume data avoids a full re-verify.

---

## Technical Notes — RabbitHole Swarm Protocol Design (the concrete part)

Design stance: **BLAKE3/Bao content addressing + a QUIC peer transport + the RabbitHole server as private tracker and relay.** Use **iroh** as the connectivity+blobs substrate if you want to move fast; the design below is described protocol-first so it can also be hand-rolled on **quinn**.

### 1. Content addressing & manifest format

Two-level addressing, both BLAKE3:

- **File hash** = BLAKE3 root of the file's bytes (32 bytes). This *is* the content ID. Identical files anywhere in the network share it → automatic dedup and cross-source swarming.
- **Manifest hash** = BLAKE3 root of the serialized manifest. A "download" is identified by a manifest hash (the RabbitHole analogue of an infohash / magnet link).

**Manifest** (CBOR or Postcard; keep it canonical so its hash is stable):

```
Manifest {
  version: u16,
  name: String,                 // display name of the set (folder / album / release)
  total_size: u64,
  chunk_size: u32,              // see §2
  files: [ FileEntry {
      path: String,             // relative path within the set
      size: u64,
      blake3_root: [u8;32],     // per-file content ID
      mode: FileMode,           // perms/flags
  } ],
  // Optional: a manifest may reference sub-manifests for very large libraries.
}
```

Notes:
- **Per-file hashing (BT v2 style, but BLAKE3)** — not a single concatenated stream. This gives cross-set dedup and lets a peer advertise "I have file X" independently of which manifest asked for it.
- No hash list is stored in the manifest beyond the 32-byte roots. **Verification structure comes for free** from BLAKE3/Bao at transfer time (§3).
- A shareable **"rabbit link"**: `rabbit://<server-host>/<manifest-hash>?name=...` — enough to reach a coordinator and fetch the full manifest from it or from peers.

### 2. Chunk size

- **Verification leaf**: fixed by BLAKE3 = **1024 B** (free, incremental, don't touch it).
- **Transfer/advertisement chunk**: choose a **fixed 1 MiB chunk** for advertisement bitfields and request scheduling, subdivided into **64 KiB request blocks** on the wire (QUIC handles the small-block flow control well; 16 KiB is unnecessarily fine for QUIC). 1 MiB keeps bitfields small (a 10 GiB set = ~10 k chunk bits ≈ 1.25 KiB bitfield) while still allowing fine-grained parallelism across many sources.
- Because verification is Bao-based, **chunk size and verification granularity are decoupled** — a wrongly received 1 MiB chunk is detected and re-fetched without penalizing the rest of the file. This removes the classic BT tension where big pieces mean expensive re-downloads on hash failure.

### 3. Verification

- Every received byte range is verified **on the fly against the file's BLAKE3 root** using Bao verified streaming (`bao-tree` / iroh-blobs). A malicious or buggy peer cannot inject bad data undetected, and cannot even make you buffer a whole chunk before detection.
- No trust is placed in *who* served a chunk — content addressing makes **all sources interchangeable** (server, LAN peer, remote peer, federated server). This is what makes multi-source pulls safe.
- Manifest integrity: the manifest hash in the rabbit link authenticates the whole file set (each `blake3_root` inside it is thereby authenticated).

### 4. Peer / seed discovery via the server (the "tracker" role)

The RabbitHole server runs a **coordinator service**. Core state: a map `file_hash → set<PeerRecord>` and `manifest_hash → Manifest + set<PeerRecord>`.

Client → server control messages (over the existing authenticated RabbitHole session):
- `AdvertiseFiles([{ file_hash, size, path, permission_scope }])` — "list without upload." The client tells the server *what it holds*, **not the bytes**. The server stores only metadata + which peer holds it.
- `PublishManifest(Manifest)` — register a downloadable set; server stores the manifest (small) and can serve it to others.
- `FindSources(file_hash | manifest_hash) → [PeerRecord{ peer_id, node_addr, has_bitfield_hint, capabilities, is_server_cached }]`.
- `Announce(manifest_hash, event=started|stopped|completed, bitfield_summary)` — keeps liveness fresh, like a tracker announce; enables progress display and rarest-first stats.

The server may **optionally cache chunks itself** (`is_server_cached`) — acting as a permanent seed / web-seed for popular or at-risk files. This is a policy knob: a server can be pure-coordinator (holds nothing), a partial cache (LRU of hot chunks), or a full mirror.

**Rarest-first is easier here**: the coordinator aggregates bitfield summaries and can hand the downloader a *rarity-annotated source list*, so the client doesn't need DHT-scale gossip to know what's rare.

**No DHT needed for the core product** — the trusted server is the rendezvous. DHT/gossip (libp2p or iroh gossip) is reserved for the **federation** layer (§7) if you want serverless resilience later.

### 5. Peer transport & the swarm session

- **Transport: QUIC** (quinn, or iroh's endpoint). One QUIC connection per peer pair; each **chunk request is a QUIC stream**, so many chunks download concurrently over one connection with independent flow control and no head-of-line blocking. QUIC gives you TLS 1.3 encryption, multiplexing, and connection migration (survives IP changes) out of the box.
- **Peer wire messages** (mapped onto QUIC streams / a small framing):
  - `Hello{ peer_id, auth_token }` (auth token issued by the server — see §6)
  - `Have(file_hash, bitfield)` / `HaveDelta(chunk_idx)`
  - `RequestRange(file_hash, offset, len)` → response stream carries **Bao-encoded** bytes (data + needed tree nodes) so the receiver verifies inline.
  - `Cancel(request_id)` for endgame.
- **Scheduling**: multi-source rarest-first. The downloader maintains a global chunk map across *all* connected sources + server cache + federated servers, assigns each outstanding chunk to the currently-fastest source that has it, and enters **endgame** for the final N chunks (request from several sources, cancel losers).
- **Fairness/choking**: BitTorrent's tit-for-tat exists to punish leeches in an anonymous swarm. In RabbitHole peers are **authenticated members of a known community**, so replace tit-for-tat with **server-assignable upload slots / rate policy** (e.g. per-user concurrent-upload cap, priority for the server's own cache). Keep **optimistic-unchoke-style rotation** only as a load-spreading heuristic, not as an anti-cheat mechanism. This is a meaningful simplification the trusted-server topology buys you.

### 6. Permissions on advertised files

Because advertisement is just metadata, permissions are enforced by the **coordinator gating discovery** and by **peers gating serving**:

- Each `AdvertiseFiles` entry carries a **permission_scope**: `public` (any member), `group:<id>`, `users:[...]`, or `link-only` (must possess the manifest hash). Mirrors Hotline's per-folder access model.
- **Discovery gate**: `FindSources` only returns a peer for a file if the requester passes the file's scope. Unauthorized users never learn a holder exists.
- **Serving gate (defense in depth)**: the serving peer's `Hello.auth_token` is a **short-lived, server-signed capability** (e.g. an Ed25519-signed token: `{requester_id, file_hash, expiry}`). The holder verifies the signature offline before streaming bytes — so even if a peer address leaks, a peer won't serve without a valid token. This keeps enforcement working during brief server outages and prevents the server from being a per-chunk bottleneck.
- **Content-hash addressing caveat**: since the file *hash* is the ID, anyone who independently possesses the same file can serve it; permission scopes govern the *catalog/discovery*, not the mathematical fact of content identity. Sensitive material should rely on the capability tokens + encryption, not on hash secrecy.

### 7. Federation (multi-server pulls)

- Servers form a **federation**: a downloader's home server can proxy `FindSources` to peered servers, or hand the client **direct rabbit links to federated coordinators**. The client then treats a federated server (and its cached chunks + its members' advertised files) as **just more sources** in the same content-addressed swarm — safe because verification is per-content, not per-source.
- Trust: federated source lists are advisory; capability tokens for cross-server serving are issued by the *holder's* server, so each server remains authority over its own members' files.

### 8. NAT traversal

- **Default path — QUIC hole punching with relay fallback** (the iroh model): the RabbitHole server (plus optional dedicated relay nodes) act as the **rendezvous/signaling** point. Two NATed clients learn each other's candidate addresses via the server and perform **simultaneous QUIC hole punching**; because it's at the QUIC layer, the congestion controller and loss detection stay correct.
- **Fallbacks, in order**: (1) direct — one peer is publicly reachable or has a **UPnP/NAT-PMP/PCP** port mapping; (2) hole-punched QUIC; (3) **server/relay TURN-style forwarding** — the server relays the encrypted QUIC/Bao stream when punching fails. Since bytes are content-addressed and encrypted, relaying is safe and the relay can even opportunistically cache what flows through it.
- **Browser clients**: run the peer protocol over **WebRTC data channels** with the server as WebRTC **signaling** server; note the WebTorrent lesson — **browser peers can only reach WebRTC-capable peers**, so the RabbitHole server (or a hybrid gateway) must offer a WebRTC endpoint to bridge browser members into the native-QUIC swarm.
- Address privacy: peers can be configured to **never expose their IP to other members** — forcing all their transfers through the server/relay — for users who want the classic "only the server sees me" Hotline privacy model.

### 9. Persistence & resume

- **Per-file store**: sparse full-size file + a sidecar **`.rhstate`** holding: manifest hash, file hash, chunk_size, and a **persisted chunk bitfield** (which 1 MiB chunks are complete & verified). Optionally persist the partial **Bao outboard** so verification of already-downloaded ranges survives restart without re-hashing.
- **On reconnect**: load bitfield → re-`Announce(started, bitfield)` to the server → `FindSources` → resume only missing chunks. Completed chunks immediately become **advertisable** (the client becomes a partial seed mid-download — swarm effect).
- **Integrity on resume**: trust the bitfield for speed; do a lazy background BLAKE3 re-verify (cheap) and any chunk that fails is simply re-fetched, since granularity is decoupled from verification.
- **Graceful across coordinator restarts**: advertisement state is re-sent on session re-establish (the server treats advertisements as soft state with TTL, refreshed by `Announce`), so a server bounce doesn't lose the catalog permanently.

---

## Pitfalls & Lessons

- **Don't reinvent NAT traversal by hand.** Hole punching, ICE candidate gathering, and relay fallback are where P2P projects die. Lean on iroh/quinn; expressing punching at the QUIC layer (iroh) is materially more reliable than UDP-layer punching underneath a separate transport.
- **Browser peers are a walled garden.** Per WebTorrent, a browser peer reaches only WebRTC peers. Plan a **hybrid gateway** (server-side WebRTC endpoint) from day one, or browser members become second-class.
- **SHA-1 is dead; even fixed hash lists age badly.** Choosing **BLAKE3/Bao** avoids v1's weak hash *and* v2's separate-Merkle-tree bookkeeping — you get content ID, dedup, and verified streaming from one primitive. This is the single highest-leverage decision.
- **Decouple transfer chunk size from verification granularity.** BT v1's pain (big piece = expensive re-download on hash fail) disappears with Bao. Pick chunk size for scheduling/bitfield economics, not for verification.
- **Tit-for-tat choking is anti-leech machinery you probably don't need.** In an authenticated community, replace it with server-driven slot/rate policy; keep only the load-spreading rotation. Over-porting BitTorrent fairness adds complexity for a problem you don't have.
- **"List without upload" leaks availability.** Advertising metadata means unauthorized users could infer *who holds what* unless discovery is gated. Gate at the coordinator **and** enforce with signed capability tokens at the serving peer — don't rely on address secrecy.
- **Content hashing defeats access control by design.** Anyone with identical bytes has the same ID; scopes protect the catalog, not the content. Encrypt genuinely sensitive files.
- **Soft-state expiry.** Advertised-file records must have TTL and refresh, or the coordinator will hand out sources for peers that went offline hours ago (the classic dead-tracker-peer problem). Re-announce on reconnect.
- **Bitfield trust vs. re-verify.** Trusting a persisted bitfield speeds resume but risks serving corrupt data after disk issues; do lazy background verification.
- **Endgame or long tails will haunt you.** Without endgame mode, one slow final chunk stalls the whole transfer; implement it.
- **libp2p is heavy for a single-app protocol.** It's a great toolbox (Kademlia, gossipsub, transports) but sprawling; for RabbitHole's coordinator-anchored model, **iroh (or quinn hand-rolled)** is a tighter fit. Reserve libp2p/gossip for a future serverless-federation tier.

## Implications for RabbitHole

- **Adopt iroh as the connectivity + blob substrate** (endpoints = "dial keys not IPs," built-in QUIC hole punching + relay, `iroh-blobs`/`bao-tree` for verified streaming). It maps almost 1:1 onto this design and removes the two hardest pieces (NAT traversal, verified streaming). Fall back to **quinn** directly only if you need protocol control iroh doesn't expose.
- **Use BLAKE3 everywhere** for content IDs, manifest hashes, and Bao verified transfer — one hash primitive spanning addressing, dedup, and integrity.
- **The server stays central and trusted** — this is a *feature*, not a limitation to design around. It cleanly fills the tracker, signaling, relay, permission-authority, and optional-seed roles that decentralized BitTorrent must solve the hard way. Keep DHT/gossip out of v1.
- **Ship "list without upload" as the headline feature**: `AdvertiseFiles` (metadata only) + capability-token-gated peer serving + coordinator-gated discovery. This is the concrete thing that makes RabbitHole's swarm distinct from both classic Hotline and vanilla BitTorrent — a community file library where members' disks *are* the swarm and the server never has to store the bytes.
- **Persist aggressively** (`.rhstate` bitfield + optional Bao outboard) so transfers survive the reconnect-heavy reality of a BBS-style app, and so downloaders become partial seeds mid-transfer.
- **Federation via content-addressed source lists** lets multiple RabbitHole servers share swarms safely because verification is per-content, not per-source — a natural growth path into a "network of servers" reminiscent of the old tracker-server scenes, without giving up per-server authority.

### Rust crate shortlist
- **iroh** + **iroh-blobs** + **bao-tree** — connectivity (QUIC, hole punching, relay) and verified content-addressed blob transfer. Primary recommendation.
- **quinn** — the QUIC implementation under iroh; use directly for a bespoke wire protocol.
- **blake3** — hashing; `bao`/`bao-tree` for verified streaming encoding.
- **libp2p** — reserve for optional serverless federation (Kademlia DHT, gossipsub); overkill for the coordinator-anchored core.
- **rqbit / cratetorrent internals** — study for battle-tested **piece-manager, scheduler, endgame, and fast-resume** logic to port the *scheduling* ideas (even though RabbitHole's hashing/transport differ).

---

### Sources
- [iroh (n0-computer/iroh)](https://github.com/n0-computer/iroh) · [iroh 1.0 / QUIC + NAT traversal](https://stackradar.tech/posts/iroh-1-0-released-a-new-era-for-peer-to-peer-data-transfer-mqg226cf) · [iroh-blobs protocol docs](https://docs.rs/iroh-blobs/latest/iroh_blobs/protocol/index.html)
- [rqbit (ikatson/rqbit)](https://github.com/ikatson/rqbit) · [rqbit BEP 52 design issue #546](https://github.com/ikatson/rqbit/issues/546)
- [BitTorrent v2 (libtorrent blog)](https://blog.libtorrent.org/2020/09/bittorrent-v2/) · [BEP 52 spec](https://www.bittorrent.org/beps/bep_0052.html)
- [bao-tree (BLAKE3 verified streaming)](https://github.com/n0-computer/bao-tree) · [BLAKE3](https://github.com/BLAKE3-team/BLAKE3) · [Bao (oconnor663/bao)](https://github.com/oconnor663/bao)
- [WebTorrent](https://github.com/webtorrent/webtorrent) · [webtorrent-hybrid — browser vs TCP/UDP peers issue #12](https://github.com/webtorrent/webtorrent-hybrid/issues/12)

**Web tools were available and used** to verify current details on iroh, rqbit, BitTorrent v2/BEP 52, BLAKE3/Bao, and WebTorrent's browser constraint; the rest is synthesized from established P2P knowledge. No files were created or modified — this is a research/design deliverable returned inline.
