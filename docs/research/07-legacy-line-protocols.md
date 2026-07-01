# RabbitHole Legacy Line-Protocol Brief

## Overview

RabbitHole must speak six families of legacy TCP line protocols natively. All six share a common shape that maps cleanly onto a Rust async core:

- **Line-oriented, ASCII-command protocols** (finger, NNTP, ident) — CRLF-terminated commands, numeric or text status lines, often a "dot-stuffed multi-line" body terminator (`.\r\n`).
- **In-band binary negotiation** (Telnet) — an escape byte (IAC = 255) multiplexes control commands into an otherwise 8-bit byte stream.
- **HTTP-derived streaming** (Icecast/SHOUTcast) — an HTTP-ish handshake followed by an endless body with periodic inline metadata.
- **Rendering layer** (ANSI/VT100 + CP437) — not a wire protocol but the byte-level presentation contract for the "screen" delivered over Telnet.

The unifying architectural insight: each connection is a state machine over a framed byte stream. Build one `tokio` TCP accept loop per listener, wrap each socket in a protocol-specific codec (`tokio_util::codec::Framed`), and drive a per-connection state machine. Shared domain state (message bases, user presence, mount points) lives behind an actor/handle or `Arc<RwLock<…>>` and is projected into each protocol's view.

---

## Key Features (per protocol)

| Protocol | Port | Framing | Terminator | Auth | RabbitHole role |
|---|---|---|---|---|---|
| Telnet | 23 | 8-bit + IAC escapes | none (stream) | app-level | BBS full-screen TUI |
| finger | 79 | one request line | connection close | none | user/presence lookup |
| ident (RFC 1413) | 113 | one query line | CRLF | none | *client* side mostly |
| NNTP | 119 / 563 (TLS) | CRLF lines | `.\r\n` for multiline | AUTHINFO | message-base syndication |
| Icecast/SHOUTcast | 8000 (conv.) | HTTP handshake + raw stream | never (endless) | source password / Basic | radio streaming |
| ANSI/VT100 | (over telnet) | ESC `[` … sequences | per-sequence | n/a | rendering |

---

## Technical Notes

### 1. Telnet (RFC 854/855 + option RFCs)

**The IAC mechanism.** Everything control-plane is introduced by byte `255 = IAC` ("Interpret As Command"). A literal 0xFF in the data stream must be sent doubled (`IAC IAC`). Core command bytes:

```
240 SE    end subnegotiation
250 SB    begin subnegotiation
251 WILL  253 DO
252 WONT  254 DONT
255 IAC
```

**Option negotiation grammar** is `IAC <WILL|WONT|DO|DONT> <option-code>`, and it is intentionally asymmetric to avoid negotiation loops (RFC 854 rule): you only reply if the request *changes* state. Semantics:

- `WILL x` = "I want to enable x on my side." Peer answers `DO x` (accept) or `DONT x` (refuse).
- `DO x` = "Please enable x on your side." Peer answers `WILL x` / `WONT x`.
- Never acknowledge a request that would leave you in the state you already announced (this is what prevents infinite ping-pong).

**Key options a BBS needs:**

| Option | Code | Purpose |
|---|---|---|
| BINARY (RFC 856) | 0 | 8-bit clean transmission — **required** for CP437/ANSI art (bytes 128–255) |
| ECHO (RFC 857) | 1 | server-side echo; needed for password masking |
| SGA — Suppress Go-Ahead (RFC 858) | 3 | disables half-duplex line turnaround → char-at-a-time interactive mode |
| TTYPE — Terminal-Type (RFC 1091) | 24 | learn client emulation (e.g. "ANSI", "xterm", "vt100") |
| NAWS — window size (RFC 1073) | 31 | learn columns×rows |

**Typical opening handshake** a full-screen BBS sends on connect (server → client):
```
IAC WILL ECHO       (server will echo → client stops local echo)
IAC WILL SGA        (go to character mode)
IAC DO   NAWS       (ask client for its window size)
IAC DO   TTYPE      (ask client for terminal type)
IAC WILL BINARY / IAC DO BINARY   (both directions 8-bit clean)
```

**Subnegotiation (SB/SE)** carries multi-byte payloads:

- NAWS (client → server, unsolicited on resize too):
  `IAC SB NAWS <W-hi> <W-lo> <H-hi> <H-lo> IAC SE` — 16-bit big-endian width and height. Note: any 0xFF inside the payload is doubled.
- TTYPE — server asks, client answers; can be polled repeatedly to cycle through the client's list:
  Server: `IAC SB TTYPE SEND IAC SE`
  Client: `IAC SB TTYPE IS "ANSI" IAC SE` (SEND=1, IS=0)

