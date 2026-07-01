# Contributing to RabbitHole

## Ground rules

- **The plan is the map.** `PLAN.md` defines what lands in which wave and
  why; `TODO.md` tracks it. Work that jumps waves needs a dependency check
  against PLAN §15 first.
- **The protocol spec is code.** Any change to `crates/proto` updates
  `docs/protocol/` in the same PR, and vice versa.
- **Codecs ship with round-trip tests.** Every wire/file format (RHP frames
  now; PKT, QWK, SAUCE, telnet, Hotline transactions later) gets
  encode→decode→encode byte-equality tests and, where inputs are untrusted,
  fuzzing.
- **`core` and `proto` stay wasm-clean.** No tokio, sockets, or fs in their
  default features — CI enforces it.
- **Frontends are adapters.** Domain logic lives in `core`/`server-core`;
  if a CLI/TUI/GUI/telnet surface contains business rules, it's in the
  wrong place.

## Workflow

```console
$ cargo test --workspace
$ cargo clippy --workspace --all-targets   # warnings are errors in CI
$ cargo fmt --all
```

- Branches: feature branches off `main`; PRs need green CI.
- Commit messages: imperative subject, body explains *why*.
- New dependencies: prefer pure-Rust (the mobile cross-compile story is a
  project constraint); justify anything with a C toolchain dependency.

## Naming

Themed names (Burrow, Warren, Looking Glass, Wishing Well, Cheshire mode,
Tunnels) always appear alongside their plain function in docs and UI — 
charming, never confusing. See PLAN §2.1.
