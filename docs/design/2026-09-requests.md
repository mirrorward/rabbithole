# Requests of 2026-09-18: header, bookmarks, profiles, downloads, swarm, radio

> Kevin's list from the lobby and radio screenshots, what already exists for each
> item, and the slice plan. Findings come from reading the code, with paths, so
> each slice builds on what is there. Status is kept current as slices land.

## The list

| # | Request | Size | Status |
|---|---|---|---|
| 1 | News bulletin has rounded corners; it should sit flush inside its container | small | 0.207.0 |
| 2 | Header: burrow mark cut off; status dropdown not vertically centred; coloured icon per status; Leave does nothing and should be red | small | 0.207.0 |
| 3 | Directory: bookmark burrows (kept even when they drop off the list), manage them, add custom ones, show their status | medium | 0.208.0 |
| 4 | Clicking a username in the lobby opens their profile | small | 0.207.0 |
| 5a | Demo downloads fail; they should be testable in full | small | 0.209.0 |
| 5b | Download asks where to save, unless a default folder is set; option for per-burrow subfolders | medium | 0.211.0 (desktop; a browser tab cannot choose) |
| 5c | Download offers swarm options: other burrows and people you have access to | medium/large | 0.212.0 within one burrow (Best available / Peers only / This burrow only, with origin fallback); sources on other burrows need per-source tickets and wait for the server-to-server design |
| 6 | People with downloads can opt in to helping other people's swarm downloads | medium | 0.213.0 (desktop, opt-in, in memory while the app is open) |
| 7 | Server-to-server transfers: pick files or folders to send to another burrow you are on, optional swarm, permissions on both sides, clear errors, pieces from participants without exposing the destination | large, security-sensitive | 0.225.0 (burrows) and 0.226.0 (app): the destination-initiated pull over the federation session, approved peers only, filed under the person, trees recreated. Not yet: the swarm leg, pulling from a non-peer. [`server-to-server-transfers.md`](server-to-server-transfers.md) |
| 8 | Radio: the server advertises the stream URL (never ask the user); player shows the current song with cover and the last 10; the burrow uses its own streaming server | large | 0.210.0 (advertised address, cover, last ten) and 0.214.0 (a library rotation streams its own audio) |

## Findings

### 2. Why Leave did nothing

`confirm_leave` used `window.confirm()`. A desktop webview answers that `false`
without showing anything, so in the app the button was inert. Fixed with an
in-app `alertdialog` (`AppState::confirm`, `ConfirmAsk`, `ConfirmIntent`). The
answer is carried out by `AppState` from an intent, not a stored closure: the
asking view can be disposed before the answer arrives.

### 5. Downloads today

- `AppState::download` (`crates/ui-web/src/app.rs`) branches on
  `native::native_available()`. Native: `native::start_swarm_download`, written
  by `apps/desktop/src/transfers.rs` to `download_dir()/sanitize_name(name)`.
  No dialog, no setting. Web: `FileCommand::Download`, bytes over the socket,
  saved by `WsClient::save_download` (Blob + `<a download>`).
- **Demo**: `MockClient` answers with `vec![0; size]` and nothing ever calls
  `save_download` (it lives only in `ws.rs`), so the row reads Done and no file
  lands. `FAILING_DEMO_FILE` is the deliberate failure fixture and stays.
  Fix: seeded real bytes plus a shared save helper both paths call.
- `apps/desktop` has no `tauri-plugin-dialog` or `tauri-plugin-fs`; commands
  are `ping, tick_ack, fullscreen_state, tracker_index, native_available,
  connect_native, swarm_start_download`.
- Settings: `crates/ui-web/src/settings.rs` (`rh.settings.v1`), every new field
  needs `#[serde(default)]`. A native folder choice must round-trip through a
  Tauri command: `localStorage` is webview-scoped, and the shell must re-sanitise
  and contain any path the webview hands it.

### 5c, 6. The swarm