**How a BBS presents a full-screen ANSI UI over Telnet:** after negotiating ECHO+SGA (character mode, no line buffering), BINARY (8-bit), and learning cols/rows via NAWS, the server treats the socket as a raw terminal: it emits ANSI/VT100 escape sequences (see §6) to position the cursor and paint colored CP437 cells, reads raw keystrokes (including arrow-key escape sequences `ESC [ A/B/C/D`), and redraws regions as needed. It must interleave/strip inbound IAC sequences from the keystroke stream at all times, since the client can renegotiate (especially NAWS) mid-session.

### 2. finger / fingerd (RFC 1288), TCP 79

Dead-simple request/response. Client opens the connection, sends **one CRLF-terminated line**, server replies with free-form text and **closes the connection** (the close is the terminator).

Request grammar (`{Q1}` and `{Q2}` from the RFC):
```
{Q1} = [ /W SP ] [ username ] CRLF            (local query)
{Q2} = [ /W SP ] [ username ] @ host CRLF     (forwarding query)
```
- Empty line (`CRLF` only) → "who's online" / list of all users (the `{C}` case).
- `username CRLF` → detailed info for one user.
- Leading `/W` (the "whois"/verbose switch) → request a longer/more detailed report.
- `user@host` → asks *this* server to relay to `host`. **Security note:** RFC 1288 says forwarding SHOULD be disabled by default (open relays enabled the 1988 Morris-worm-era abuse). RabbitHole should refuse `@` forms.

Response conventions: plain ASCII, lines terminated CRLF, no status codes. Classic fields: login name, real name, terminal/idle, last login, plan/project (`.plan`/`.project` files historically). RabbitHole can synthesize these from user profiles and presence.

### 3. "who"/presence service + ident (RFC 1413)

