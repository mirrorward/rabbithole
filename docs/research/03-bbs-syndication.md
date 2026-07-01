# RabbitHole Technical Brief: BBS Software & Message Networking (Syndication)

## Overview

The BBS ecosystem RabbitHole is reviving splits into two eras that still coexist in modern software:

- **The dial-up/DOS era (1980s–90s):** message bases, door games, ANSI art, X/Y/Zmodem transfers, and **FidoNet** as the dominant store-and-forward mail network. Formats here are byte-level, little-endian, DOS-centric (CP437, MS-DOS timestamps, ARC/ZIP bundling).
- **The Internet-bridge era (mid-90s onward):** BBSes gatewayed to **Usenet (NNTP)** and email (SMTP), and offline readers exchanged **QWK/QWKE** packets. FidoNet moved from modems to TCP via **binkp**.

Five actively-maintained packages define the current state of the art, and each is a useful reference implementation:

| Software | Lang | Notes for us |
|---|---|---|
| **Synchronet** | C/C++ (+ JS scripting via SpiderMonkey) | The reference. Ships FidoNet (SBBSecho tosser + binkp/BinkIT), QWK networking, full NNTP/SMTP/POP3/web servers, its own SMB message base. Best-documented format wiki. |
| **Mystic BBS** | Pascal (FreePascal) | Closed-source but hugely popular in the modern scene. Built-in FidoNet tosser (MUTIL), QWK, telnet/ssh. Great UX/theming reference. |
| **WWIV** | C++ | Historically its own **WWIVnet** network (simpler than FTN); now open source. Good study of a non-FTN store-and-forward design. |
| **ENiGMA½** | Node.js | Modern, cross-platform, actively developed. FTN + QWK. The closest philosophical cousin to a "Rust revival" — read its source for clean modern reimplementations of these formats. |
| **Citadel** | C | Different lineage (room-based, not message-area-based). Citadel has its own **C/86 / Citadel networking** and modern **IGnet**; today leans into SMTP/IMAP/XMPP. Good model for room/threaded semantics. |

**Design takeaway for us:** the message base and the network syndication layer should be decoupled. Every one of these systems has a native store and separate scanner/tosser processes that translate to/from wire formats. We should treat FTN, QWK, and NNTP as *pluggable syndication adapters* over a canonical internal message model.

---

## Key Features (functional surface to match)

**Message bases.** Public message "areas"/"conferences" (echomail-style) plus private mail. Threading (reply/reference chains), read pointers per-user, area subscription/security gating. Synchronet's internal store is **SMB** (Synchronet Message Base): an index file + data file with fixed headers and variable data fields — a good model: store canonical messages once, project into any wire format.

**Door games / external programs.** The BBS drops a **dropfile** describing the current session into a known directory, then execs the door. Formats:
- **DOOR.SYS** — the ubiquitous ~52-line ASCII file (COM port, baud, user name, security level, time left, ANSI y/n, node, etc.). Line-oriented, one value per line.
- **DORINFO1.DEF** (RBBS/QuickBBS) — another common ASCII layout.
- **DOOR32.SYS** — the modern standard for socket/telnet doors: 11 lines — line 0 = comm type (0=local,1=serial,2=telnet), line 1 = comm/socket handle, line 2 = baud, line 3 = BBSID, line 4 = user record #, line 5 = real name, line 6 = handle/alias, line 7 = security level, line 8 = minutes left, line 9 = emulation (0=ascii,1=ansi,2=avatar,3=rip,4=maxgfx), line 10 = node number.
- **CHAIN.TXT** (WWIV), **PCBOARD.SYS/USERS.SYS** (PCBoard) also exist.
- For socket doors, the handle passed on line 1 is the actual OS socket descriptor — the door inherits the connection. On Windows this needs `WSADuplicateSocket`; on Unix it's an inherited fd. **This is the single trickiest interop detail for door support** and we must decide our model (inherited fd vs. a local telnet/pty bridge). ENiGMA½ solves it by spawning a telnet bridge for stdio doors.

**Sysop tools.** User editor, message-area manager, node/chat monitor, sysop-can-break-in-to-chat, activity logs, event scheduler (for nightly toss/pack/poll events).

