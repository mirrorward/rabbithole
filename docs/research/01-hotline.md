# Hotline Communications — Technical Research Brief for RabbitHole

## Overview

Hotline (Hotline Communications Ltd., 1996–2001, originally Mac-only then Windows) is a client/server platform combining chat, private messaging, threaded news/message boards, and a file-transfer/browser system, plus a global **Tracker** for server discovery. It defined the "warez + community" BBS-successor aesthetic that RabbitHole targets.

Three network endpoints matter:
1. **Hotline server** — control channel on a base TCP port (default **5500**). Carries all "transactions."
2. **File-transfer channel** — **base port + 1** (default 5501). A separate short-lived TCP connection per transfer.
3. **Tracker** — separate server, default TCP **5498**, that lists registered servers.

Everything after the handshake is a **transaction**: a 20-byte header plus an optional parameter list of typed **fields**. Requests carry a client-chosen ID; replies echo it. Servers can also initiate transactions (push notifications: chat, user joins, IMs).

Best modern implementable references:
- **mierau/hotline** wiki (clean, byte-accurate client protocol + tracker docs).
- **jhalter/mobius** — a complete Go Hotline server; canonical source for exact transaction-type numbers, field IDs, and access bits (values quoted below are from its source).
- **Virtual1's Hotline Protocol v1.1.1** (the original reverse-engineered spec) — authoritative for the obfuscation and encoding quirks.

---

## Key Features (concrete)

- **Single TCP handshake** with 4-byte magic `TRTP` and a version short; trivially cheap to negotiate.
- **Uniform transaction model** — one framing for login, chat, files, admin, news. Add a feature = add a transaction type + fields, no new framing.
- **Typed field/parameter bag** per transaction (like a flat TLV map keyed by 16-bit field IDs). Forward-compatible: unknown fields are skippable.
- **Public chat** (broadcast) + **private chat rooms** (invite/join/leave, per-room subject) + **1:1 instant messages** + **user-to-user messages** with quoting and auto-response.
- **User list** with live join/change/leave notifications; each user has a name (nickname), 16-bit user ID, icon ID, and a flags word.
- **64-bit access bitmask** per account — fine-grained, ~41 named privileges (file ops, chat, news, admin, drop boxes, aliases, broadcast, "cannot be disconnected").
- **File browser** over transactions (list/info/comment/rename/move/delete/new folder/alias) with **actual bytes** moving over the separate transfer port.
- **File transfer with forks + resume** — Mac-origin "flattened file object" preserving data fork, resource fork, type/creator codes, dates, comment; resume via fork-offset resume data.
- **Folder download/upload** — recursive, item-by-item action negotiation (send/resume/skip).
- **Drop boxes** — write-only upload folders; contents hidden unless you hold the view-dropbox privilege.
- **Two news systems**: legacy **flat message board** (one growing text blob) and later **threaded news** (nested categories/bundles → articles with parent/child/prev/next links).
- **Server agreement + banner** shown at login; icons/avatars by numeric ID (client-side art).
- **Tracker** for discovery: servers register (heartbeat), clients pull a flat server list.

---

## Technical Notes (protocol / data-model specifics)

### 1. Handshake (base port)

Client → Server, **Client Hello (12 bytes)**:
```
'TRTP'  (4)  0x54525450
SubProtocolID (4)   // "HOTL" for Hotline; user-defined
Version (2)         // = 1
SubVersion (2)      // user-defined
```
> Note: the on-the-wire client hello is often described as the 8 bytes `TRTP` + `HOTL` followed by the two version shorts; combined this is the "TRTPHOTL…" signature clients send.

Server → Client, **Server Hello (8 bytes)**:
```
'TRTP'  (4)
ErrorCode (4)   // 0 = OK; non-zero => server closes connection
```
No further data on error; the socket is dropped. Version negotiation is minimal — the client advertises version, the server accepts or rejects.

### 2. Transaction frame (20-byte header, big-endian)

```
Offset Size Field
0      1    Flags        // reserved, send 0
1      1    IsReply      // 0 = request, 1 = reply
2      2    Type         // transaction type (operation code)
4      4    ID           // client-chosen, non-zero; reply echoes it
8      4    ErrorCode    // meaningful in replies (0 = success)
12     4    TotalSize    // total data across all fragments
16     4    DataSize     // data length in THIS message
```
`TotalSize` vs `DataSize` supports fragmentation of large payloads (rarely needed for control traffic; large data goes over the transfer port instead).

