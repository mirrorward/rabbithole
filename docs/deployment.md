# Deploying RabbitHole

This guide covers running a RabbitHole **burrow** (server) in production, plus
notes for the tracker (`looking-glass`) and clients. Three paths are described:
building from source, Docker/Compose, and a systemd service.

The workspace ships these binaries:

| Binary          | Crate         | Role                                  |
| --------------- | ------------- | ------------------------------------- |
| `burrow`        | `apps/server` | The server daemon                     |
| `rabbit`        | `apps/cli`    | Command-line client                   |
| `rabbit-tui`    | `apps/tui`    | Terminal (ratatui) client             |
| `looking-glass` | `apps/tracker`| Tracker / directory service           |

## Ports and transports

Open these on the host/firewall for a public burrow:

| Port | Proto | Purpose                                        |
| ---- | ----- | ---------------------------------------------- |
| 4653 | UDP   | QUIC — primary transport                       |
| 443  | TCP   | Optional WSS reverse proxy for browser clients |
| 4655 | TCP   | S2S federation (when enabled)                  |

Optional legacy surfaces served by `burrow` when enabled in config: telnet
(default `0.0.0.0:2323`, TCP) and finger (default `0.0.0.0:7979`, TCP).

The tracker additionally listens on `5498/tcp` (HTRK listing) and `5499/udp`
(HTRK registration) — only relevant if you run `looking-glass`.

## Configuration

`burrow` reads a TOML file (`burrow.toml`, kept next to the data dir by
default) and then applies `RABBITHOLE_*` environment overrides. Precedence:
defaults < TOML file < environment < runtime `burrow ctl config-set`.

The **only** environment variables that exist (see
`crates/server-core/src/config.rs`) are:

| Variable                   | Default            | Meaning                                       |
| -------------------------- | ------------------ | --------------------------------------------- |
| `RABBITHOLE_NAME`          | `An Unnamed Burrow`| Display name of this burrow                    |
| `RABBITHOLE_MOTD`          | (empty)            | Message of the day shown at sign-in            |
| `RABBITHOLE_AGREEMENT`     | (empty)            | Agreement text users must accept (empty = off) |
| `RABBITHOLE_GUEST_ENABLED` | `true`             | Allow guest sign-in (`true`/`false`/`on`/`off`)|
| `RABBITHOLE_QUIC_ADDR`     | `0.0.0.0:4653`     | QUIC listener socket address                   |
| `RABBITHOLE_WS_ADDR`       | `127.0.0.1:4654`   | Plaintext WebSocket backend (loopback only)     |
| `RABBITHOLE_WS_ALLOW_INSECURE_REMOTE` | `false` | Explicit break-glass remote-plaintext acknowledgement |
| `RABBITHOLE_DATA_DIR`      | `./burrow-data`    | Where the db, blobs, keys, and ctl socket live |

All other settings (registration mode, quotas, telnet/finger, theme, etc.) are
edited via the TOML file or at runtime with `burrow ctl config-set KEY VALUE`.
Listener/data-dir changes require a restart; text/identity fields apply live.

### Public WebSocket access

The built-in WebSocket listener is plaintext and defaults to
`127.0.0.1:4654`. Do not expose or port-forward it. Put an HTTPS-capable reverse
proxy on the public interface, terminate `wss://` there, and proxy `/rhp` to
`ws://127.0.0.1:4654/rhp`. In `burrow.toml`, declare the browser and discovery
truth explicitly:

```toml
ws_addr = "127.0.0.1:4654"
ws_allowed_origins = [
  "https://bbs.example",
  # Packaged Tauri clients; these exact custom-protocol origins are not
  # general web origins and remain opt-in.
  "tauri://localhost",
  "http://tauri.localhost",
  "https://tauri.localhost",
]
ws_public_url = "wss://bbs.example/rhp"
```

Origins are exact scheme/host/effective-port matches; suffixes and wildcards
are not accepted. A non-loopback `ws_addr` is refused unless
`ws_allow_insecure_remote = true` (or the equivalent environment override) is
set. That escape hatch exposes credentials and bearer tokens in transit; it is
intended only for tightly controlled proxy/container topologies, never direct
Internet publication.

For Docker, the proxy cannot reach a listener bound to loopback inside the
burrow container. The checked-in `docker-compose.wss.yml` overlay therefore
uses private Compose networks: burrow binds `0.0.0.0:4654` with the explicit
acknowledgement, Caddy is the only other service, and port 4654 is never
published on the host. Caddy also joins the edge network for ACME/TLS traffic.
Edit `examples/container-wss.toml` and
`examples/Caddyfile` to the same public DNS name, point DNS at the host, then:

