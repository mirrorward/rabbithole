# Haxial KDX / HDX Research Brief — for RabbitHole

## Overview

**Haxial KDX** was a Hotline-lineage BBS/community server-and-client system built by **Haxial Software Pty. Ltd.** (Australia), developed roughly **2001–2005**, with the final release **1.600 (2004)**; the company shut down in **2009**. It is frequently described as "the last of the Hotline" — a spiritual successor created in the same tradition as Hotline (public chat + private messaging + news/message boards + file transfer + trackers), but with a **security-first redesign**: the entire protocol is encrypted, and permissions are organized around reusable **Account Classes** rather than per-user flag soup.

Note on naming: the mainline product is **KDX** (Client + Server). The "HDX" label in the request most likely conflates KDX with Hotline's own **HTXF** file-transfer channel / the general "Hotline-derivative" family; I found no separate Haxial product literally named "HDX." The Haxial suite around KDX included **RemoteAdminTool (RAT)**, **NetFone** (VoIP/Internet Telephone — voice chat was also folded into KDX), **DiskCatalog** (offline file-index search), **TextEdit** (free text editor), and **WorldClock**. All ran cross-platform on **Mac OS 9, Mac OS X, and Windows 95–XP**, which was itself a differentiator (Hotline was Mac-centric with a weaker Windows story).

Important sourcing caveat: this environment's egress policy blocked direct fetches of the primary sources (the hlwiki KDX Protocol page, the preterhuman/technowiki wikis, the official *KDX 1.520 Documentation* PDF, Fandom, and the ProfDrLuigi/Haxial-KDX GitHub repo were all 403/denied). The brief below is assembled from search-result excerpts of those pages plus domain knowledge of Hotline-era systems. **Wire-format specifics should be treated as "needs confirmation" and verified against the official 1.520 Documentation PDF and the hlwiki KDX Protocol page before you commit to a binary format.** Access those from an unrestricted network.

---

## Key Features (concrete)

**Chat & messaging**
- Public chat with **multiple chat rooms** per server (Hotline had a single main chat + ad-hoc private chats).
- **Private messaging** (persistent PM windows), broadcast/announcement messages from admins.
- **Voice chat / Internet Telephone** integrated (shared lineage with NetFone).
- **IRC bridging** was cited as a KDX capability — servers could relay a chat room to/from IRC.
- **Personalized identity**: nickname, custom **user icons**, full **alias** support.
- Client can hold **multiple simultaneous server connections** with window management, plus an **address book** of servers.

**News / message boards**
- Tree UI: server → newsgroups → messages (top pane = connected servers with expandable newsgroup list; bottom pane = messages in the selected newsgroup).
- **Post Message**, reply, and context-menu actions per message.
- **Self-delete grace**: a user can delete their *own* just-posted message even without the "Can Delete News Messages" privilege — ownership determined by matching the stored login.
- **Automatic news archiving**: server rolls old posts into text files under a `News Archives/` folder, sized by a configurable KB-per-archive threshold, then removes them from the live group.