**Parameter list** (the "data" that follows the header):
```
2 bytes  ParameterCount
repeat ParameterCount times:
  2 bytes  FieldID
  2 bytes  FieldSize
  N bytes  FieldData
```
Field data types: **unsigned integer** (encoded as 16-bit if it fits, else 32-bit), **string** (8-bit ASCII/MacRoman), **binary**. Big-endian throughout.

**Request/reply/server-push semantics:**
- Request: `IsReply=0`, unique non-zero `ID`. Reply: `IsReply=1`, same `ID`, `ErrorCode` set.
- Server-initiated (push): server sends a transaction the client didn't request (e.g. `TranChatMsg`, `TranNotifyChangeUser`, `TranServerMsg`). These may use `ID=0` or a server-chosen ID and expect no reply.

### 3. Transaction type numbers (from mobius; big-endian shorts)

| # | Name | Purpose |
|---|------|---------|
| 0 | Error | error carrier |
| 101 | GetMsgs | read flat message board |
| 102 | NewMsg | post to flat board / new-msg push |
| 103 | OldPostNews | legacy post |
| 104 | ServerMsg | server → user message/notice |
| 105 | ChatSend | send public chat |
| 106 | ChatMsg | chat message push to clients |
| 107 | **Login** | login (login, pass, name, icon, version) |
| 108 | SendInstantMsg | 1:1 IM / user message |
| 109 | ShowAgreement | server pushes agreement text |
| 110 | DisconnectUser | admin kick |
| 111 | DisconnectMsg | disconnect notice |
| 112 | InviteNewChat | start private chat, invite users |
| 113 | InviteToChat | invite to existing room |
| 114 | RejectChatInvite | decline |
| 115 | JoinChat | join room |
| 116 | LeaveChat | leave room |
| 117 | NotifyChatChangeUser | room roster change push |
| 118 | NotifyChatDeleteUser | room leave push |
| 119 | NotifyChatSubject | subject changed push |
| 120 | SetChatSubject | set room subject |
| 121 | **Agreed** | client accepted agreement (sends name/icon/flags) |
| 122 | ServerBanner | banner fetch |
| 200 | GetFileNameList | list directory |
| 202 | DownloadFile | request download → get ref# |
| 203 | UploadFile | request upload → get ref# |
| 204 | DeleteFile | delete |
| 205 | NewFolder | mkdir |
| 206 | GetFileInfo | file/folder info |
| 207 | SetFileInfo | rename/comment |
| 208 | MoveFile | move |
| 209 | MakeFileAlias | alias/symlink |
| 210 | DownloadFldr | recursive folder download |
| 211 | DownloadInfo | transfer progress info |
| 212 | DownloadBanner | banner |
| 213 | UploadFldr | recursive folder upload |
| 300 | GetUserNameList | fetch user list |
| 301 | NotifyChangeUser | user joined/changed push |
| 302 | NotifyDeleteUser | user left push |
| 303 | GetClientInfoText | "get info" on a user |
| 304 | SetClientUserInfo | set own name/icon |
| 348 | ListUsers | admin: list accounts |
| 349 | UpdateUser | admin: bulk account update |
| 350 | NewUser | admin: create account |
| 351 | DeleteUser | admin: delete account |
| 352 | GetUser | admin: read account |
| 353 | SetUser | admin: write account |
| 354 | UserAccess | server pushes your access mask |
| 355 | UserBroadcast | admin broadcast |
| 370 | GetNewsCatNameList | threaded news: list categories |
| 371 | GetNewsArtNameList | list articles in category |
| 380 | DelNewsItem | delete category/bundle |
| 381 | NewNewsFldr | create news bundle |
| 382 | NewNewsCat | create news category |
| 400 | GetNewsArtData | fetch article body |
| 410 | PostNewsArt | post article |
| 411 | DelNewsArt | delete article |
| 500 | KeepAlive | ping |

### 4. Field IDs (from mobius)

Core: `Error=100, Data=101, UserName=102, UserID=103, UserIconID=104, UserLogin=105, UserPassword=106, RefNum=107, TransferSize=108, ChatOptions=109, UserAccess=110, UserFlags=112, Options=113, ChatID=114, ChatSubject=115, WaitingCount=116`.