**Presence/"who".** Not a standardized wire protocol — historically served *via* finger (empty query) or a custom port. RabbitHole should maintain a central presence registry (who's connected, on what protocol, idle time, current activity) and project it into: finger's empty-query listing, an NNTP `LIST`-style extension, a BBS "who's online" screen, and Icecast listener counts. Keep this as one in-memory actor updated by every connection handler on connect/disconnect/activity.

**ident (RFC 1413), TCP 113.** An *identification* protocol, not authentication. A server that received an inbound connection can ask the client host's identd: "who owns the TCP connection with these two ports?"

- Query (to the client's port 113): `<server-port> , <client-port>\r\n` (the two ports as seen by the querying server).
- Response: `<server-port> , <client-port> : USERID : <opsys> : <username>\r\n`
  or `… : ERROR : NO-USER` / `INVALID-PORT` / `HIDDEN-USER` / `UNKNOWN-ERROR`.

**Concept & caveat:** the reply is only as trustworthy as the remote host — it identifies a *user on the client machine*, unverifiable by you. Modern use is mostly IRC-style annotation. For RabbitHole: implementing an *ident server* is low value (privacy leak); the useful direction is an optional *ident client* to annotate inbound NNTP/telnet sessions with a best-effort remote username, purely cosmetic.

### 4. NNTP (RFC 3977 + RFC 4643 auth), ports 119 / 563 (implicit TLS)

**Session model.** Server greets with `200` (posting allowed) or `201` (no posting). Commands are CRLF lines; responses are a 3-digit code + text. Multi-line responses (article bodies, lists, overview) end with a **lone `.` on its own line**, and the body is **dot-stuffed** (any line beginning with `.` gets an extra `.` prepended on send, stripped on receive).

**Command set:**

- `CAPABILITIES` → `101` + list of what the server supports (`VERSION 2`, `READER`, `OVER`, `LIST`, `POST`, `NEWNEWS`, `AUTHINFO USER`, etc.). This replaced the old `MODE READER` sniffing.
- `GROUP <name>` → `211 <count> <low> <high> <name>`; selects a group and sets the "current article pointer" to the first article.
- `LISTGROUP [group [range]]` → `211 …` then a list of article numbers (`.`-terminated).
- Article retrieval by number or `<message-id>`:
  - `ARTICLE` → `220` headers+blank+body
  - `HEAD` → `221` headers only
  - `BODY` → `222` body only
  - `STAT` → `223` (no data — just validates existence and moves the pointer)
- `NEXT` / `LAST` → `223` move the current-article pointer forward/back within the group.
- `POST` → `340` (send it), client sends dot-terminated article, server → `240` (accepted) / `441` (rejected).
- `IHAVE <message-id>` → server-to-server feed offer; `335` send / `435` don't want / `436` try later.
- Overview: `OVER [range]` / legacy `XOVER` → `224` + tab-delimited overview lines. Field order is fixed by the "overview format" (`LIST OVERVIEW.FMT`): `number \t Subject \t From \t Date \t Message-ID \t References \t :bytes \t :lines [\t extra headers]`.
- `NEWNEWS <wildmat> <date> <time>` → `230` + message-IDs posted since a timestamp (syndication/catch-up).
- `LIST [ACTIVE|NEWSGROUPS|OVERVIEW.FMT]` → group catalog & metadata.

**Auth (RFC 4643):** `AUTHINFO USER <name>` → `381`, then `AUTHINFO PASS <pw>` → `281` (accepted) / `481` (rejected). `AUTHINFO SASL <mech>` for stronger mechanisms. Servers should offer AUTHINFO USER/PASS **only over TLS** (563 or `STARTTLS`), since it's cleartext.

**Article format** (RFC 5536/5322 style) — the load-bearing headers:
```
Message-ID: <unique@host>      globally unique, immutable identity
Newsgroups: rabbit.general,rabbit.tech   comma-separated, may cross-post
References: <parent@host> <grand@host>    threading chain (build trees from this)
From:, Subject:, Date:          required display headers
```

**Mapping RabbitHole message bases → newsgroups (for syndication):**
- Each message base becomes one newsgroup with a dotted hierarchical name (`rabbithole.general`, `rabbithole.support.rust`).
- Maintain a stable per-group monotonic **article-number index** (NNTP requires sequential water-mark low/high per group) *separately* from your internal post IDs — clients page by number.
- Assign each post a permanent `Message-ID` on creation; store it so `ARTICLE <message-id>` and dedup on `IHAVE`/`POST` work.
- Derive `References` from your reply-parent links so threaded readers reconstruct trees.
- Precompute an **overview cache** (the OVER fields) per article — this is the hot path for readers listing a group; computing it on demand per request is the classic performance mistake.
- For federation, implement `IHAVE`/`NEWNEWS` so peers can pull/push; dedup strictly by Message-ID.

### 5. Radio streaming (Icecast / SHOUTcast)

Two distinct connection types on the same server:

**Source connection (encoder → server).** Modern Icecast uses HTTP `PUT`/`SOURCE` to a mount point with an `Authorization: Basic` (source password). Legacy SHOUTcast DNAS uses a bare password line then `icy-` headers on the source port+1. The source then streams encoded audio indefinitely. The server fans the bytes out to all listeners on that mount.

**Listener connection (player ← server).** An HTTP-style GET on the mount point:
```
GET /stream.mp3 HTTP/1.0
Icy-MetaData: 1            ← "I can parse inline metadata"
```
Server response:
```
ICY 200 OK                (or HTTP/1.0 200 OK on Icecast)
icy-name: RabbitHole Radio
icy-genre: …
icy-br: 128
icy-metaint: 8192         ← bytes of audio between metadata blocks
Content-Type: audio/mpeg  (or application/ogg, audio/ogg)
```
then the endless audio body.

**Mount points** are the routing key: `/live.mp3`, `/jazz.ogg`. One source binds a mount; N listeners subscribe to it. Multiple mounts = multiple stations.

**Inline metadata (`icy-metaint`).** *Only sent if the listener requested `Icy-MetaData: 1`.* The server inserts a metadata block after **exactly `icy-metaint`** bytes of audio, repeating forever. Block format:
- 1 length byte `L`; actual metadata length = `L × 16`.
- Then `L×16` bytes: `StreamTitle='Artist - Track';StreamUrl='…';` NUL-padded to the 16-byte boundary.
- When nothing changed, send `L = 0` (a single zero byte, no payload) — this is the common case and avoids re-sending the title every interval.
- **8192 is the safe conventional interval** — some players choke on other values.

**Transport details:**
- MP3/AAC: raw byte stream, framing is intrinsic (frame sync words); metadata interleaving as above works because MP3 is resync-tolerant.
- Ogg (Vorbis/Opus): metadata is carried **in-stream via Ogg pages / Vorbis comments**, *not* via icy-metaint — don't splice ICY blocks into Ogg. Icecast signals track changes by starting a fresh Ogg logical stream.
- Use HTTP/1.0-style endless body (no `Content-Length`); **avoid `Transfer-Encoding: chunked`** for classic ICY clients — legacy players expect a raw stream, and ICY metadata splicing is incompatible with chunk framing. (Chunked is fine only for modern HTTP-audio clients that don't ask for icy-metaint.)

**Listener count** = number of live listener sockets per mount; exposed via Icecast's `/status-json.xsl` / admin `/admin/stats`. RabbitHole should track this in the same presence registry as §3.

### 6. ANSI / VT100 escape sequences + CP437

The presentation contract for anything drawn over Telnet.

**Structure.** Control Sequence Introducer = `ESC [` = `0x1B 0x5B`. General form: `ESC [ <params;…> <final-byte>`.

**Cursor positioning / screen:**
```
ESC[<row>;<col>H   or ESC[<r>;<c>f   move cursor (1-based)
ESC[<n>A/B/C/D                        up/down/right/left n cells
ESC[s / ESC[u                         save / restore cursor
ESC[2J                                clear screen
ESC[K                                 clear to end of line
ESC[?25l / ESC[?25h                   hide / show cursor
```

**SGR colors (Select Graphic Rendition), `ESC[<params>m`:**
```
0 reset  1 bold/bright  4 underline  5 blink  7 reverse
30–37 foreground (black,red,green,yellow,blue,magenta,cyan,white)
40–47 background
```
Classic 16-color ANSI-BBS palette = the 8 base colors × the bold bit for bright foregrounds (e.g. `ESC[1;31m` = bright red). Blink historically toggled "bright background" on some clients. RabbitHole can *also* emit `ESC[38;5;<n>m` (256-color) / `ESC[38;2;r;g;bm` (truecolor) when TTYPE indicates a modern emulator, and fall back to the 16-color set for `ANSI`/`vt100`.

**CP437.** The DOS/BBS code page. Bytes 0–127 are ASCII; **128–255 are the load-bearing part**: box-drawing (`0xC9 0xCD 0xBB` etc.), blocks (`0xB0 0xB1 0xB2 0xDB` — the shading/solid blocks that make ".ans" art), and accented glyphs. This is *raw bytes*, not UTF-8 — hence Telnet **BINARY mode is mandatory** or the high bytes get mangled.
- `.ans` art files are literally CP437 bytes + embedded SGR escapes — RabbitHole can stream them through nearly verbatim.
- For modern UTF-8 terminals, ship a CP437→Unicode translation table (each of the 256 bytes has a canonical Unicode target) and pick per-connection based on TTYPE/encoding negotiation.

---

## Pitfalls & Lessons

- **Telnet negotiation loops.** Blindly replying to every WILL/DO causes infinite ping-pong. Track option state per side and only respond when the request *changes* state (RFC 854's core rule).
- **Telnet 0xFF escaping.** Any literal `0xFF` in data (common in CP437 art and NAWS payloads) must be doubled to `IAC IAC`. Forgetting this corrupts art and window-size reads.
- **BINARY mode omitted** → CP437 high bytes stripped to 7 bits → garbled art. Always negotiate BINARY before painting art.
- **NAWS is dynamic.** Clients resend it on resize mid-session; your inbound parser must always be watching for IAC even while reading keystrokes.
- **finger forwarding = open relay.** Refuse `user@host`; RFC 1288 warns explicitly. Also cap output size / don't leak sensitive fields.
- **ident is not auth.** Treat any reply as an unverified hint. Don't run an ident *server* unless you accept the privacy exposure.
- **NNTP dot-stuffing.** Both directions. A body line starting with `.` must be doubled on send and un-doubled on receive; a lone `.` is the terminator. Getting this wrong truncates articles or leaks the terminator into content.
- **NNTP article numbering.** Group article numbers are a per-group monotonic sequence with published low/high water marks — you cannot reuse or reorder them; keep them separate from internal IDs. Message-IDs must be globally unique and permanent.
- **NNTP overview performance.** Readers list groups via OVER constantly; compute overview lines lazily on first post and cache them, never per-request.
- **AUTHINFO over cleartext.** USER/PASS is plaintext — only advertise it on TLS (563/STARTTLS).
- **Icecast metadata splicing math.** Off-by-one on the `icy-metaint` byte count desyncs every player permanently — the counter must reset to exactly the interval after each block, and the length byte is *units of 16*. Send `0x00` when unchanged rather than re-sending the title.
- **Ogg ≠ ICY metadata.** Never inject icy-metaint blocks into Ogg/Opus; use Vorbis comments / new logical streams. Also don't send `icy-metaint` unless the listener asked with `Icy-MetaData: 1`.
- **No Content-Length on streams.** Endless body; avoid chunked encoding for classic ICY clients.
- **ANSI 1-based coordinates.** `ESC[1;1H` is the top-left, not `0;0`. And always `reset` (`ESC[0m`) between colored regions or attributes bleed.

---

## Implications for RabbitHole (core mapping + Rust crates)

**Server core.** One `tokio` runtime; a listener task per protocol/port (`TcpListener::accept` loop, `tokio::spawn` per connection). Model each connection as an explicit state machine. Use `tokio_util::codec::{Framed, Decoder, Encoder}` to write per-protocol codecs:
- Line codecs (finger, NNTP, ident): `LinesCodec` or a custom CRLF/dot-terminator decoder.
- Telnet: a custom `Decoder` that splits the stream into `Data(Bytes)` / `Command` / `Subnegotiation` events, handling IAC escaping and 0xFF doubling internally.
- Icecast: manual HTTP handshake then a broadcast fan-out.

**Shared state.** A single presence/registry actor (task owning state + `mpsc` command channel, or `Arc<RwLock<…>>`) feeds finger's who-list, the BBS who's-online screen, NNTP session info, and Icecast listener counts. Message bases live behind a store that exposes both a native API and NNTP projections (group index, article-number map, Message-ID map, overview cache). Radio mounts use `tokio::sync::broadcast` to fan a source's audio `Bytes` chunks to N listener tasks.

**Per-item crate guidance:**

| Area | Crates |
|---|---|
| Async core | `tokio`, `tokio-util` (codecs), `bytes` |
| Telnet | `telnet` crate for reference/parsing (simple; may be easier to hand-roll a `Decoder` for full server-side control of NAWS/TTYPE) |
| TLS (NNTP 563, streaming) | `tokio-rustls` / `rustls` |
| NNTP | no mature *server* crate — build on `tokio-util` codecs; `nntp-proxy` is a Tokio-based reference for structure; the old `nntp` client crate is reference-only |
| Article/date/ID | `uuid` (Message-IDs), `chrono`/`time` (RFC 5322 dates), `mailparse`/custom for header parse |
| Icecast/streaming | hand-rolled HTTP handshake + `tokio::sync::broadcast`; `hyper`/`axum` if you want the modern HTTP-audio path too; audio framing is pass-through so no decoder needed |
| ANSI/VT100 render | `crossterm`/`termcolor` for sequence constants, or hand-emit; `ratatui` can drive a TUI whose output you serialize to the socket (with a custom backend) |
| CP437 | `codepage-437` / `encoding_rs`-adjacent tables for CP437↔Unicode translation |
| Wildmat (NNTP NEWNEWS/LIST) | small hand-rolled matcher (wildmat ≠ glob exactly) |

**Recommended build order:** (1) Telnet + ANSI/CP437 rendering core — it unblocks the whole interactive BBS surface and forces the connection-state-machine pattern you'll reuse. (2) finger + presence registry — trivial and immediately useful, establishes shared state. (3) NNTP reader (GROUP/ARTICLE/OVER/POST + AUTHINFO over TLS) against your message bases. (4) Icecast streaming. (5) NNTP peering (IHAVE/NEWNEWS) and optional ident client last.

---

Sources:
- [RFC 3977 — NNTP](https://datatracker.ietf.org/doc/rfc3977/) · [RFC 4643 — NNTP Authentication](https://www.rfc-editor.org/rfc/rfc4643.html)
- [RFC 1073 — Telnet Window Size (NAWS)](https://www.rfc-editor.org/rfc/rfc1073) · [RFC 1091 — Telnet Terminal-Type](https://www.rfc-editor.org/rfc/rfc1091.html) · [Telnet Negotiation (MUD-Dev)](http://mud-dev.wikidot.com/telnet:negotiation)
- [SmackFu — SHOUTcast Metadata Protocol](http://www.smackfu.com/stuff/programming/shoutcast.html) · [Icecast Protocol spec (gist)](https://gist.github.com/niko/2a1d7b2d109ebe7f7ca2f860c3505ef0) · [ICY docs (cast.readme.io)](https://cast.readme.io/docs/icy)
- [tokio](https://crates.io/crates/tokio) · [telnet crate](https://crates.io/crates/telnet) · [nntp-proxy](https://crates.io/crates/nntp-proxy) · [Rust network crates (lib.rs)](https://lib.rs/network-programming)

Note on the RFCs I answered from knowledge (not re-fetched but standard): RFC 854/855 (Telnet base, IAC/WILL/WONT/DO/DONT), RFC 856 (BINARY), 857 (ECHO), 858 (SGA), RFC 1288 (finger, `{Q1}/{Q2}`, `/W`, forwarding-disabled advice), and RFC 1413 (ident query/response format). Web search confirmed the numeric details for NNTP status codes, Telnet option codes/subnegotiation grammar, and the icy-metaint block format (length byte × 16, 8192 convention, `Icy-MetaData: 1`).