**File sharing**
- File **and folder** transfer, **resumable transfers**, **automatic download sorting**, file browsing, **cached file lists**, and notably **extremely fast file searching** (server-side index).
- **Multiple base folders** and **special folders**; folder visibility/accessibility gated per Account Class ("Folder Access" can both hide *and* deny).
- **Trackers**: servers register with tracker(s); trackers support **categories** and even **accounts on the tracker** (more structure than Hotline's flat tracker list).

**Accounts, permissions & administration**
- **Accounts with Classes**: rights are assigned to a *class*; every member of the class inherits changes instantly. Privileges act as "master switches."
- Default visual convention: **black = Guest, blue = User, red = Administrator**.
- **Per-class flood/attack protection** (tunable strictness), **IP address restrictions**, **speed limiting** per class.
- Admin/ops surface: **Connection Monitor**, **Server History**, **User Info**, **Process Monitor**, **Broadcast Message**, **Server Icon**, **Change Own Password**.
- **Remote administration**: Remote Configuration, **Remote Upgrade** (push a new server build), Remote File Management, Remote Trash Emptying, **Remote Shutdown** — and via the companion **RemoteAdminTool**: view/control the remote display, list/kill/restart processes, launch programs, see per-disk free space, uptime, restart/shutdown. RAT is cross-platform (Mac can drive Windows and vice-versa) and encrypted.

**Security**
- **Entire protocol encrypted** end to end (this is the headline differentiator vs. Hotline).
- Reported crypto: a **mix of standard primitives — MD5, CRC32, Twofish** (128-bit block, 128–256-bit keys) — **plus proprietary Haxial algorithms**. (See Technical Notes for the caution here.)

---

## Technical Notes (protocol / security specifics)

- **Transport**: TCP, client/server. KDX Server is a documented target in port-forwarding guides (a dedicated service port range around the **3000s** is referenced, and Hotline-family designs typically split a control/session port from a separate file-transfer port). **Confirm exact default ports and whether transfer uses a second connection** against the official docs before implementing — the search evidence here is suggestive, not authoritative.
- **Encryption model**: the design intent was that *all* traffic (login, chat, news, file transfer, admin) is encrypted, unlike Hotline where the wire protocol was effectively cleartext framing and passwords were only trivially obfuscated. KDX combined Twofish for bulk encryption with MD5/CRC32 for hashing/integrity, layered over **undisclosed proprietary steps** (likely custom key exchange / handshake obfuscation).
  - **Lesson, not a template**: the proprietary layer means KDX's scheme is essentially *security-through-obscurity around* a good cipher, with an unknown (probably non-forward-secret, likely un-authenticated in the AEAD sense) handshake. For RabbitHole, treat Twofish/MD5/CRC32 as historical color, **not** as a spec to reproduce.
- **Message model**: like Hotline, KDX is a **transaction/request-reply** protocol with typed operations (login, get-file-list, download, post-news, send-chat, PM, admin ops). The client maintains multiple concurrent server sessions. News is server-persisted; chat is transient; files stream over a transfer channel with **resume** (byte-offset restart) support.
- **Server-side indexing**: "extremely fast file searching" and "cached file lists" imply the server maintains a persisted index of the shared tree, refreshed on change, rather than walking the filesystem per query.
- **Class-based authorization**: authorization decisions are resolved through the requester's Account Class at request time; folder ACLs support both *hide* and *deny* semantics distinctly.

---

## Pitfalls & Lessons

1. **Proprietary crypto is a dead end.** KDX's "our own algorithms mixed with Twofish/MD5" was impressive for 2002 but is exactly what you should *not* copy. It's unauditable, almost certainly lacks forward secrecy and modern authenticated encryption, and MD5/CRC32 are broken/inappropriate for integrity today.
2. **Obscurity killed longevity.** Closed source + closed protocol + a single vendor meant that when Haxial folded (2009), the ecosystem couldn't be maintained or extended. Contemporary revival relies on reverse-engineering (hlwiki, the ProfDrLuigi archive) rather than a spec.
3. **Feature sprawl vs. focus.** KDX bundled chat, news, files, trackers, VoIP, IRC bridge, remote desktop admin — powerful, but it also meant a large attack/maintenance surface and a steeper learning curve. It never reached Hotline's mindshare partly because it was *more* complex, not simpler.
4. **Per-user flags don't scale — classes do.** Hotline's per-account privilege management became unmanageable on busy servers; KDX's **Account Classes** were a genuine, well-liked improvement. Adopt this pattern from day one.
5. **Discoverability matters.** Trackers with categories/accounts were better than Hotline's flat list, but centralized trackers are also a single point of failure and abuse. Plan for federation/multiple trackers and for trackers going dark.

**What users liked better than Hotline (the "why KDX won hearts" list):**
- Real, always-on **encryption** (privacy from ISP/network snooping; Hotline was cleartext).
- **Account Classes** = sane bulk permission management.
- **Multiple chat rooms** + IRC bridge vs. Hotline's single room.
- **True cross-platform parity** (first-class Windows + Mac OS 9 + Mac OS X).
- **Superior remote server administration** (Remote Config/Upgrade/Shutdown + RAT screen control).
- **Faster/searchable file browsing** with resumable transfers and auto-sorting downloads.
- **News auto-archiving** and self-delete grace — small quality-of-life wins.

---

## Implications for RabbitHole

**Adopt (proven wins):**
- **Account Classes / role-based permissions as the core authz model**, with privileges as master switches and folder ACLs distinguishing *hide* from *deny*. This is the single most-praised KDX improvement over Hotline.
- **Multiple named chat rooms** per server from the start; treat single-room as a special case, not the model.
- **Server-maintained file index** for fast search + cached listings; resumable, offset-based transfers; auto-sort on download.
- **News as first-class persisted boards** (server → group → thread), with self-delete grace and configurable archiving/retention.
- **Rich, remotable admin surface**: connection monitor, server history, broadcast, live config, remote upgrade — but gate every remote-admin op behind an explicit, auditable privilege.
- **Trackers with categories**, but design for **multiple/federated trackers** and graceful degradation when a tracker is unreachable.

**Modernize (do NOT copy KDX here):**
- **Encryption**: use **TLS 1.3** for transport (forward secrecy, authenticated), or if you want an in-protocol handshake, use **Noise Protocol Framework** with modern AEAD (ChaCha20-Poly1305 / AES-GCM). **Never** ship proprietary crypto, MD5, or CRC32-for-integrity. Passwords: **Argon2id** hashing server-side, never store or transmit reversibly.
- **Open, documented, versioned protocol**: publish the wire format so RabbitHole doesn't repeat KDX's obscurity-driven death. A length-prefixed, typed **transaction/request-reply** framing (like KDX/Hotline) maps cleanly onto Rust enums + serde/`bytes`; keep a protocol version field and negotiate capabilities.
- **Least privilege on remote-admin / RAT-style features**: KDX-style "view/control display, launch programs, remote shutdown" is a RAT in every sense — sandbox it, make it opt-in per class, and log it. This is the highest-risk feature area to inherit.

**Scope guidance:** ship the **chat + PM + class-based accounts + news + file transfer + trackers** core first (that's the beloved Hotline/KDX heart). Treat **VoIP and IRC bridging as later, optional modules** — they added complexity that arguably diluted KDX's adoption.

**Highest-value follow-up (blocked here by egress policy):** obtain from an unrestricted network the **official *Haxial KDX 1.520 Documentation* PDF** (`cdn.preterhuman.net/texts/computing/KDX_info/Documentation.pdf`), the **hlwiki KDX Protocol page** (`hlwiki.com/index.php/KDX_Protocol`), and the **ProfDrLuigi/Haxial-KDX GitHub repo** — these hold the concrete transaction opcodes, exact ports, and handshake details you'll need to lock the wire format. To make that repo readable in-session, ask the user to add it via the add-repo flow (currently only `kevinelliott/rabbithole` is in scope).

**Sources** (via search; full-text fetch was blocked by this environment's egress policy):
- [KDX Protocol — Hotline Wiki](https://hlwiki.com/index.php/KDX_Protocol)
- [Haxial KDX (software) — Higher Intellect Vintage Wiki](https://wiki.preterhuman.net/Haxial_KDX_(software))
- [Haxial KDX 1.520 Documentation (PDF, preterhuman CDN)](http://cdn.preterhuman.net/texts/computing/KDX_info/Documentation.pdf)
- [KDX — Complete Russian user and admin guide (Sudo Null)](https://sudonull.com/post/55126-KDX-the-last-of-the-Hotline-Complete-Russian-user-and-admin-guide)
- [Haxial KDX Server 1.0 — Applefritter](https://www.applefritter.com/node/15286)
- [Haxial Software, Ltd. — everything2](https://everything2.com/title/Haxial+Software,+Ltd.)
- [Haxial RemoteAdminTool 1.0 — MacTech](http://preserve.mactech.com/content/haxial-remoteadmintool-10-remotely-admin-your-mac-0)
- [KDX Trackers — Haxial Wiki (Fandom)](https://haxial.fandom.com/wiki/KDX_Trackers)
- [ProfDrLuigi/Haxial-KDX — GitHub](https://github.com/ProfDrLuigi/Haxial-KDX)
- [The KDX Living Thread — MacRumors Forums](https://forums.macrumors.com/threads/the-kdx-living-thread-bbs-meets-a-gui-for-os-9-and-x.2295379/)