**User accounts & security.** The classic model is a single integer **security level** (0–255) plus a bitmask of **flags/AR (access restrictions)** and **exemptions**. Access to areas/doors/commands is gated by "requires SL ≥ N and flag X." Time limits and download ratios per level. Simple, and worth replicating as a baseline even if we add RBAC later.

**ANSI/ASCII art & CP437.** Screens are CP437-encoded byte streams with embedded **ANSI X3.64 / VT100 escape sequences** (`ESC[` CSI): SGR colors (30–37 fg, 40–47 bg, 1=bold/bright), cursor positioning, save/restore. The 16-color DOS palette + 256 CP437 glyphs (box-drawing 0xB0–0xDF, etc.) are the aesthetic. Variants: **PCBoard @X codes** (`@X0F`), **Wildcat**, **Avatar/AVT** (compressed control codes, RLE), **RIPscrip** (vector graphics). We must render CP437→Unicode (there's a canonical 1:1 mapping; note 0x00–0x1F have printable glyphs in CP437 that ASCII treats as control — the "picture-for-control-code" issue). Also handle **cursor-based animation** (art that repositions the cursor), not just linear color runs.

**Multi-node chat.** Inter-node messaging, teleconference/multi-user chat rooms, node listing, page-sysop. Needs a shared IPC/pub-sub layer between nodes — trivial in a modern single-process Rust server with channels, historically done with shared memory/files.

---

## Technical Notes (the formats!)

### CP437 & SAUCE (verified)

**SAUCE** ("Standard Architecture for Universal Comment Extensions", ACiD, current rev 5) — metadata appended to art files. It is the **last 128 bytes** of the file, and if any comment lines are present they immediately precede it.

SAUCE record = 128 bytes, fields in order (all char fields space-padded, NOT null-terminated):

| Offset | Len | Field | Notes |
|---|---|---|---|
| 0 | 5 | ID | literally `"SAUCE"` |
| 5 | 2 | Version | `"00"` |
| 7 | 35 | Title | |
| 42 | 20 | Author | |
| 62 | 20 | Group | |
| 82 | 8 | Date | `CCYYMMDD` |
| 90 | 4 | FileSize | uint32 LE, original size **excluding** SAUCE |
| 94 | 1 | DataType | e.g. 1=Character |
| 95 | 1 | FileType | e.g. for DataType 1: 0=ASCII, 1=ANSI, 2=ANSImation |
| 96 | 2 | TInfo1 | uint16 LE — for ANSI: character width |
| 98 | 2 | TInfo2 | uint16 LE — number of lines |
| 100 | 2 | TInfo3 | |
| 102 | 2 | TInfo4 | |
| 104 | 1 | Comments | count of 64-byte comment lines preceding the record |
| 105 | 1 | TFlags | e.g. ANSiFlags: non-blink/iCE color, letter-spacing, aspect ratio |
| 106 | 22 | TInfoS | null-terminated string, e.g. font name like `"IBM VGA"` |

**Comment block:** if Comments = N (>0), the N×64-byte comment lines sit immediately before the record, prefixed by the 5-byte ID `"COMNT"`. A file with SAUCE is often also terminated by an **EOF/Ctrl-Z (0x1A)** just before the comment block/record — legacy DOS convention; be tolerant of its presence or absence.

**Implementation note:** to detect SAUCE, read the final 128 bytes and check for `"SAUCE"` at offset 0. FileSize lets you recover the "art payload" length so you can strip metadata cleanly.

---

### FidoNet FTN (the big one for syndication)

**Addressing — 5D:** `zone:net/node.point@domain`. Point 0 = the main node (usually omitted). Domain is the FTN network name (`fidonet`, and "othernets" like `fsxnet`, `micronet`). Example: `1:234/5.6@fidonet`. A node with no point is a "boss node"; points route through their boss. Zone examples historically: 1=North America, 2=Europe, 3=Oceania, 4=Latin America.

**Two traffic types:**
- **Netmail** — private, point-to-point, addressed to a specific node. Routed hop-by-hop. Carries routing kludges.
- **Echomail** — public conference messages broadcast to all nodes carrying that "echo" (area). Distinguished by an **`AREA:<TAG>`** line as the very first line of the message body, plus **SEEN-BY** and **PATH** control lines at the end.

**Nodelist format** — a plaintext, comma-delimited file published weekly (`NODELIST.nnn`, nnn=day-of-year), distributed as a compressed diff (`NODEDIFF`). Line format:
```
Keyword,Number,Name,Location,SysopName,Phone,Speed,Flags...
```
Keyword is `Zone`/`Region`/`Host`/`Hub`/`Pvt`/`Down`/`Hold` or blank (a normal node). Hierarchy is positional: a `Zone` line opens a zone, `Host` lines open nets, subsequent blank-keyword lines are nodes in that net. First line is a header with a CRC-16. This is the "DNS" of FidoNet; we'd parse it into an address→endpoint table.

**PKT — the packet (verified: Type-2 header = 58 bytes, all multi-byte ints little-endian).**

*Packet header (FTS-0001 Type-2), 58 bytes:*

| Offset | Len | Field |
|---|---|---|
| 0 | 2 | origNode |
| 2 | 2 | destNode |
| 4 | 2 | year (e.g. 2026) |
| 6 | 2 | month (0–11 in FTS-0001; watch this) |
| 8 | 2 | day |
| 10 | 2 | hour |
| 12 | 2 | minute |
| 14 | 2 | second |
| 16 | 2 | baud |
| 18 | 2 | packet type = **2** |
| 20 | 2 | origNet |
| 22 | 2 | destNet |
| 24 | 1 | prodCode (low) |
| 25 | 1 | serialNo / prodCode-hi / revision |
| 26 | 8 | password (space/null padded) |
| 34 | 2 | origZone (qOrigZone) |
| 36 | 2 | destZone (qDestZone) |
| 38 | 20 | fill / reserved |

**Type-2+ (FSC-0039)** and **Type-2.2 (FSC-0048)** reuse the fill bytes to carry zone/point/domain and a **capability word** (offset 40, value `0x0001` with a byte-swapped copy at offset 26–27's region) so a reader can distinguish a plain type-2 from an extended one. Type-2+ adds `auxNet`, `origZone`/`destZone` (canonical placement), `origPoint`/`destPoint`, and `capabilWord`. **Practical rule:** parse as 2+; if the capability word/validation copy isn't present, fall back to plain type-2 and get zone/point from kludges instead.

*Packed message (follows the header; repeats until two 0x00 bytes where a messageType would be):*

| Offset | Len | Field |
|---|---|---|
| 0 | 2 | messageType = **2** |
| 2 | 2 | origNode |
| 4 | 2 | destNode |
| 6 | 2 | origNet |
| 8 | 2 | destNet |
| 10 | 2 | attributeWord (Private/Crash/Received/Sent/File/InTransit/Orphan/KillSent/Local/Hold/etc. bitflags) |
| 12 | 2 | cost |
| 14 | var | **DateTime** — null-terminated ASCII, 20 bytes incl. NUL, format `"01 Jan 26 12:34:56"` |
| … | var | **toUserName** — null-terminated, ≤36 |
| … | var | **fromUserName** — null-terminated, ≤36 |
| … | var | **subject** — null-terminated, ≤72 |
| … | var | **message body** — null-terminated text |

End of packet = a `0x0000` where the next messageType would begin.

**Body internal structure (this is where echomail lives):**
- **First line** of an echomail body: `AREA:TAGNAME<CR>` (no such line = netmail).
- **Kludge lines** — begin with `0x01` (Ctrl-A, "SOH") + text + `<CR>`; not displayed to users. Key ones:
  - `\x01INTL <destZone>:<destNet>/<destNode> <origZone>:<origNet>/<origNode>` — zone info for netmail.
  - `\x01FMPT <n>` / `\x01TOPT <n>` — origin/destination **points**.
  - `\x01MSGID: <origaddr> <serial-hex>` — globally unique message id (FTS-0009). **This is the primary dupe key.**
  - `\x01REPLY: <msgid>` — threading (reply-to).
  - `\x01PID:` / `\x01TID:` — producing program.
- **Origin line** (echomail): ` * Origin: <text> (<ftn-address>)` — last line of the human-visible body; the address here establishes the message's true origin for PATH building.
- **SEEN-BY:** lines (echomail, after origin): `SEEN-BY: 234/5 6 7 100/1` — net/node list of every system that has seen this message in this echo. **Suppression list** — you never send an echomail to a node already in SEEN-BY. You add yourself and your downlinks when tossing outbound.
- **PATH:** kludge (`\x01PATH:`) — the chain of nodes the message physically traversed; append your address on toss. Used for loop detection and topology debugging.

**Dupe detection:** primary key is **MSGID** (origaddr + serial); fallback is a hash over from/to/subject/date/body when MSGID absent. Tossers keep a rolling dupe database (per-area, time-windowed). SEEN-BY prevents re-broadcast to peers; the dupe DB catches messages that loop back via alternate paths.

**Bundling / ARCmail / mail bundles.** PKT files are not sent raw over the network for echomail; they're compressed into **mail bundles**. Naming convention encodes day-of-week and a sequence: `<hex-diff-of-net/node>.<ext>` where ext is `mo0`–`mo9`, `tu0`… (Mon–Sun) for **ARCmail** bundles (historically ARC-compressed, later ZIP but keeping the `.mo0` extension). **FLO/attach** files and control files: `.flo` (references files to send), `.pnt`, and `*.?ut`/`*.?lo` (out/flo) BSO — **BinkleyStyle Outbound** — where the extension's first char encodes flavor (c=crash, h=hold, d=direct, f/o=normal). The whole outbound directory scheme (BSO) is what mailers scan.

**Transport.** Historically modem sessions (EMSI handshake). Modern = **binkp** (FTS-1026), a simple framed TCP protocol on port 24554 (frames have a 2-byte big-endian length with high bit = command frame; commands: M_NUL, M_ADR, M_PWD, M_FILE, M_OK, M_GOT, M_EOB, etc.). Synchronet's **BinkIT** and ENiGMA½'s binkp are clean modern references.

**Tossers & scanners (the process model):**
- **Scanner** — reads your local message base, finds new outbound echomail, wraps into PKT, bundles/compresses, queues into BSO outbound for each uplink/downlink. (Also called the "packer.")
- **Tosser** — reads inbound bundles, unpacks, de-dupes, files each message into the right local area, updates SEEN-BY/PATH, and re-scans for onward routing to downlinks.
- **Mailer** — moves bundles over the wire (binkp).
- **AreaFix** — automation: a downlink sends a specially-addressed **netmail** (subject/body = password + commands like `+AREATAG`, `-AREATAG`, `%LIST`, `%QUERY`) to your system; your AreaFix processor edits that node's subscription list and replies by netmail. Essential for self-service echo subscription.

---

### QWK / QWKE offline mail (verified: 128-byte blocks; conf # at bytes 124–125; 5-byte NDX)

A **.QWK** file is just a ZIP (originally ARC/PAK) bundle. The BBS's QWK **door** packs unread messages; the user reads offline and packs replies into a **.REP**.

**.QWK contents:**
- **MESSAGES.DAT** — all message text + headers, laid out in **128-byte blocks**.
- **CONTROL.DAT** — ASCII, describes BBS + conferences.
- **`nnn.NDX`** — one per conference (nnn = zero-padded conf number), fast index into MESSAGES.DAT.
- **DOOR.ID** — ASCII, identifies the mail door and its capabilities (so the reader knows how to build the .REP).
- Optional: `NEWFILES.DAT`, `BLT-n.nn` bulletins, `SESSION.TXT`, welcome/news screens.

**MESSAGES.DAT layout.** First 128-byte block = a producer ID string, space-padded (e.g. `"Produced by ..."`). Then each message = **1 header block + N body blocks**, all 128 bytes.

*Header block (offsets are 1-based per the classic spec; subtract 1 for zero-based):*

| Bytes (1-based) | Len | Field |
|---|---|---|
| 1 | 1 | **Status flag** (ASCII): `' '`=public unread, `'-'`=public read, `'*'`=private unread (to sysop/others), `'+'`=private read, `'~'`=comment to sysop, etc. |
| 2–8 | 7 | Message number (ASCII, right-space-padded) |
| 9–16 | 8 | Date `MM-DD-YY` |
| 17–21 | 5 | Time `HH:MM` |
| 22–46 | 25 | **To** (uppercase, space-padded) |
| 47–71 | 25 | **From** |
| 72–96 | 25 | **Subject** |
| 97–108 | 12 | Password |
| 109–116 | 8 | Reference (reply-to) message number (ASCII) |
| 117–122 | 6 | **Number of 128-byte blocks** including this header block (ASCII; so body = count−1 blocks) |
| 123 | 1 | Active flag: `0xE1`(225)=active, `0xE2`(226)=killed/inactive |
| 124 | 1 | Conference number, low byte |
| 125 | 1 | Conference number, high byte |
| 126–128 | 3 | Logical message number in packet / filler |

**Body encoding gotcha:** message text lines in MESSAGES.DAT are **not** terminated by CR/LF; the end-of-line marker is byte **`0xE3` (227)** ("π" pi character in CP437). Body is space-padded out to the last 128-byte block. Readers replace 0xE3 with newline on display and back on pack.

**.NDX record — 5 bytes each:**
- Bytes 1–4: message pointer = the **1-based block number** of the message header in MESSAGES.DAT, stored as a **Microsoft MKS$ / MS Binary Format float** (not IEEE 754! — 4-byte MBF). This is a genuine landmine: you must implement MBF↔integer conversion.
- Byte 5: conference number (low byte).

**CONTROL.DAT (ASCII, line-oriented):**
```
Line 1:  BBS name
Line 2:  BBS city/state
Line 3:  BBS phone
Line 4:  Sysop name
Line 5:  Mail-door serial#,BBSID
Line 6:  Packet creation date/time
Line 7:  User name (uppercase)
Line 8:  (blank / menu name)
Line 9:  0
Line 10: total messages in packet
Line 11: total conferences MINUS 1 (highest conf index)
Then, repeating per conference: conf number, then conf name (two lines each)
Then: welcome/news/goodbye screen filenames
```

**DOOR.ID** — key/value-ish ASCII lines: `DOOR = <mailer name>`, `VERSION =`, `SYSTEM =`, `CONTROLNAME =`, and **`CONTROLTYPE = ADD`/`NET`** plus feature flags telling the reader which extensions (e.g. QWKE) are supported.

**.REP reply packet.** A ZIP named `<BBSID>.REP` containing a single **`<BBSID>.MSG`** file (same 128-block MESSAGES.DAT format). The first block is the producer/BBSID line; each reply's header uses conference number to route to the right area. The BBS's door reads .REP on upload and posts.

**QWKE extensions** (backward-compatible): the tiny 25-char To/From/Subject fields are the pain point. QWKE adds variable-length To/From/Subject and long conf names via extra kludge lines embedded at the start of the message body (`To:`, `From:`, `Subject:` lines) and signals support in DOOR.ID. **QWKnet** (networking over QWK) uses special conferences and `@` routing lines to relay between BBSes offline — a poor-man's FidoNet.

**lastread / pointers.** The reader tracks last-read message per conference (`*.PTR`/`DOORID.NET` conventions, or the door tracks it server-side). On next pack, only messages after the pointer are included. We must model per-user, per-area high-water read marks — the same concept NNTP calls the newsrc.

---

### NNTP / Usenet gateway

**Concepts.** NNTP (RFC 977, extended by RFC 3977; overview data via RFC 2980/OVER). Articles are RFC 822/1036/5536 (**Netnews**) messages: headers (`From`, `Newsgroups`, `Subject`, `Message-ID`, `References`, `Date`, `Path`, `Xref`) + body, terminated by a line with a single `.` (dot-stuffing: lines beginning with `.` get an extra `.`). Newsgroups map cleanly to BBS message areas; `References` gives threading; `Message-ID` is the dupe key (exactly analogous to FTN MSGID).

**How BBSes bridged to Usenet.** A gateway process:
1. **Inbound:** pulls articles for subscribed groups (NNTP `GROUP`/`OVER`/`ARTICLE`, or was fed via a news transit feed), strips/rewrites headers, and files each into the matching local area — including a synthetic FTN-style origin/tearline if it also lived in FidoNet.
2. **Outbound:** takes local posts, synthesizes RFC 5536 headers (generates a `Message-ID` in the gateway's domain, sets `Newsgroups`, `From`), and `POST`s upstream (or `IHAVE`/`TAKETHIS` for transit feeds).
3. **Loop/dupe control** relies on `Message-ID` history and the `Path:` header (the Usenet analog of FTN's PATH), so an article doesn't ping-pong across the gateway. **FidoNet↔Usenet gateways** were common and standardized address-mangling (FTN `1:234/5.6` ↔ `f6.n5.z1.fidonet.org` style) and had to reconcile FTN's `AREA:`/SEEN-BY dupe model with Usenet's `Message-ID`/`Path` model — a genuine two-way translation problem.

Synchronet runs a **native NNTP server** so external newsreaders read the BBS's areas directly, and can also act as a client-side gateway — the cleanest modern reference for us.

---

## Pitfalls & Lessons

- **Endianness & integer widths.** All FTN PKT integers are little-endian 16-bit. Off-by-one on the 58-byte header or the type-2/2+ ambiguity silently corrupts everything downstream.
- **Type-2 vs 2+ ambiguity.** There is no clean version byte; you disambiguate via the capability word and its validation copy. Parse defensively and cross-check against kludge lines (INTL/FMPT/TOPT) for zone/point.
- **MS Binary Format floats in .NDX.** A non-IEEE 4-byte float storing an integer block pointer. Easy to get wrong; some implementations just ignore .NDX and rescan MESSAGES.DAT. We should implement MBF but not *depend* on the index being correct (many doors wrote buggy NDX files).
- **Non-obvious line terminators.** QWK bodies use `0xE3` (π), not CRLF. FTN kludges use leading `0x01` and `<CR>` (0x0D) only, no LF. CP437 art uses bare CR or CRLF inconsistently. Normalize on ingest, re-emit exactly on egress.
- **CP437 ↔ Unicode round-tripping.** The 0x00–0x1F "control range has glyphs" issue and NUL/space padding conventions bite. Keep raw bytes alongside decoded text so you can re-serialize losslessly.
- **Dupe detection is a correctness feature, not an optimization.** Without a solid MSGID/Message-ID history + SEEN-BY/PATH handling, an echo will storm the network. This is the classic way a new FTN node gets excommunicated. Budget real engineering here.
- **SEEN-BY/PATH must be maintained precisely.** Adding your address on toss, honoring the suppression list on scan, and never mangling foreign SEEN-BY entries. Bugs here cause both loops and message loss.
- **Timestamps.** FTS-0001 month field is 0-based in some readers, 1-based in others; the ASCII DateTime string (`"01 Jan 26 ..."`) is the authoritative one. QWK dates are `MM-DD-YY` (Y2K-ambiguous — assume windowing). Store canonical UTC internally.
- **Character-count fields lie.** QWK "number of blocks" counts the header block; To/From/Subject are hard-truncated to 25 chars (hence QWKE). Don't trust field lengths blindly.
- **Door socket handoff** is the hardest interop point (inherited fd vs. WSADuplicateSocket vs. telnet bridge). Pick the telnet/pty-bridge model for portability and safety.
- **Bundling naming collisions.** `.mo0`–`.su9` day-coded extensions wrap; two bundles same day/second can collide. Real tossers handle rename/merge.
- **Nodelist is huge and diff-distributed.** Don't re-download; apply NODEDIFF. Validate the CRC-16 header line.
- **Security-level model is coarse.** A single 0–255 SL + flags is easy but inexpressive; teams regretted hardcoding it. Add a proper capability/role layer while keeping an SL projection for door/legacy compatibility.

---

## Implications for RabbitHole

1. **Canonical internal message model, syndication adapters at the edges.** Define one internal `Message` (stable UUID + FTN-MSGID + Usenet Message-ID + Fido/RFC addresses + area + references + raw-bytes + decoded-text + attributes). Implement **FTN, QWK, and NNTP as encoders/decoders** over it. This mirrors what all five reference systems do (native store + separate tosser/scanner/gateway) and lets us syndicate the *same* content across all three networks.

2. **Model the process pipeline explicitly** as async Rust services: `scanner` (base→PKT→bundle→BSO), `tosser` (bundle→dedupe→base→re-route), `mailer` (binkp TCP), `qwk-door` (pack/unpack .QWK/.REP), `nntp-gateway`, plus native `nntp/smtp` servers. Rust's async + channels make multi-node chat and inter-process IPC far cleaner than the shared-file/shmem hacks of the originals.

3. **Get the byte-level codecs right and test them against real data.** Build fuzz/round-trip tests using actual Synchronet/Mystic-generated PKT bundles and QWK packets (both are freely producible). Round-trip = decode→encode must be byte-identical for the fields we own. Reuse the verified layouts above (PKT 58-byte header LE; packed-message null-terminated strings; QWK 128-byte blocks with 0xE3 EOL, conf at 124–125; 5-byte MBF NDX; 128-byte SAUCE).

4. **A first-class dupe/loop subsystem.** Shared MSGID/Message-ID history store with time windowing, plus correct SEEN-BY/PATH and Usenet Path handling. Treat it as core infrastructure shared by all adapters, since FTN↔Usenet gatewaying needs both dupe models reconciled.

5. **Preserve raw bytes for lossless CP437/kludge round-tripping.** Store original encoded body + decoded Unicode. Ship a CP437↔Unicode table, an ANSI/CSI renderer (SGR + cursor positioning + iCE/blink handling + Avatar RLE optionally), and a SAUCE reader/writer.

6. **Legacy-compatible auth as a projection.** Internal RBAC/capabilities, but expose a derived security-level integer + flags so DOOR32.SYS dropfiles and legacy door games get sane values.

7. **Door support via a telnet/PTY bridge**, emitting DOOR32.SYS (and DOOR.SYS/DORINFO1.DEF for old games). Avoid raw fd inheritance except as an optimization.

8. **Reference implementations to read, in order:** ENiGMA½ (modern JS FTN/QWK — closest to what we're writing), Synchronet (the format bible + BinkIT/SBBSecho + native NNTP), Mystic (UX/theming + MUTIL tosser behavior), WWIV (simpler non-FTN network design), Citadel (room/threading semantics + modern IMAP/SMTP bridging).

---

**Verification note:** I confirmed the load-bearing layouts against sources — FTS-0001 / Synchronet fidonet_packets (Type-2 header = 58 bytes, little-endian; packed-message field order), the QWK spec (128-byte blocks, conference # at bytes 124–125, 5-byte NDX record with MBF pointer), and SAUCE rev 5 (128-byte record layout). Several primary spec mirrors (Synchronet wiki, ftsc.org, wmcbrine, textfiles) returned **HTTP 403 policy denials from the outbound gateway**, so the finer per-field details above (exact QWK offsets, kludge semantics, binkp framing, NDX MBF) are drawn from my knowledge of the specs rather than freshly re-fetched — they're accurate but worth a spot-check against the primary docs when you implement.

**Sources:**
- [FTS-0001 (FTSC)](http://ftsc.org/docs/fts-0001.016)
- [FidoNet Packets — Synchronet wiki](http://wiki.synchro.net/ref:fidonet_packets)
- [FidoNet message packet — Just Solve the File Format Problem](http://fileformats.archiveteam.org/wiki/FidoNet_message_packet)
- [QWK Format — Synchronet wiki](https://wiki.synchro.net/ref:qwk)
- [The Mysterious QWK-File Format — wmcbrine.com](https://wmcbrine.com/mmail/specs/qwkfoy.html)
- [QWK Mail Packet File Layout (Patrick Y. Lee) — textfiles.com](http://textfiles.com/programming/qwk.txt)
- [QWK — Just Solve the File Format Problem](http://justsolve.archiveteam.org/wiki/QWK)
- [SAUCE rev 5 — acid.org](https://www.acid.org/info/sauce/sauce.htm)
- [binkp protocol — FTS-1026](http://ftsc.org/docs/fts-1026.001)