- `crates/swarm`: blake3 root = blob id, 1 MiB units, Bao blocks, QUIC only.
  Peers advertise roots (`Caps::SWARM_ADVERTISE`), `FindSources(root)` returns a
  `SourceList`, `SourceTicket` is a `CapToken{root, fetcher, expires}` signed by
  the origin server, 10 minutes, gated by `FILE_DOWNLOAD`.
- **A ticket authorises one root, for one named fetcher, within one server's
  swarm.** Peers verify against their own server's key. Striping one fetch
  across burrows needs per-`SourcePeer` tokens, which
  `docs/design/native-swarm-backend.md` defers.
- Swarm is already the native default; the UI never says so and has no toggle.
  `SwarmError::NoPeerSources{server_has}` is not treated as "fall back to the
  origin" today.
- Seeding exists only in the CLI (`apps/cli` `SwarmAction::Share`: `SeedStore`,
  `PeerServer`, `swarm_contact`, re-announce at two thirds of the TTL). The
  desktop shell never constructs them. Browsers cannot seed until the WebRTC
  gateway lands.

### 7. Server-to-server

Federation (`apps/server/src/federation.rs`, `crates/federation`) moves signed
catalogs, search and board events between admin-approved peers. It moves no file
bytes; `fanout::plan_fetch` plans and fetches nothing. Upload permission hooks
exist at the destination (`Caps::FILE_UPLOAD`, `upload_quota_bytes`,
`handlers8.rs`), download hooks at the source (`FILE_DOWNLOAD`, `FILE_LIST`).

Proposed model, to be reviewed before any code: a **destination-initiated
pull**. The person asks the destination burrow to fetch root R from source S;
the destination checks their `FILE_UPLOAD` and quota there, the source checks
their `FILE_DOWNLOAD` there, and the destination fetches as a swarm client with
its own ticket. Participants holding pieces serve the destination the way they
serve any fetcher, so they never need an account on it, and the destination
opens no new listener. This keeps the existing trust anchor. It is
security-sensitive (a burrow acting on a person's behalf at another burrow) and
gets a design review before implementation, per the working agreement.

### 8. Radio today

- Server: `radio_enabled` defaults **false**; `radio_addr` `0.0.0.0:8000`; ICY
  listeners at `http://host:8000/<slug>`; SOURCE auth via `Caps::BROADCAST`;
  no TLS, no CORS. **An autodj/library station has no byte fan-out**: only live
  SOURCE mounts stream, so "ambient AUTO" is metadata only and 404s as audio.
- Protocol (`crates/proto/src/radio.rs`, family 9): `RadioNowPlaying` and
  `RadioOff` only. No station list, stream URL, cover, or history. Postcard
  encoding: add a **new message type**, do not append fields
  (`docs/protocol/versioning.md`). Registry count and golden must be bumped.
- Client: the typed address is `RadioPrefs.base` (`rh-radio` in localStorage),
  `radio::stream_url(base, station)`, prompt in `RadioPlayerPanel`.
- Images can already travel: `BlobGet`/`BlobData`, as avatars do.

Plan: (a) a new `RadioStations` message carrying per-station `stream_url`,
`cover: Option<BlobId>` and `recent: Vec<Track>` (cap 10), with
`radio_public_base` config beside `files_http_base` and a fallback derived from
the socket's host plus the radio port; (b) a 10-deep ring in `Program`; (c) the
player pane redrawn around now-playing and history, the address field deleted;
(d) closing the autodj byte gap so the burrow's own station actually streams.

## Slice order

1. Header, news band, lobby names (1, 2, 4).
2. Bookmarks on the connect window and the Looking Glass (3).
3. Demo downloads that save real bytes (5a).
4. Radio: advertised stream URL, now-playing with cover, last 10 (8a to 8c).
5. Save location: dialog, default folder, per-burrow subfolders (5b).
6. Swarm choices on download, origin fallback (5c within one burrow).
7. Opt-in seeding from the desktop shell (6).
8. Radio autodj byte fan-out (8d).
9. Server-to-server transfers: design review, then build (7, and cross-burrow 5c).
