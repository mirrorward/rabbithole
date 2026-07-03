# warren-stampede

A load generator that drives many concurrent RHP sessions against a burrow.
It reuses the real client core (`rabbithole-core`) — the same transport
selection, hello negotiation, auth, chat, and push handling every frontend
uses — so what it measures is what real rabbits experience.

## Usage

```text
warren-stampede --url ws://127.0.0.1:4654 --sessions 100 --duration 60 --scenario chat
```

- `--url` — `ws://host:port` (WebSocket) or `host:port` (QUIC, with
  `--fingerprint <hex>` and optionally `--server-name`).
- `--sessions N` — concurrent session target.
- `--ramp-per-sec R` — session start rate during ramp-up.
- `--duration SECS` — run length from the start of the ramp.
- `--scenario idle|chat|mixed` — keepalive only / a jittered lobby line
  every 5–15 s (own-echo RTT measured) / 80% idle + 20% chat.
- `--guests` (default) or `--user-prefix load --password s3cret` for
  pre-created accounts named `load0`, `load1`, ….
- `--max-errors N` — circuit breaker: abort (exit 2) once errors exceed N.
- `--max-reconnects N` — bounded per-session reconnects after a drop.
- `--json` — machine-readable final report on stdout.

A progress line prints to stderr every 5 s; Ctrl-C drains sessions cleanly
and still prints the report. Exit codes: `0` clean, `1` finished with
errors, `2` circuit-breaker abort.

## The 10k stampede (real hardware, not CI)

The design target is 10 000 concurrent sessions against a dedicated burrow.
Raise the file-descriptor limit on both ends (`ulimit -n 65536`) and run:

```text
warren-stampede \
    --url ws://burrow.example.net:4654 \
    --sessions 10000 --ramp-per-sec 200 --duration 600 \
    --scenario mixed --guests --max-errors 500 --json > stampede.json
```

CI runs only the in-process smoke (`tests/smoke.rs`): 50 guest chat
sessions for ~5 s against a real burrow, asserting zero errors, every
session echoed, and sane connect latency.
