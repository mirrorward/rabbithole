# RabbitHole

> Down the rabbit hole: a modern Rust revival of the golden age of online
> communities — Hotline, KDX, BBSes, and AOL — one server, many doors.

A **Burrow** (server) hosts chat rooms, message boards, file libraries, direct
messages, a swarm file-distribution layer (**the Warren**), pirate radio, and a
request board (**the Wishing Well**) — reachable from native apps, terminals,
browsers, telnet BBS clients, newsreaders, offline mail readers, and (yes)
real classic Hotline clients. Servers federate over **Tunnels** and are
discoverable through **Looking Glass** directories.

**Status: Wave 1 (vertical slice) — a server you can talk to.**

- [`PLAN.md`](PLAN.md) — the full-vision phased roadmap
- [`TODO.md`](TODO.md) — the wave-by-wave tracker
- [`docs/research/`](docs/research/) — the research briefs behind the design
- [`docs/protocol/`](docs/protocol/) — the RabbitHole Protocol (RHP) spec

## What exists today

| Piece | State |
|---|---|
| `crates/proto` | RHP framing + session/presence/chat families, hello/version/capability negotiation, push-replay sequencing (wasm-clean) |
| `crates/identity` | Ed25519 identity keys, Argon2id passwords, hashed session tokens, TOTP + recovery codes |
| `crates/net` | `Transport` trait with QUIC (quinn, fingerprint pinning) + WebSocket implementations |
| `crates/blobs` | content-addressed blob store (blake3, refcounted GC) |
| `crates/store-server` | accounts, classes, sessions, ACLs, audit log (sqlx/SQLite WAL) |
| `crates/server-core` | event bus, config (TOML+env+live), roles/classes/**ACL evaluator** (property-tested), auth (password/guest/resume), presence registry, lobby chat, push replay log |
| `crates/core` | native `Client` driver: request/reply + push buffering + auth/chat/who helpers |
| `apps/server` (`burrow`) | full session state machine, persistent Ed25519 + TLS identity, agreement gate, `burrow ctl` unix admin socket |
| `apps/cli` (`rabbit`) | `login` (password/guest, QUIC or WS), `who`, `say`, `history`, `tail`, `status`, `--json` |
| everything else | stubs awaiting their wave (see PLAN.md §15) |

## Try it

```console
$ cargo run -p burrow -- run
  INFO burrow: burrow is up quic=0.0.0.0:4653 ws=0.0.0.0:4654 fingerprint=<hex>

# Admin from another terminal (unix socket, no network):
$ burrow ctl account-create alice wonderland user
$ burrow ctl config-set motd "Down the rabbit hole we go"

# Sign in and chat:
$ rabbit login ws://127.0.0.1:4654 --user alice --password wonderland
$ rabbit say "oh my ears and whiskers"
$ rabbit who
$ rabbit tail            # stream the lobby

# QUIC with certificate pinning (fingerprint from `burrow ctl status`):
$ rabbit login 127.0.0.1:4653 --fingerprint <hex> --server-name localhost --guest
```

## Development

```console
$ cargo test --workspace          # full suite
$ cargo clippy --workspace --all-targets
$ cargo fmt --all
$ cargo check -p rabbithole-proto -p rabbithole-core --target wasm32-unknown-unknown
```

See [`CONTRIBUTING.md`](CONTRIBUTING.md). License: **TBD** (being decided —
see PLAN.md §16; until then, all rights reserved by the project owner).
