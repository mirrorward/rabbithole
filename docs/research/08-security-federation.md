# RabbitHole — Security, Identity & Federation Design Brief

## Overview

RabbitHole is a modern, distributed community server drawing lineage from Hotline-style servers (message bases, file libraries, chat, per-folder permissions, a global tracker) but rebuilt on a Rust stack with contemporary cryptography and a real federation story. The design goal is a **cluster of independently operated servers** that can share message bases, offer cross-server file search, and present portable user identities — without recreating the well-known failure modes of federated systems (spam, state divergence, catastrophic key loss, metadata leakage).

The recommendation below is deliberately **layered and incremental**: strong local security first (transport, auth, authz), then opt-in E2EE for DMs, then a Usenet/FidoNet-inspired flood-federation model for message bases (chosen over full Matrix-style state resolution because RabbitHole's data model is append-heavy and doesn't need per-message shared mutable state), plus a Matrix-style `.well-known` + tracker/directory hybrid for discovery.

A note on tooling: web search was available and used to verify the parameter-sensitive and fast-moving areas (Argon2id parameters, Matrix state resolution, QUIC/Noise tradeoffs, Signal sealed sender, Usenet flooding, Rust crypto crate status). Sources are listed at the end.

---

## Key Features (recommended target)

- **Transport:** QUIC (quinn + rustls, TLS 1.3) as the primary client and server-to-server transport; plain TLS 1.3 (rustls) fallback for restrictive networks.
- **Authentication:** Argon2id password hashing (tuned params), optional TOTP 2FA, optional per-user Ed25519 identity key (SSH-like challenge/response), opaque server-side session tokens with rotation.
- **Authorization:** Hybrid model — coarse roles (guest → user → moderator → admin → superuser) mapped to a **64-bit permission bitmask** (Hotline heritage), overlaid with **per-folder/per-file ACLs** resolved by nearest-ancestor inheritance.
- **DM privacy:** Opt-in end-to-end encryption using X25519 + Double-Ratchet, with sealed-sender-style envelope encryption to hide sender metadata from the home server.
- **Federation:** Flood-fill message propagation with content-addressed message IDs for deduplication (Usenet/FidoNet model), Ed25519-signed events, per-peer trust and subscription lists.
- **Discovery:** `.well-known/rabbithole/server` for capability/routing metadata + a modern global directory (trackers) publishing health/uptime/metadata, with signed server descriptors.
- **Abuse prevention:** Token-bucket rate limiting, IP/subnet bans, tiered guest restrictions, registration gating (invite/CAPTCHA/email), federation-level allow/deny and reputation.

---

## Technical Notes (concrete algorithms & crates)

### 1. Transport security