Server/agreement/banner: `BannerType=152, NoServerAgreement=152, Version=160, CommunityBannerID=161, ServerName=162`.

Files: `FileNameWithInfo=200, FileName=201, FilePath=202, FileResumeData=203, FileTransferOptions=204, FileTypeString=205, FileCreatorString=206, FileSize=207, FileCreateDate=208, FileModifyDate=209, FileComment=210, FileNewName=211, FileNewPath=212, FileType=213, QuotingMsg=214, AutomaticResponse=215, FolderItemCount=220`.

Users/news: `UsernameWithInfo=300, NewsArtListData=321, NewsCatName=322, NewsCatListData15=323, NewsPath=325, NewsArtID=326, NewsArtDataFlav=327, NewsArtTitle=328, NewsArtPoster=329, NewsArtDate=330, NewsArtPrevArt=331, NewsArtNextArt=332, NewsArtData=333, NewsArtParentArt=335, NewsArt1stChildArt=336, NewsArtRecurseDel=337`.

### 5. Login flow (concrete)

1. Client → `Login (107)` with fields: `UserLogin (105)`, `UserPassword (106)`, `UserName/nick (102)`, `UserIconID (104)`, `Version (160)`.
   - **`UserLogin` and `UserPassword` are obfuscated**: each byte `b → 255 - b` (equivalently XOR 0xFF). This is *not* encryption — plaintext-equivalent. (Empty password is often the single byte or empty; guest = empty login.)
2. Client optimistically fires `ShowAgreement (109)` request, `GetUserNameList (300)`, and news requests **before** the login reply arrives (classic client behavior — pipelining).
3. Server replies to `Login`, pushes the agreement text.
4. Client → `Agreed (121)` carrying `UserName (102)`, `UserIconID (104)`, `Options/UserFlags`. For v1.5+ servers the client may instead send `SetClientUserInfo (304)` with just name + icon and expect no reply.
5. Server pushes `UserAccess (354)` with the 8-byte access mask (field 110) and begins sending `NotifyChangeUser (301)` for the roster.

### 6. Access privilege model (64-bit bitmask = 8-byte array)

Bit ordering is **big-endian across the byte array**: bit *i* lives at `bytes[i/8]`, mask `1 << (7 - i%8)`. Named bits (mobius, `AccessX = bit index`):

```
0 DeleteFile        1 UploadFile       2 DownloadFile     3 RenameFile
4 MoveFile          5 CreateFolder     6 DeleteFolder     7 RenameFolder
8 MoveFolder        9 ReadChat        10 SendChat        11 OpenChat
12 CloseChat(unused)13 ShowInList(un.) 14 CreateUser      15 DeleteUser
16 OpenUser         17 ModifyUser      18 ChangeOwnPass(un.)  20 NewsReadArt
21 NewsPostArt      22 DisconUser      23 CannotBeDiscon  24 GetClientInfo
25 UploadAnywhere   26 AnyName         27 NoAgreement     28 SetFileComment
29 SetFolderComment 30 ViewDropBoxes   31 MakeAlias       32 Broadcast
33 NewsDeleteArt    34 NewsCreateCat   35 NewsDeleteCat   36 NewsCreateFldr
37 NewsDeleteFldr   38 UploadFolder    39 DownloadFolder  40 SendPrivMsg
```
(Original spec: 64 flags reserved, ~27 used in classic era; later servers extended to ~41.)

### 7. Chat & messaging

- **Public chat**: `ChatSend (105)` with `Data (101)` → server broadcasts `ChatMsg (106)` (with `Data`, `ChatID=0` for public). `ChatOptions (109)` toggles emote/style.
- **Private rooms**: `InviteNewChat (112)` (list of user IDs) → server returns `ChatID (114)`; invitees get `InviteToChat (113)`; `JoinChat (115)`/`LeaveChat (116)`; roster pushes `NotifyChatChangeUser/DeleteUser (117/118)`; subject via `SetChatSubject (120)` → `NotifyChatSubject (119)`.
- **IM / user message**: `SendInstantMsg (108)` with target `UserID (103)`, `Data (101)`, optional `QuotingMsg (214)`, and `AutomaticResponse (215)` for away replies.
- **KeepAlive (500)** ping to hold idle connections.

### 8. File browser & transfer

