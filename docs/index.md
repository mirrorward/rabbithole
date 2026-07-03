# RabbitHole

> Down the rabbit hole: a modern Rust revival of the golden age of online
> communities — Hotline, KDX, BBSes, and AOL — one server, many doors.

A **Burrow** (the server, `burrow`) hosts chat rooms, message boards, file
libraries, direct messages, a swarm file-distribution layer (**the Warren**),
pirate radio, and a request board (**the Wishing Well**) — reachable from
native apps, terminals, browsers, telnet BBS clients, newsreaders, offline
mail readers, and real classic Hotline clients. Burrows federate over
**Tunnels** and are discoverable through **Looking Glass** directories.

This site is built with [mdBook](https://rust-lang.github.io/mdBook/) over the
docs in the repository. Build it locally with `mdbook build docs` (or
`mdbook serve docs` for a live preview); the rendered output lands in
`docs/book/`.

## What this documentation covers

- **[The RabbitHole Protocol (RHP)](protocol/README.md)** — the native wire
  protocol: framing, the family/message-type registry, and one page per
  message family (session, presence, chat, DMs, boards, files, swarm, admin,
  wishing well, federation), plus the [versioning &
  compatibility](protocol/versioning.md) policy that the proto registry test
  mechanically enforces.
- **Operating a server** — [deployment](deployment.md), the [legacy-surfaces
  matrix](legacy-surfaces.md) (every listener, its port, enable key, min-role,
  and rate-limit classes), and a [constrained/RF-link budget
  guide](deployment-lora.md) for running over Reticulum on LoRa-class radios.
- **Design & decisions** — the research briefs behind the design and the
  architecture decision records (e.g. the [Reticulum
  path](research/reticulum-decision.md)).

## Status (honest)

RabbitHole is pre-1.0 and moves fast; the wire may change between minor
versions (the proto registry test guards against *accidental* breakage — see
[versioning](protocol/versioning.md)). Waves 0–14 are substantially landed:
the native RHP server and its surfaces (accounts/personas/TOTP, chat/rooms/DMs
with mute + slow-mode, boards + offline sync, file libraries + Bao-verified
transfers + the swarm, moderation, rate limiting, backups), the SPA web client
(an installable PWA served by `burrow --http`), TUI clients, and the full
legacy matrix (telnet BBS with doors/ZMODEM/QWK, finger, NNTP reader +
peer-feed + NNTPS/STARTTLS, Hotline with private rooms + HTXF resume,
FidoNet/binkp, RSS/Atom syndication, Icecast radio) are all built and tested.
The Looking Glass tracker (signed descriptors, gossip, health INDEX) and a
`warren-stampede` load harness round it out.

**Model-only / deferred** (present as tested crates, not yet wired to a live
transport): the Reticulum/RNS layer (`rabbithole-reticulum` — data model,
crypto, link + delay-tolerant tunnel state machines), end-to-end DM/room
encryption wiring, federation flood-fill + `persona@server` attestation over
S2S, and the desktop/mobile GUI shells. Some 1.0-track items (a public
flagship Looking Glass, NAT hole-punching / WebRTC for browser peers, audio
encode pipelines) need environments beyond the repository and are tracked in
`TODO.md`.

## Repository landmarks

- `PLAN.md` — the full-vision phased roadmap
- `TODO.md` — the authoritative wave-by-wave status tracker
- `CHANGELOG.md` — the release history (at the repository root)
- `examples/flagship-burrow.toml` — a fully-commented sample server config