**Recommendation: QUIC via `quinn` + `rustls` as primary; `rustls` TLS 1.3 over TCP as fallback. Do not adopt Noise for the wire transport.**

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| **TLS 1.3 (rustls) over TCP** | Ubiquitous, firewall-friendly, audited, trivial cert story (Let's Encrypt/ACME), well-understood | Head-of-line blocking, 1–2 RTT setup, no built-in stream multiplexing | Keep as fallback |
| **QUIC (quinn + rustls)** | Encryption built in (TLS 1.3 handshake), 0–1 RTT resumption, native stream multiplexing (great for concurrent file transfer + chat + control on one connection), no HOL blocking, connection migration | Higher CPU (userspace crypto), UDP sometimes blocked/throttled, more code surface | **Primary** |
| **Noise Protocol Framework** (snow crate / nQUIC) | Minimal config, no X.509/PKI baggage, small and auditable, great for pinned peer keys | No PKI/CA ecosystem → you must build your own trust distribution; no browser/HTTP compat; smaller battle-tested surface | Only for a niche server↔server pinned-key mode; not the default |

Concrete rationale: QUIC's multiplexing is a natural fit for RabbitHole because a single client session juggles chat, file listing, file transfer, and message-base sync simultaneously — TCP+TLS would need multiple connections or suffer HOL blocking. QUIC's encryption *is* TLS 1.3 (rustls provides the handshake in quinn), so you get the audited crypto and the ACME certificate ecosystem for free. Noise is elegant but its lack of a PKI ecosystem means you'd reinvent certificate/trust distribution — reserve it only if you later want a pure pinned-key server mesh.

**Crates:** `quinn`, `rustls` (with `aws-lc-rs` or `ring` backend), `rustls-acme` / `instant-acme` for automatic certs, `webpki`/`rustls-pki-types` for validation.

### 2. Authentication

**Password hashing — Argon2id.** Use the `argon2` crate (RustCrypto). Recommended tuned parameters:

- **Default profile:** `m = 64 MiB (65536 KiB)`, `t = 3`, `p = 1` — the OWASP 2024 "standard" profile, ~250–400 ms on modern hardware.
- **Minimum acceptable floor:** `m = 19 MiB`, `t = 2`, `p = 1` (OWASP minimum) for constrained/embedded server deployments.
- **High-security profile:** `m = 128 MiB`, `t = 4`, `p = 1`.
- Store parameters *inside* the PHC-format hash string (`$argon2id$v=19$m=...`) so you can raise them later and rehash-on-login transparently.
- 16-byte random salt (crate default), 32-byte output. Never log or reflect the hash.

**TOTP 2FA (optional):** RFC 6238, `totp-rs` crate, SHA-1 or SHA-256, 30 s step, 6 digits, ±1 window skew. Store the shared secret encrypted at rest (see key management). Provide one-time recovery codes (hashed with Argon2id too).

**Per-user public-key identity (optional, SSH-like):** Each user may register one or more **Ed25519** public keys (`ed25519-dalek` — pure Rust, actively maintained, the de-facto standard). Login = server sends a random challenge, client signs it, server verifies. This is the basis for both passwordless login and portable federated identity (see §Federation). Support agent-style delegation later if desired.

**Session tokens:** Prefer **opaque, high-entropy random tokens** (32 bytes from a CSPRNG, `rand`/`getrandom`) stored server-side (Redis/SQLite) over self-contained JWTs — opaque tokens are instantly revocable and leak nothing. Bind tokens to a device/connection fingerprint, set idle + absolute expiry, rotate on privilege change, and issue a separate short-lived token for federation calls. If you must go stateless, use `PASETO` v4 (`rusty_paseto`) over hand-rolled JWT to avoid the alg-confusion footguns.

### 3. Authorization

**Recommendation: three-layer evaluation — Role → Permission bitmask → ACL — resolved cheaply in that order.**

1. **Roles** (`guest`, `user`, `moderator`, `admin`, `superuser`): a simple ordered enum. Each role maps to a *default* permission bitmask. Roles are the coarse default; ACLs are the fine override.

2. **Permission bitmask (Hotline heritage):** a `u64` (64 named capability bits: `READ_MSGS`, `POST_MSGS`, `DOWNLOAD_FILE`, `UPLOAD_FILE`, `DELETE_FILE`, `CREATE_FOLDER`, `RENAME`, `MOVE`, `KICK_USER`, `BAN_USER`, `EDIT_OTHERS`, `BROADCAST`, `MANAGE_USERS`, `FEDERATE`, …). Permission checks are single bitwise-AND operations — effectively free. Store as an integer column; expose in the API as named flags. Reserve upper bits for future/federation capabilities. (If you expect to exceed 64 someday, use a `bitflags`-backed newtype so you can widen to `u128` without churn.)

3. **Per-folder/per-file ACLs:** For resources needing overrides, store ACL entries as `(resource_id, principal, allow_mask, deny_mask)` where principal is a user, a role, or a group. **Evaluate with nearest-ancestor inheritance:** walk from the resource up its folder path, and the first level that specifies a rule for the principal wins (deny bits take precedence over allow bits at the same level). Cache the *effective mask* per (principal, folder) with invalidation on ACL edits so hot-path checks stay O(1).

**Effective permission algorithm (per request):**
```
effective = role_default_mask
effective |= user_grant_mask          // explicit per-user grants
effective &= ~user_revoke_mask
if resource has ACL chain:
    (allow, deny) = nearest_ancestor_rule(resource, principal_set)
    effective |= allow
    effective &= ~deny
return (effective & required_bit) != 0
```
Deny-wins, closest-node-wins, cached. This gives Hotline-like simplicity for the common case and Unix/NTFS-like flexibility where needed.

**Crates:** `bitflags`, plus `sqlx`/`sea-orm` for ACL storage; keep the evaluator hand-written (don't pull a heavyweight policy engine like OPA unless requirements grow).

### 4. End-to-end encryption for DMs

**Recommendation: implement opt-in E2EE for direct messages; it is worth it, but scope it tightly and do NOT try to E2EE public message bases.**

- **Key agreement:** X25519 (`x25519-dalek`) for ECDH; long-term identity key = the user's Ed25519 key (convert to X25519 for DH, or keep a separate X25519 identity subkey + signed prekeys, Signal-style X3DH).
- **Ratcheting:** Double Ratchet (`vodozemac` from the Matrix project, or the reference `double-ratchet` approach) for forward secrecy + post-compromise security. Each message gets a fresh symmetric key (AEAD: ChaCha20-Poly1305 or AES-256-GCM via RustCrypto `aead` traits).
- **Sealed sender (metadata protection):** encrypt the sender certificate + ciphertext in an outer envelope keyed to the recipient's identity, so the relaying/home server sees recipient-only routing info and not who sent it. This meaningfully reduces the metadata the server (and any subpoena of it) can reveal.

**Why worth it, with caveats:** DMs are exactly the surface where users expect confidentiality even from the operator, and a small, self-hosted community server is a plausible compromise/subpoena target — E2EE removes the operator from the trust boundary for private messages. **But:** it complicates multi-device sync, message search (server can't index ciphertext), moderation of reported DMs, and history for new devices. So: make it opt-in per-conversation, keep **public** message bases and file libraries as *server-side encrypted-at-rest but operator-readable* (E2EE there breaks search/moderation for no real threat-model gain), and clearly document that E2EE DMs won't be server-searchable.

### 5. Server-to-server federation

**Comparison of prior art:**

| System | Trust | Routing | Dedup | Identity | Notes |
|---|---|---|---|---|---|
| **Usenet (NNTP)** | Peer agreements, no global auth | **Flooding** — push to peers unless already seen | **Message-ID** (globally unique); seen-set discards dupes | Weak (headers forgeable) | Massively scalable, append-only, no shared mutable state — closest to RabbitHole's model |
| **FidoNet** | Nodelist + sysop agreements, zone/net/node addressing | Store-and-forward along a routing hierarchy | Per-message, hierarchical | Node addresses in a signed nodelist | Directory-driven routing, human-vetted trust |
| **ActivityPub** | Per-actor HTTP signatures; instance-level allow/block | Push (inbox) + follow graph; fan-out | Activity `id` URIs | Actor = URL; portability weak | Simple, HTTP-native; spam/moderation are pain points |
| **Mastodon (AP profile)** | Instance blocklists, HTTP sig | Fan-out to followers' instances | Object `id` | `@user@host` | Practical AP deployment; relays add flood-like distribution |
| **Matrix (S2S API)** | **No single trust root** — each server picks notary servers; homeservers publish signing keys at `/_matrix/key/v2/server`; all events Ed25519-signed | Per-room event **DAG** replicated to all participating servers | Event IDs = content hashes; DAG parent refs | `@user:server`, server signing keys | **State Resolution v2** merges forks after netsplits; must accept old-DAG events (can't distinguish delayed from malicious ban-evasion), then let state-res settle — powerful but heavy |

**Recommendation for RabbitHole: a Usenet/FidoNet-style signed flood-fill for message bases, an ActivityPub-lite pull model for file search, and Matrix-style key publication for identity/trust. Explicitly avoid Matrix's per-room state DAG + state resolution.**

Reasoning: RabbitHole message bases are fundamentally **append-only threaded posts** — there is no per-message *shared mutable state* to reconcile, so Matrix's state-resolution complexity (its hardest, most bug-prone component) buys nothing. Usenet's flooding proved that append-only content federates at global scale with a trivial dedup rule.

**Concrete federation model:**

1. **Message-base sync (flood-fill):**
   - Every post is an **event**: `{content, thread_id, base_id, author_id, origin_server, created_at}`, assigned a **content-addressed ID** = `blake3(canonical_serialization)` (use `blake3` crate). Content addressing gives free dedup and tamper-evidence.
   - Every event is **Ed25519-signed twice**: by the author's per-user key (portable identity) and by the origin server's signing key (routing accountability). Verify both on ingest.
   - **Propagation:** each server subscribes to specific message bases from specific peers. On new event, push (`ihave`-style offer) to subscribed peers; peer requests only unseen IDs. Maintain a **seen-set** (Bloom filter + backing store) keyed on content ID → O(1) dedup, loops impossible.
   - **Ordering:** events carry parent references (thread reply-to) forming a per-thread DAG; display order is `(created_at, content_id)` tiebreak. No global consensus needed — eventual consistency, like Usenet.
   - **Moderation/retraction:** signed `tombstone` and `redact` events flood the same way; each server applies them locally per its own policy (a server may honor or ignore a remote redaction — servers are sovereign).

2. **Cross-server file search:** Don't flood file *contents*. Use a **pull/query fan-out** (ActivityPub-lite): a client's home server queries peer servers' signed, cached **file catalogs** (periodically published manifests: `{path, size, blake3, tags, server_id, permissions_summary}`). Home server aggregates, dedups by `blake3`, and presents results honoring each origin's permission summary. Transfers happen directly against the origin server (with that server's own authz applying). Optionally publish catalog digests to the directory (§6) for global search.

3. **Cross-server identity:** A user is `user@home.server`. Identity is **anchored by the user's Ed25519 public key**, published and cross-signed by the home server (Matrix-style: fetch the home server's signing keys from its `.well-known`/key endpoint, then verify the user-key attestation). This makes identity **portable and verifiable** independent of any central authority; a user could even migrate home servers by re-attesting the same identity key. Servers maintain per-peer trust lists (allow/deny + reputation), and can require key-continuity for migrated identities.

### 6. Server discovery / directory

**Recommendation: combine three layers.**

1. **`.well-known` self-description (Matrix-inspired):** each server serves `https://host/.well-known/rabbithole/server` → signed JSON with federation endpoint, QUIC/TLS ports, protocol versions, signing-key fingerprints, and capability flags. This is the authoritative, decentralized bootstrap — no directory required to connect if you know the hostname.

2. **Trackers (Hotline heritage, modernized):** lightweight tracker services where servers *register* and clients *browse*. Modernize with: signed server descriptors (Ed25519), periodic health/uptime heartbeats, and rich metadata (name, description, topic tags, user count, guest policy, region, federation openness). Multiple independent trackers can coexist; clients can subscribe to several. Trackers gossip registrations to each other (flood-fill again) for resilience.

3. **Global directory service:** an aggregating index (fed by tracker gossip + `.well-known` crawling) offering search over servers *and* — optionally — cross-server file catalog digests and public message-base topics. It publishes uptime/health metrics and flags stale/dead servers. Treat it as an *index, not an authority*: everything it returns is server-signed and independently verifiable, so a malicious directory can withhold or reorder but cannot forge.

### 7. Abuse prevention

- **Rate limiting:** token-bucket per (IP, user, endpoint-class). Separate stricter buckets for expensive ops (registration, file upload, federation ingest, search). Crate: `governor`. Apply at both connection and application layers; return `429`-equivalent with retry hints.
- **Bans / IP blocks:** ban by user ID, IP, and CIDR subnet; support temporary + permanent; store with reason + expiry + issuer for auditability. Federation-level bans (block a whole peer server) and shared/subscribable blocklists (Mastodon-style) between friendly servers.
- **Guest restrictions:** guests get a minimal permission bitmask (read public bases, browse limited file areas, maybe throttled chat); no upload, no DM to non-consenting users, no federation-visible actions. Escalate to `user` only after registration/verification.
- **Registration gating:** configurable per server — open, invite-only (signed invite tokens), email verification, or CAPTCHA. For CAPTCHA prefer privacy-respecting options (self-hosted `hCaptcha`-style or proof-of-work challenges like `mCaptcha`) over Google reCAPTCHA.
- **Federation abuse:** verify both signatures on every ingested event; enforce per-peer ingest rate limits; drop events whose author key isn't attested by a trusted origin; maintain per-peer spam reputation and auto-defederate on threshold. Content-addressed IDs prevent replay/dup floods inherently.
- **Content moderation tools:** report queues, per-base moderator assignment, soft-delete/tombstone, quarantine (hold-for-review) for new/low-reputation users, keyword/regex and hash-based (known-bad-file `blake3`) filtering, and audit logs of all mod actions. For file libraries, support hash-deny lists that can be shared between servers.

### 8. Privacy & data handling

- **Data minimization:** store only what's needed; make IP logging retention configurable and short by default; separate operational logs from content.
- **Encryption at rest:** encrypt sensitive columns (TOTP secrets, recovery codes, private keys if server-held) with a KMS-derived or passphrase-derived key; never store private *user* identity keys server-side in E2EE mode (client-held).
- **Metadata:** sealed-sender DMs (§4) limit who-talks-to-whom exposure; for federation, avoid leaking full user activity to peers — send only what a subscribed base requires.
- **Transparency & control:** user data export/portability (aligns with identity migration), account deletion with federated tombstone propagation (accept that remote copies are best-effort, like all federated systems), and a clear published data-handling policy per server (surfaced in the directory metadata).
- **Federated deletion caveat:** be explicit with users that federation means copies exist on other servers; deletion is a *request* honored by cooperating servers, not a guarantee.

---

## Pitfalls & Lessons (from prior art)

- **Matrix state resolution is genuinely hard.** Netsplits cause state forks; SRv2 exists precisely to merge them and has been a persistent source of subtle bugs. Ban-evasion is fundamentally awkward: you *cannot* reject events referencing old DAG points (indistinguishable from legitimately delayed events), so they must enter state-res anyway. **Lesson:** don't inherit shared-mutable-state semantics you don't need — append-only bases sidestep the entire problem.
- **Usenet spam.** Flooding with weak identity made Usenet a spam magnet. **Lesson:** RabbitHole must mandate dual Ed25519 signatures + per-peer reputation + registration gating from day one — flooding is fine, *unauthenticated* flooding is not.
- **ActivityPub moderation debt.** AP's optimistic push federation left moderation as an afterthought; instances rely on manually curated blocklists. **Lesson:** build moderation, reputation, and defederation into the protocol, not bolted on.
- **JWT footguns.** Alg-confusion, no revocation, oversized tokens. **Lesson:** opaque server-side tokens (or PASETO) over hand-rolled JWT.
- **Argon2 misconfiguration.** Too-low memory (< 19 MiB) is common and defeats the point. **Lesson:** ship the OWASP 64 MiB/t=3 default, store params in the hash, and rehash-on-login when you raise them.
- **E2EE overreach.** Encrypting everything breaks search, moderation, and multi-device — Matrix spent years on E2EE UX pain. **Lesson:** E2EE only DMs; keep public content operator-readable.
- **QUIC/UDP reachability.** Some networks block/throttle UDP. **Lesson:** always ship the rustls-TCP fallback.
- **Key loss = identity loss.** Ed25519 user identity is powerful but unrecoverable if lost. **Lesson:** support multiple registered keys + recovery codes + optional server-assisted recovery for non-E2EE identity.
- **Crypto crate hygiene.** Use maintained, audited crates (`rustls`, `ed25519-dalek`, `x25519-dalek`, RustCrypto `argon2`/`aead`, `blake3`, `quinn`); avoid hand-rolling primitives; pin versions and watch RUSTSEC advisories.

---

## Implications for RabbitHole — Recommended Model (concrete)

**Transport:** QUIC (`quinn` + `rustls`/TLS 1.3, `aws-lc-rs` backend) as primary client and S2S transport, multiplexing chat/files/control/sync; rustls-over-TCP fallback; ACME certs via `rustls-acme`. Reserve Noise (`snow`) only for an optional pinned-key server-mesh mode.

**Identity & auth:** Every account has an **Ed25519 identity key** (`ed25519-dalek`) as the portable, federated root of identity, cross-signed by its home server. Local login via Argon2id password (`m=64MiB, t=3, p=1`, PHC-stored, rehash-on-login) or key challenge/response, with optional TOTP 2FA. **Opaque, revocable, rotating session tokens**, server-stored.

**Authorization:** `role → u64 bitmask → nearest-ancestor ACL (deny-wins)`, with cached effective masks — near-free hot-path checks, Hotline familiarity, NTFS-grade flexibility where needed.

**DMs:** Opt-in E2EE (X25519 + Double Ratchet via `vodozemac`, ChaCha20-Poly1305, sealed-sender envelopes). Public bases/files: encrypted at rest, operator-readable, fully searchable/moderatable.

**Federation:** **Signed flood-fill** for message bases — content-addressed (`blake3`) events, dual Ed25519 signatures (author + origin server), Bloom-filter seen-set dedup, per-thread DAG for display ordering, tombstone/redact events, per-peer subscriptions and trust/reputation. **Pull-query fan-out** for cross-server file search over signed, cached catalogs (dedup by `blake3`, transfers direct-to-origin under origin authz). **Portable Ed25519 identity** verified via home-server signing keys, enabling account migration. Explicitly **no Matrix-style shared-state resolution** — the append-only model doesn't need it.

**Discovery:** `.well-known/rabbithole/server` (signed, authoritative bootstrap) + modernized **trackers** (signed descriptors, health/uptime heartbeats, gossip between trackers) + an aggregating **global directory** (index-not-authority; everything server-signed and verifiable; optional global file/topic search).

**Abuse & privacy:** `governor` token-bucket rate limits (stricter for register/upload/federation), user/IP/CIDR + peer-server bans, subscribable shared blocklists, tiered guest permissions, configurable registration gating (invite/email/`mCaptcha`), dual-signature + reputation + auto-defederation on federation ingest, moderation queues with tombstones and hash-deny lists, data minimization, at-rest encryption of secrets, sealed-sender metadata protection, and portable export/deletion with best-effort federated tombstone propagation.

This gives RabbitHole modern, audited local security; opt-in privacy where it matters; and a federation model matched to its actual data shape (append-only bases + cataloged files + portable keys) rather than the heaviest available option.

---

## Sources

- [OWASP Password Storage Cheat Sheet (Argon2id parameters)](https://cheatsheetseries.owasp.org/cheatsheets/Password_Storage_Cheat_Sheet.html)
- [Argon2 — Wikipedia](https://en.wikipedia.org/wiki/Argon2)
- [Matrix Server-Server API spec (v1.16) — signing keys, notary trust, event DAG](https://spec.matrix.org/v1.16/server-server-api/)
- [Matrix State Resolution v2 explainer](https://matrix.org/docs/older/stateres-v2/)
- [quinn — QUIC in Rust (docs.rs)](https://docs.rs/quinn/latest/quinn/) and [quinn-rs/quinn (GitHub)](https://github.com/quinn-rs/quinn)
- [nQUIC: Noise-Based QUIC Packet Protection (paper)](https://eprint.iacr.org/2019/028.pdf)
- [Signal — Sealed Sender](https://signal.org/blog/sealed-sender/) and [Signal Protocol — Wikipedia](https://en.wikipedia.org/wiki/Signal_Protocol)
- [How Usenet Handles News (flooding, message-IDs)](https://tldp.org/LDP/nag/node258.html) and [Usenet — Wikipedia](https://en.wikipedia.org/wiki/Usenet)
- [ed25519-dalek (docs.rs)](https://docs.rs/ed25519-dalek/) and [Awesome Rust Cryptography](https://cryptography.rs/)