**Browsing** is pure transactions: `GetFileNameList (200)` with `FilePath (202)` returns repeated `FileNameWithInfo (200)` records (type/creator, size, dates, name). `GetFileInfo (206)`, `SetFileInfo (207)` (rename via `FileNewName 211`, comment via `FileComment 210`), `MoveFile (208)`, `DeleteFile (204)`, `NewFolder (205)`, `MakeFileAlias (209)`.

**Transfer setup**: client sends `DownloadFile (202)` / `UploadFile (203)` (with `FileName`, `FilePath`, optionally `FileResumeData 203`, `FileTransferOptions 204`). Server replies with a **`RefNum (107)`** and `TransferSize (108)` / `FileSize (207)`.

**Transfer channel** (base port+1): client opens a new TCP connection and sends a **16-byte HTXF header**:
```
'HTXF' (4)  0x48545846
ReferenceNumber (4)   // == RefNum from the setup reply; matches transfer to session
DataSize (4)          // file/transfer size
RSVD (4)              // reserved, 0
```
Then bytes flow. Downloads stream a **Flattened File Object** (below); uploads send one from the client.

**Flattened File Object (FFO)** — Mac fork-preserving container:
```
FlatFileHeader (24 bytes):
  'FILP' (4) | Version 0x0001 (2) | Reserved (16 zero) | ForkCount (2)  // 2 or 3

For each fork — Fork Header (16 bytes):
  ForkType (4)  'INFO' | 'DATA' | 'MACR(resource)'
  CompressionType (4)
  Reserved (4)
  DataSize (4)

INFO fork payload (72+ bytes):
  Platform (4)  'AMAC' or 'MWIN'
  TypeSignature (4)      // e.g. 'TEXT'
  CreatorSignature (4)   // e.g. 'ttxt'
  Flags (4) | PlatformFlags (4) | Reserved (32)
  CreateDate (8) | ModifyDate (8)
  NameScript (2) | NameSize (2) | Name (var, ≤128)
  CommentSize (2) | Comment (var)
DATA fork payload = raw file bytes
MACR fork payload = Mac resource fork (omit for cross-platform)
```

**Resume**: `FileResumeData (203)` carries a fork-offset list (`ForkInfoList` with per-fork `DataSize`/offset). Server resumes each fork from the given offset. In-progress uploads are written to `<name>.incomplete` then renamed on completion.

**Folder download** (`DownloadFldr 210`): after HTXF connect, per item the server sends a FileHeader `Size(2)|Type(2, 0x0001=dir)|encoded path`, then waits 2 bytes from client: `1`=SendFile, `2`=ResumeFile (+resume data), `3`=NextFile(skip). On send: `FileSize(4)` + FFO + data fork + resource fork.

**Folder upload** (`UploadFldr 213`): server drives with `NextFile [00 03]`; client sends `DataSize(2)|IsFolder(2)|PathItemCount(2)|path segments`. Each path segment: `2 reserved | 1 length | N bytes`. Server answers `1`/`2`/`3` (receive/resume/skip).

**Drop boxes**: ordinary folders where listing is suppressed unless the caller has `ViewDropBoxes (bit 30)`; uploads allowed, downloads/listing denied otherwise.

### 9. News systems

**Legacy flat board**: `GetMsgs (101)` returns one big text blob (`Data 101`); `NewMsg/OldPostNews (102/103)` prepend a post. No structure beyond concatenated text separated by delimiters.

**Threaded news** (later): hierarchy of `NewsCategoryListData15` nodes.
- Node `Type`: `NewsBundle = {0,2}` (folder, no articles) vs `NewsCategory = {0,3}` (holds articles + subcats).
- `GetNewsCatNameList (370)` with `NewsPath (325)` → category records (`NewsCatListData15 323`, `NewsCatName 322`).
- `GetNewsArtNameList (371)` → `NewsArtListData (321)`: per article ID, date, title, poster, size, and **thread links** (`ParentArt 335`, `FirstChildArt 336`, `PrevArt 331`, `NextArt 332`).
- `GetNewsArtData (400)` → body (`NewsArtData 333`), flavor `NewsArtDataFlav (327)` = `"text/plain"` (flavor count `{0,1}`).
- `PostNewsArt (410)` (title/poster/parent), `DelNewsArt (411)` (with recurse flag 337), `NewNewsCat (382)`, `NewNewsFldr (381)`, `DelNewsItem (380)`.

### 10. Agreement, banner, icons, user list

