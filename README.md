# RabbitHole

> Down the rabbit hole: a modern Rust revival of the golden age of online
> communities — Hotline, KDX, BBSes, and AOL — one server, many doors.

A **Burrow** (server) hosts chat rooms, message boards, file libraries, direct
messages, a swarm file-distribution layer (**the Warren**), pirate radio, and a
request board (**the Wishing Well**) — reachable from native apps, terminals,
browsers, telnet BBS clients, newsreaders, offline mail readers, and (yes)
real classic Hotline clients. Servers federate over **Tunnels** and are
discoverable through **Looking Glass** directories.

**Status: Wave 0 (foundations).** The plan is the product right now:

- [`PLAN.md`](PLAN.md) — the full-vision phased roadmap
- [`TODO.md`](TODO.md) — the wave-by-wave tracker
- [`docs/research/`](docs/research/) — the research briefs behind the design
- [`docs/protocol/`](docs/protocol/) — the RabbitHole Protocol (RHP) spec

## What exists today

| Piece | State |
|---|---|
| `crates/proto` | RHP framing, families, hello/version/capability negotiation (wasm-clean) |
| `crates/identity` | Ed25519 identity keys, Argon2id passwords, session tokens, TOTP + recovery codes |
| `crates/net` | `Transport` trait with QUIC (quinn, fingerprint pinning) + WebSocket implementations |
| `crates/blobs` | content-addressed blob store (blake3, refcounted GC) |
| `crates/store-server` / `store-client` | SQLite migration harnesses (sqlx / rusqlite) |
| `crates/server-core` | event bus (every protocol surface subscribes to one stream of truth) |
| `apps/server` (`burrow`) | binds QUIC 4653 + WS 4654, answers RHP hellos |
| `apps/cli` (`rabbit`) | `rabbit hello <endpoint>` — dial a burrow over either transport |
| everything else | stubs awaiting their wave (see PLAN.md §15) |

## Try it

```console
$ cargo run -p burrow -- --name "My First Burrow"
  INFO burrow: generated self-signed TLS identity … fingerprint="<hex>"
  INFO burrow: quic listener up addr=0.0.0.0:4653
  INFO burrow: websocket listener up addr=0.0.0.0:4654

# In another terminal — QUIC with the fingerprint from the server log:
$ cargo run -p rabbit -- hello 127.0.0.1:4653 --fingerprint <hex> --server-name localhost
connected to "My First Burrow" (rhp/1 quic)

# …or WebSocket, no pinning needed on loopback:
$ cargo run -p rabbit -- hello ws://127.0.0.1:4654
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