```sh
docker compose -f docker-compose.yml -f docker-compose.wss.yml config
docker compose -f docker-compose.yml -f docker-compose.wss.yml up -d

# Must fail: the plaintext backend is not host-published.
! nc -z 127.0.0.1 4654

# Must return the signed descriptor containing wss://YOUR-HOST/rhp.
curl -fsS https://YOUR-HOST/.well-known/rabbithole/server
```

Complete the smoke by authenticating and resuming through
`wss://YOUR-HOST/rhp` with the browser or `warren-stampede`; do not treat a
successful TLS handshake alone as application authentication proof.

Useful commands against a running server (over its local ctl socket):

```sh
burrow ctl status
burrow ctl who
burrow ctl config-get name
burrow ctl config-set motd "Down the rabbit hole"
burrow ctl account-create <login> <password> [role]
```

## Path 1 — Build from source

Requires a Rust toolchain (edition 2021, rust-version 1.85+).

```sh
git clone https://github.com/mirrorward/rabbithole
cd RabbitHole
cargo build --release -p burrow -p rabbit -p rabbit-tui -p looking-glass
```

Binaries land in `target/release/`. Run the server:

```sh
./target/release/burrow --data-dir /var/lib/burrow run
```

The helper `scripts/release.sh` builds all four binaries, strips them, and
stages a checksummed `dist/rabbithole-<version>-<host-target>.tar.gz`.

Pre-built, per-platform archives are also attached to each GitHub Release
(published automatically by `.github/workflows/release.yml` on `v*` tags for
Linux gnu/musl, macOS arm64/x86_64, and Windows x86_64). Each archive has a
`.sha256` companion — verify before use:

```sh
sha256sum -c rabbithole-<version>-<target>.tar.gz.sha256
```

## Path 2 — Docker / Compose

The provided `Dockerfile` is multi-stage: it compiles `burrow` on `rust:slim`
and ships only the stripped binary on `debian:stable-slim`, running as a
non-root `burrow` user with `/data` as a volume.

```sh
docker build -t rabbithole/burrow:latest .
docker run -d --name burrow \
  -p 4653:4653/udp \
  -v burrow-data:/data \
  -e RABBITHOLE_NAME="My Burrow" \
  rabbithole/burrow:latest
```

Or with Compose (edit the `environment:` block in `docker-compose.yml` first):

```sh
docker compose up -d
docker compose logs -f burrow
```

Data persists in the named `burrow-data` volume mounted at `/data`
(`RABBITHOLE_DATA_DIR=/data` is baked into the image).

## Path 3 — systemd

Install the binary and unit, create the service user, and enable it:

```sh
sudo install -m0755 target/release/burrow /usr/local/bin/burrow
sudo useradd --system --home-dir /var/lib/burrow --shell /usr/sbin/nologin burrow
sudo install -m0644 contrib/burrow.service /etc/systemd/system/burrow.service

sudo systemctl daemon-reload
sudo systemctl enable --now burrow
sudo systemctl status burrow
journalctl -u burrow -f
```

The unit (`contrib/burrow.service`) runs as the non-root `burrow` user with a
hardened sandbox (`NoNewPrivileges`, `ProtectSystem=strict`, `PrivateTmp`,
dropped capabilities, a `@system-service` syscall filter, and more). systemd
provisions the data directory via `StateDirectory=burrow` at `/var/lib/burrow`.

To override configuration, either edit `/var/lib/burrow/burrow.toml` or provide
an environment file: uncomment `EnvironmentFile=-/etc/burrow/burrow.env` in the
unit and populate it with `RABBITHOLE_*` assignments from the table above.

## Running the tracker

`looking-glass` is optional and independent of `burrow`. It exposes its status
listener on `0.0.0.0:4655` by default (plus HTRK on `5498/tcp` and `5499/udp`):

```sh
./target/release/looking-glass --status 0.0.0.0:4655
```

## Constrained / RF links

For running RabbitHole reachability over Reticulum on LoRa, packet radio, and
other low-bandwidth, duty-cycle-limited bearers — bandwidth and power budgets,
governor tuning, and topology patterns — see
[`deployment-lora.md`](deployment-lora.md). (Forward-looking: the RNS layer is a
tested pure-Rust model today; the live interface adapter is future work.)