- **Agreement**: server pushes `ShowAgreement (109)` text at login unless account has `NoAgreement (bit 27)` / field `NoServerAgreement (152)`. Client must `Agreed (121)` before full access.
- **Banner**: `ServerBanner (122)` / `DownloadBanner (212)`; `BannerType (152)`, `CommunityBannerID (161)` — image fetched, often over the transfer port.
- **Icons/avatars**: numeric `UserIconID (104)`; the artwork is client-side (a resource set), only the ID travels. Nickname = `UserName (102)`.
- **User list**: `GetUserNameList (300)` returns `UsernameWithInfo (300)` records = `UserID(2)|IconID(2)|Flags(2)|NameLen(2)|Name`. Live deltas via `NotifyChangeUser (301)` / `NotifyDeleteUser (302)`. `UserFlags (112)` encodes away/admin/refusing-PM/refusing-chat bits.

### 11. Admin

- **Kick**: `DisconnectUser (110)` with target `UserID`; blocked if target has `CannotBeDiscon (bit 23)`.
- **Ban**: typically kick + persistent ban list keyed by IP/account (implementation-specific; classic servers stored banned addresses server-side).
- **Accounts**: `ListUsers (348)`, `GetUser (352)`, `NewUser (350)`, `SetUser (353)`, `DeleteUser (351)`, bulk `UpdateUser (349)`; account record = login + obfuscated password + name + 8-byte access mask.
- **Broadcast**: `UserBroadcast (355)` / `ServerMsg (104)`.

### 12. Tracker protocol (default port 5498)

**Client → tracker** (list request):
```
Magic 'HTRK' 0x4854524B (4)
Version (2)   0x0001 (legacy) | 0x0002 (with auth)
[LoginSize(1)|Login(var)|PasswordSize(1)|Password(var)]  // v2 only
```
**Tracker → client** (listing):
```
Magic 'HTRK' (4)
Version (2)
MessageType (2)  0x0001
MessageSize (2)
ServerCount (2)
ServerCount (2)  // repeated
then ServerCount records:
  IPAddress (4)
  Port (2)
  UserCount (2)
  Unused (2)
  NameSize (1) | Name (var)
  DescSize (1) | Description (var)
```
Connection closes after the full list. **Registration** (server → tracker, UDP in classic era) is a periodic heartbeat carrying the server's port, name, description, and user count so the tracker keeps the entry alive; entries expire without heartbeats.

---

## Pitfalls & Lessons

- **"Encryption" is fake.** Login/password use `255 - byte` obfuscation, and traffic is plaintext. Any modern revival must add TLS. If you want protocol compatibility with legacy clients you cannot — so plan a native secure transport and treat legacy compat as a separate, clearly-insecure mode.
- **Field ID collisions/overloads.** `152` means both `BannerType` and `NoServerAgreement`; several fields are context-dependent. Don't assume field IDs are globally unique in meaning — interpret per transaction type.
- **Integer width is value-dependent.** Integers are 16-bit "if they fit," else 32-bit, and `FieldSize` tells you which. Parse by size, not by assumed type. This bites naive decoders.
- **Optimistic pipelining at login.** Real clients send agreement/userlist/news requests before the login reply. A strict request-then-reply server state machine will deadlock or reject valid clients. Handle out-of-order / early requests.
- **Server-initiated transactions share the same channel and framing** as replies. If you key handlers only on "did I send a request with this ID," you'll drop pushes. Route by `IsReply` + `Type`.
- **Mac fork baggage.** The FFO carries resource forks, type/creator codes, MacRoman name script, and Mac epoch dates. Cross-platform files need `MWIN` platform and no MACR fork. Getting dates/encodings wrong corrupts metadata silently.
- **Two-connection transfer model** is stateful and fragile: the RefNum ties the HTXF socket to the setup transaction, and it's a small window. Timeouts, NAT, and port+1 firewalling were constant real-world problems.
- **Folder transfer is a chatty per-item lockstep** (send/resume/skip handshakes). It's latency-bound and hard to pipeline; large trees are slow.
- **Two incompatible news systems** coexisted (flat blob vs threaded). Clients had to detect server capability. Don't ship both as first-class; pick threaded and offer a flat view over it.
- **Access bit ordering is big-endian within a byte array** (`1 << (7 - i%8)`) — easy to implement backwards and silently grant/deny wrong privileges.
- **Tracker duplicate ServerCount field** and 1-byte length prefixes cap names/descriptions at 255 bytes and were a known parsing gotcha.

