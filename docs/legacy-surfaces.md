# Legacy Surfaces — Operator Matrix

Every non-native listener a burrow can run. **All are off by default**
(`*_enabled = false`); the native QUIC/WS transports and rate limiting are
the only things on out of the box. Config keys are exact
(`crates/server-core/src/config.rs`); most are settable via
`ctl config set`, keys marked *TOML-only* are edited in `burrow.toml`.
Listener addresses require a restart; `*_min_role` gates apply live
(re-checked per login/connection).

Rate-limit classes are the Wave-13 token buckets (`ratelimit_*` knobs,
master switch `ratelimit_enabled`, on by default): **conn** = new
connections /IP/min, **auth** = *failed* logins /IP/min, **msg** = chat+DM
/account/s, **post** = board posts /account/min, **transfer** = transfer
opens /account/min, **legacy** = legacy-surface commands /IP/s. A knob of 0
disables that class.

| Surface | Default port | Enable key | Min-role gate | Rate classes | Deferred |
|---|---|---|---|---|---|
| Telnet BBS | 2323 (`telnet_addr`) | `telnet_enabled` | `telnet_min_role` (default guest) | conn, auth, legacy | inline byte transfers (ZMODEM codec exists, unwired — `files get` mints an HTTP handoff link instead) |
| — doors (on telnet) | — | `doors_enabled` (+ `telnet_enabled`) | cap `DOOR_RUN` on `doors/<id>` (member+ default) | (telnet's) | socket-handle inheritance (`%H` is always `0`; both io modes bridge stdio) |
| — files browser (on telnet) | — | (with telnet) | caps `FILE_LIST` / `FILE_DOWNLOAD` / `DROPBOX_VIEW`, same ACLs as native/Hotline | (telnet's) | byte transfers — `get` prints `<files_http_base>/files/<area>/<path>`; empty `files_http_base` (default) turns the handoff off. Serving the links is the web slice's job |
| Finger (RFC 1288) | 7979 (`finger_addr`) | `finger_enabled` | `finger_min_role` — finger is anonymous, so **any value above guest refuses every query** with a polite notice | conn | — (one capped query per connection by design) |
| NNTP reader (RFC 3977) | 1119 (`nntp_addr`) | `nntp_enabled` | `nntp_min_role` — anonymous reading counts as guest; above that, unauthenticated commands get 480 and a below-minimum `AUTHINFO` gets 481 | conn, legacy, auth (failed AUTHINFO), post (`POST`, per account) | article numbering shifts when retention drops posts (accepted for a read gateway) |
| NNTP peer feed (IHAVE + RFC 4644 CHECK/TAKETHIS, NEWNEWS) | 1120 (`nntp_feed_addr`) | `nntp_feed_enabled` | `nntp_feed_peers` allowlist (user → password, *TOML-only*); **empty = refuse every peer** (fail safe); every transit verb answers 480 until authenticated | conn, legacy, auth | — |
| Hotline (+HTXF) | 5500 (`hotline_addr`); HTXF bulk channel binds control port + 1 (5501) | `hotline_enabled` | `hotline_min_role` — Hotline guest sign-ins (empty credentials) count as guest and are refused above it | conn, auth (login), legacy (per transaction), post (news), transfer (downloads) | HTXF **upload**, fork-offset resume, folder downloads (tolerated with empty success replies); DisconnectUser bans are in-memory only; DeleteUser is a soft delete (disable); a few private-chat push edges (native topic set not echoed as 119) |
| FTN / binkp mailer | 24554 (`ftn_addr`) | `ftn_enabled` (tossing/scanning also needs non-empty `ftn_node`) | binkp session password `ftn_password` (`""`/`"-"` = unsecured); gateway posts/DMs only under a member-baseline subject holding the `board`/`dm` caps | conn | ARCmail bundle decompression (raw `.PKT` only; bundles left in spool), answering-side sending (outbound rides `poll_uplink` dials), crash-recovery resume / `M_GET` |
| QWK / QWKE | — | — | — | — | **codec + packet builder only** (`rabbithole-legacy-qwk`: MESSAGES.DAT, CONTROL.DAT, NDX, QWKE kludges, `.REP` ingest). No listener, no config keys, no ZIP bundling or board wiring yet |
| Radio delivery (Icecast/ICY) | 8000 (`radio_addr`) | `radio_enabled` | listeners (`GET`) are anonymous; a `SOURCE`/`PUT` DJ on this port authenticates HTTP Basic against a **real account** and needs cap `BROADCAST` on the `radio` resource | conn, auth (failed source logins) | live sources are fanned out **verbatim** (no decode/transcode into the audio `Station` playout) |
| Radio source ingest + updinfo | 8001 (`radio_source_addr`) | `radio_source_enabled` | shared credentials `radio_source_user` (default `"source"`) / `radio_source_password` — **empty password refuses every source and updinfo** (fail safe); guests never broadcast | conn, auth | same passthrough caveat as delivery |
| Syndication fetcher (RSS/Atom) | — (outbound only, no listener) | `syndication_enabled` (+ non-empty `syndication_feeds`) | gateway posts under a member-baseline subject holding `BOARD_POST` | — (its own politeness floor + per-feed backoff) | feed-declared TTLs (`<ttl>`/`sy:updatePeriod` not wired), IPv6 literal feed hosts, compressed responses (no `Accept-Encoding`) |

## Per-surface config keys

- **Telnet**: `telnet_enabled`, `telnet_addr`, `telnet_min_role`,
  `files_http_base` (empty default = no transfer handoff; live).
- **Doors**: `doors_enabled`, `doors_dir` (default `doors/`, relative to
  `data_dir`), `doors_max_nodes` (default 4; 0 refuses every launch),
  `doors_session_max_secs` (default 3600; 0 = unlimited; a door's own
  `daily_limit_mins` lowers it), `[[doors]]` array (*TOML-only*).
- **Finger**: `finger_enabled`, `finger_addr`, `finger_min_role`.
- **NNTP reader**: `nntp_enabled`, `nntp_addr`, `nntp_min_role`. Groups are
  postable boards (`kind == 2`) by identity slug mapping; Message-IDs are
  `<hex(event id)@origin>`.
- **NNTP feed**: `nntp_feed_enabled`, `nntp_feed_addr`, `nntp_feed_peers`
  (*TOML-only*). Accepted articles post as `{name}@usenet` gateway authors;
  dedupe via the shared `SeenKey::MessageId` store.
- **Hotline**: `hotline_enabled`, `hotline_addr`, `hotline_min_role`.
- **FTN**: `ftn_enabled`, `ftn_addr`, `ftn_node`, `ftn_uplink`,
  `ftn_uplink_host`, `ftn_password`, `ftn_inbound_dir` (default
  `ftn/inbound`), `ftn_outbound_dir` (default `ftn/outbound`), `ftn_areas`
  (AREA tag → board slug, *TOML-only*). Loop-broken by author origin: only
  `@{origin}`-authored posts are scanned outbound.
- **Radio**: `radio_enabled`, `radio_addr`, `radio_source_enabled`,
  `radio_source_addr`, `radio_source_user`, `radio_source_password`,
  `radio_library_areas` (mount slug → file-area slug, *TOML-only*) — each
  entry runs a playlist-automation station from that area's audio files; a
  live DJ source takes the mount over and rotation resumes when it leaves.
  The updinfo endpoints (`GET /admin/metadata`, `GET /admin.cgi`) ride the
  source-ingest listener and check the source credentials.
- **Syndication**: `syndication_enabled`, `syndication_feeds` (URL → board
  slug, *TOML-only*), `syndication_poll_secs` (default 1800). Dedupe is
  durable per feed (`<data_dir>/syndication/`) *and* burrow-wide
  (`SeenKey::Syndication`).

Not legacy, but adjacent: the S2S federation listener (`federation_enabled`,
`federation_addr`, default port 4655, off by default) is documented in
[`protocol/federation.md`](protocol/federation.md).
