# examples

Sample configuration and deployment recipes for a RabbitHole **Burrow**.

## `flagship-burrow.toml`

An opinionated, fully-commented configuration for a **public flagship** server
at 0.61.0. It shows every configuration key the server understands: keys whose
built-in default is fine are present but **commented out** (with their default
value shown), so you can read the whole surface top-to-bottom and uncomment only
what you change.

What it turns on out of the box, beyond the always-on native QUIC/WS transports:

- the embedded **HTTP + PWA** surface (web SPA + `/files` download handoff),
- **telnet BBS**, **finger**, **NNTP reader**, **Hotline**, and **radio**
  (delivery + DJ source ingest) legacy surfaces,
- **S2S federation**,
- rate limiting (on by default) shown at its defaults with tuning notes,
- commented sample blocks for door games, federation peers, NNTP feed peers,
  FTN areas, syndication feeds, radio library automation, and keyword teleports.

Use it as a starting point — read the header, then adjust `name`, `motd`,
`agreement`, the public `files_http_base` URL, and the radio source password
(`CHANGE-ME-...`) before going live.

### Loading it

```console
# Explicit path:
$ burrow --config /etc/burrow/flagship-burrow.toml run

# Or drop it as `burrow.toml` next to your --data-dir (the default lookup):
$ cp examples/flagship-burrow.toml ./burrow-data/burrow.toml
$ burrow --data-dir ./burrow-data run
```

Precedence is: **defaults < TOML file < `RABBITHOLE_*` env vars < runtime
`burrow ctl config-set` edits.** A handful of fields also honour env overrides
(`RABBITHOLE_NAME`, `RABBITHOLE_MOTD`, `RABBITHOLE_AGREEMENT`,
`RABBITHOLE_GUEST_ENABLED`, `RABBITHOLE_QUIC_ADDR`, `RABBITHOLE_WS_ADDR`,
`RABBITHOLE_DATA_DIR`).

### live vs. restart, and TOML-only keys

The config file uses `deny_unknown_fields` — a misspelled key refuses to load,
so every key in the sample is a real one. Each key is annotated in the file:

- **live** — re-read per request; a `ctl config-set` (or file edit + reload)
  takes effect immediately (e.g. `motd`, `*_min_role`, all `ratelimit_*`).
- **restart** — binds a listener at startup; changing it needs a server restart
  (e.g. `*_addr`, most `*_enabled`, `http_web_root`).
- **TOML-only** — the maps and arrays at the bottom of the file
  (`[nntp_feed_peers]`, `[radio_library_areas]`, `[ftn_areas]`,
  `[syndication_feeds]`, `[keywords]`, `[[doors]]`, `[[federation_peers]]`);
  `ctl config-set` cannot touch them, edit the file and restart.

Because of a TOML rule, **all scalar keys must appear before the first
`[table]`/`[[array]]` header** — that is why the map/array sections live at the
very bottom of the sample.

## Building the web SPA for `http_web_root`

The embedded HTTP surface serves a static Single-Page App plus an installable
PWA. Build the SPA once with [Trunk](https://trunkrs.dev/) and point
`http_web_root` (or `--web-root`) at its `dist/` output:

```console
$ rustup target add wasm32-unknown-unknown
$ cargo install trunk

$ cd crates/ui-web
$ trunk build --release          # emits crates/ui-web/dist/
```

Then either set it in the config:

```toml
http_enabled  = true
http_web_root = "/path/to/RabbitHole/crates/ui-web/dist"
files_http_base = "https://bbs.example.org"   # public URL of this HTTP surface
```

…or override at launch without editing the file (each flag implies `--http`):

```console
$ burrow --http --web-root crates/ui-web/dist run
  INFO burrow: http listening http=0.0.0.0:8080
```

The checked-in `crates/ui-web/assets/manifest.webmanifest` + service worker
(`assets/sw.js`) make the app installable; if the served root ships no manifest,
the server
generates a minimal one from `name` and `theme_accent`. Terminate TLS at a
reverse proxy in front of `http_addr` — keep-alive, Range, and TLS are not built
into the embedded server.

## Backups: stop → restore → start

Snapshots are taken **live**; restores are **offline** (the server must be
stopped, or the restore refuses while the ctl socket answers):

```console
# 1. Take a consistent snapshot while the server runs:
$ burrow ctl backup ./snapshots/2026-07-03
$ burrow ctl backup-verify ./snapshots/2026-07-03      # re-hash + integrity check

# 2. Stop the server (Ctrl-C / systemctl stop), then restore offline.
#    The current data dir is moved aside to <data_dir>.pre-restore-<ts>
#    (it is never deleted); only manifest-listed paths are copied in.
$ burrow restore ./snapshots/2026-07-03 --data-dir ./burrow-data

# 3. Start the server again:
$ burrow --data-dir ./burrow-data run
```

A snapshot bundles the SQLite DB (via `VACUUM INTO`, WAL-consistent), the
identity keys, `approved_peers.json`, the federation catalog, and the blob
store, each hashed in a `MANIFEST.json`. See `apps/server/src/backup.rs` for the
details.