---

## Implications for RabbitHole

1. **Adopt the transaction+field model — it's the right core.** A 20-byte header (flags, is-reply, type, id, error, total-size, data-size) plus a TLV field bag is simple, extensible, and battle-tested. Reuse the *shape*; modernize the *encoding*.
2. **Modernize the wire format.** Keep big-endian binary framing but consider: (a) mandatory TLS 1.3; (b) typed, self-describing fields (explicit type tag alongside size, not "16-or-32 by size"); (c) 64-bit sizes for large files; (d) UTF-8 strings with explicit length, dropping MacRoman/script codes. You can keep numeric type/field IDs for compactness.
3. **Keep the RefNum-based side-channel for bulk transfers** but make it robust: single multiplexed connection (or QUIC streams) instead of a fragile port+1 second socket; carry the transfer token in-band. This removes the biggest legacy operational pain (firewall/NAT/timeout on port+1).
4. **Reimplement the 64-bit access bitmask** — it's an excellent, compact RBAC primitive. Preserve the named-privilege granularity (file/chat/news/admin/dropbox/alias/broadcast/cannot-be-kicked). Define bit ordering unambiguously (LSB-first recommended for Rust) and document it once.
5. **Ship threaded news only**, with parent/first-child/prev/next links, MIME-typed article bodies (you can allow markdown, not just text/plain), and categories vs bundles. Provide a flat "recent posts" view as a query, not a second data model.
6. **Design the server to accept pipelined/early requests** and to freely push server-initiated transactions; route handlers by (is-reply, type), not by outstanding-request ID alone. This matches how real clients behaved and gives you clean pub/sub for chat, roster, and IMs.
7. **Rebuild the Tracker as a first-class service** with authenticated heartbeat registration, expiry, and a paginated listing (drop the 1-byte length caps; use length-prefixed UTF-8; include TLS fingerprint / server pubkey so clients can verify). This is a natural place to add a modern directory/search.
8. **Preserve the culture-defining features**: numeric icon/avatar IDs (but allow client-supplied images with a hash), server agreement gate, banner, nickname, live user list, private chat rooms with subjects, drop boxes, and file comments. These are cheap and are exactly the nostalgia surface RabbitHole is selling.
9. **Reference implementations to mine directly**: `jhalter/mobius` (Go server — copy the constant tables and transfer state machine logic), `mierau/hotline` (client + wiki — cleanest byte layouts), and Virtual1's spec (the quirks: obfuscation, encoding, forks).

**Key source files/URLs for implementers:**
- Hotline Client Protocol: `https://github.com/mierau/hotline/wiki/Hotline-Client-Protocol`
- Hotline Tracker Protocol: `https://github.com/mierau/hotline/wiki/Hotline-Tracker-Protocol`
- mobius server source (constants): `https://github.com/jhalter/mobius` (`hotline/transaction.go`, `field.go`, `access.go`, `transfer.go`, `flattened_file_object.go`, `file_transfer.go`, `news.go`)
- Original spec: `https://codebox.org.uk/assets/documents/hotline/HLProtocol.pdf`
- Fandom protocol wiki: `https://hotline.fandom.com/wiki/Protocol`

Sources:
- [Hotline Client Protocol — mierau/hotline wiki](https://github.com/mierau/hotline/wiki/Hotline-Client-Protocol)
- [Hotline Tracker Protocol — mierau/hotline wiki](https://github.com/mierau/hotline/wiki/Hotline-Tracker-Protocol)
- [jhalter/mobius (Hotline server in Go)](https://github.com/jhalter/mobius)
- [Hotline Protocol v1.1.1 (Virtual1), PDF](https://codebox.org.uk/assets/documents/hotline/HLProtocol.pdf)
- [Protocol — Hotline Wiki (Fandom)](https://hotline.fandom.com/wiki/Protocol)
- [Hotline Protocol Documentation — preterhuman wiki](https://wiki.preterhuman.net/Hotline_Protocol_Documentation)

---

Note: web access was available. The numeric constants (transaction types, field IDs, access bits, FFO/HTXF layouts) were pulled directly from the mobius Go source and the mierau wiki and should be treated as canonical for a compatible implementation; a couple of legacy-era details (UDP tracker registration heartbeat, ban-list persistence) are implementation-defined and noted as such.
