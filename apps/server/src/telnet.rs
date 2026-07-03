//! Burrow's telnet BBS shell: banner → login → welcome screen → MAIN MENU
//! (boards, chat, direct mail, files, doors, QWK, quit) with `/go` keyword
//! teleports.
//!
//! The `rabbithole-legacy-telnet` crate owns the protocol layer
//! ([`TelnetStream`]: negotiation, line IO, encodings) and keeps a minimal
//! reference shell of its own; this module is the burrow-side shell — it
//! authenticates against the real [`AuthService`](rabbithole_server_core::AuthService)
//! (yielding a full [`AuthedUser`] with a permission [`Subject`], which the
//! trait-shaped `TelnetAuth` seam cannot carry), enforces the
//! `telnet_min_role` surface gate, and hosts the door-game commands (`doors`
//! to list, `door <id>` to play — see [`crate::doors`]), the file
//! browser (`files`), and the QWK offline-mail packet command (`qwk` — see
//! [`crate::qwk`]; it reuses the same HTTP-link handoff discipline as
//! `get`, one link per raw packet member).
//!
//! ## One community, many doors
//!
//! A logged-in telnet session is a full citizen of the shared world: it
//! joins [`PresenceRegistry`](rabbithole_server_core::PresenceRegistry) and
//! the chat lobby exactly like a native or Hotline session, so who-lists,
//! keyword lookups, and chat fan-out all see it. The Wave 2.3 welcome
//! composer ([`crate::handlers5::compose_welcome`]) renders as text after
//! login — preceded by the operator's `theme_logo_ansi` art, written
//! verbatim to ANSI-capable terminals (TTYPE) and flattened to plain glyphs
//! via `rabbithole-art` for everything else. Retro terminal types get CP437
//! on the wire (the art-crate translation tables behind the
//! `legacy-telnet` encoding seam); everyone else stays UTF-8.
//!
//! **Boards** (`[B]`) list postable boards (RBAC `SEE`/`BOARD_READ` per
//! `board/<slug>`), page threads and posts, and post replies/new threads
//! through [`BoardService`](rabbithole_server_core::BoardService) as the
//! logged-in user — same author seed derivation as QWK ingest, same
//! `BOARD_POST` + `post` rate budget as every other surface. **Chat**
//! (`[C]`) is the live lobby: scrollback, then a `tokio::select!` between
//! typed lines (cancel-safe `read_line`) and bus [`ServerEvent::Chat`]
//! deltas. **Direct mail** (`[D]`) lists DM conversations, pages threads,
//! and replies through the same store + bus + away-auto-response path the
//! native DM handlers use. **`/go <keyword>`** works at every prompt:
//! operator keyword map first (`board:`/`area:`/`door:`/`room:`/`user:`/
//! `url:` targets), then direct board/area/door matches, then the Wave 2.3
//! room/user resolver.
//!
//! ## The file browser: HTTP handoff and ZMODEM
//!
//! `files` opens a small sub-shell over the shared
//! [`FileService`](rabbithole_server_core::FileService): `ls` walks areas →
//! folders → files as a paged plain-ASCII table, `cd` moves around, and
//! `get <name>` **does not stream bytes** — it prints an HTTP(S) handoff
//! link `<files_http_base>/files/<area>/<path>` for the caller to fetch out
//! of band. With `files_http_base` unset, `get` explains transfers aren't
//! available that way. For real retro terminals the transfer *can* stay
//! in-band: `zget <name>` sends the file over ZMODEM and `zput` receives an
//! upload into the current (writable) folder — see [`crate::zmodem`] for
//! the protocol driving, resume, and the 8-bit-clean telnet seam. RBAC
//! mirrors the Hotline surface exactly: `files` root needs
//! [`Caps::FILE_LIST`], folder listings check `FILE_LIST` on the
//! `files/<area>/<path>` resource and hide entries the caller can't
//! [`Caps::SEE`], drop-box contents stay hidden without
//! [`Caps::DROPBOX_VIEW`], `get`/`zget` need [`Caps::FILE_DOWNLOAD`] (plus
//! the drop-box download rule and the moderation gates — quarantined or
//! hash-denied content reads as absent, exactly like the HTTP serve path),
//! and `zput` needs [`Caps::FILE_UPLOAD`] on the destination (uploading
//! *into* a drop box is the classic use). `zget`/`zput` starts spend from
//! the shared per-account `transfer` budget, like every transfer-open.

use std::io;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;

use rabbithole_legacy_telnet::{Echo, Encoding, TelnetStream};
use rabbithole_proto::welcome as pw;
use rabbithole_server_core::chat::Sender;
use rabbithole_server_core::ratelimit::{class as rl, now_ms, Scope};
use rabbithole_server_core::{AuthedUser, Caps, PresenceEntry, Role, ServerEvent, LOBBY};
use rabbithole_store_server::repo2::PersonasRepo;
use rabbithole_store_server::repo3::{dm_receipts_enabled, BlocksRepo, DmsRepo};
use rabbithole_store_server::repo4::PostRow;
use rabbithole_store_server::repo6::FileNodeRow;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::{doors, Shared};

/// Failed logins allowed before the connection is closed.
const MAX_ATTEMPTS: u32 = 3;

/// Run one telnet session over `io` to completion (quit, login lockout, or
/// disconnect). Burrow calls this once per accepted socket; the caller keeps
/// the socket and performs the graceful FIN + drain close afterwards.
/// `peer_ip` keys the per-IP auth/legacy rate buckets (`None` — e.g. a test
/// harness driving an in-memory stream — is unlimited).
pub async fn run_shell<S>(io: S, shared: &Arc<Shared>, peer_ip: Option<IpAddr>) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut t = TelnetStream::new(io);
    t.set_encoding(Encoding::Utf8);
    t.start().await?;
    let name = shared.config.read().name;
    t.write_str(&format!(
        "\n*** {name} ***\nDown the rabbit hole we go.\n\n"
    ))
    .await?;

    let Some(authed) = login(&mut t, shared, peer_ip).await? else {
        return Ok(()); // disconnected or out of attempts
    };
    shared.stats.incr("telnet", "logins");
    // Retro terminal types get CP437 on the wire (the art-crate tables
    // behind the encoding seam); unknown or modern terminals stay UTF-8.
    if t.terminal().is_some_and(cp437_terminal) {
        t.set_encoding(Encoding::Cp437);
    }
    greet(&mut t, &authed).await?;

    // Join the shared world — presence + the chat lobby — exactly like a
    // native or Hotline session, and leave it however the shell ends.
    let session_id = shared.next_session_id();
    shared.presence.join(PresenceEntry {
        session_id,
        account_id: authed.account.id,
        screen_name: authed.persona.screen_name.clone(),
        role: authed.subject.role,
        transport: "telnet".into(),
        connected_at: Instant::now(),
        state: 0,
        status: None,
    });
    shared
        .chat
        .join_lobby(session_id, &authed.persona.screen_name);
    let result = async {
        show_welcome(&mut t, shared, &authed, session_id).await?;
        menu_loop(&mut t, shared, &authed, session_id, peer_ip).await
    }
    .await;
    shared.chat.session_closed(session_id);
    shared.presence.leave(session_id);
    result
}

/// Does this TTYPE name a terminal that wants CP437 bytes on the wire?
/// SyncTERM and friends report `ANSI`-family names; modern emulators say
/// `XTERM`/`VT…` and stay UTF-8.
fn cp437_terminal(term: &str) -> bool {
    let t = term.to_ascii_lowercase();
    t == "ansi"
        || t == "ansi-bbs"
        || t == "pcansi"
        || t == "scoansi"
        || t.contains("syncterm")
        || t.contains("cp437")
}

/// Can this terminal render ANSI escape art? `None` (no TTYPE reported —
/// e.g. a raw socket) degrades to plain text.
fn ansi_terminal(term: Option<&str>) -> bool {
    let Some(term) = term else { return false };
    let t = term.to_ascii_lowercase();
    [
        "ansi", "xterm", "vt1", "vt2", "vt3", "linux", "screen", "syncterm", "color",
    ]
    .iter()
    .any(|n| t.contains(n))
}

/// Prompt for credentials until success, disconnect, or [`MAX_ATTEMPTS`].
/// TOTP-gated accounts can't complete the minimal prompt yet (no
/// second-factor step), so they fail here like a bad password. Failed
/// attempts also drain the per-IP `auth` rate bucket; an empty bucket ends
/// the session before (or right after) an attempt. A correct password for
/// an account below `telnet_min_role` is refused explicitly (it does not
/// count as a failed attempt — the credentials were right).
async fn login<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    peer_ip: Option<IpAddr>,
) -> io::Result<Option<AuthedUser>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    for _ in 0..MAX_ATTEMPTS {
        if let Some(ip) = peer_ip {
            if !shared.rate_probe(Scope::Ip(ip), rl::AUTH) {
                t.write_str("Too many failed logins. Try again later.\n")
                    .await?;
                return Ok(None);
            }
        }
        t.write_str("login: ").await?;
        let Some(user) = t.read_line(Echo::On).await? else {
            return Ok(None);
        };
        let user = user.trim().to_string();
        t.write_str("password: ").await?;
        let Some(pass) = t.read_line(Echo::Hidden).await? else {
            return Ok(None);
        };
        if !user.is_empty() {
            if let Ok(authed) = shared.auth.login_password(&user, &pass, None).await {
                let min = Role::parse_min_role(&shared.config.read().telnet_min_role)
                    .unwrap_or(Role::Guest);
                if authed.subject.role < min {
                    t.write_str(&format!(
                        "\nThis system requires {} access or better on telnet. \
                         Ask your sysop.\n",
                        min.min_role_name()
                    ))
                    .await?;
                    return Ok(None);
                }
                return Ok(Some(authed));
            }
        }
        if let Some(ip) = peer_ip {
            if !shared.rate_allow(Scope::Ip(ip), rl::AUTH) {
                t.write_str("Too many failed logins. Try again later.\n")
                    .await?;
                return Ok(None);
            }
        }
        t.write_str("Login incorrect.\n\n").await?;
    }
    t.write_str("Too many failures. Goodbye.\n").await?;
    Ok(None)
}

/// Post-login greeting, including what negotiation learned about the peer.
async fn greet<S>(t: &mut TelnetStream<S>, authed: &AuthedUser) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut line = format!("\nWelcome, {}!", authed.persona.screen_name);
    let mut details = Vec::new();
    if let Some(term) = t.terminal() {
        details.push(term.to_string());
    }
    if let Some((cols, rows)) = t.window() {
        details.push(format!("{cols}x{rows}"));
    }
    if !details.is_empty() {
        line.push_str(&format!(" [{}]", details.join(", ")));
    }
    line.push('\n');
    t.write_str(&line).await
}

/// Render the composed welcome screen (Wave 2.3) as terminal text: the
/// operator's logo art first (verbatim for ANSI-capable terminals, glyphs
/// only otherwise), then the widget list top to bottom.
async fn show_welcome<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    session_id: u64,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let logo = shared.config.read().theme_logo_ansi;
    if !logo.trim().is_empty() {
        let art = if ansi_terminal(t.terminal()) {
            logo
        } else {
            // Execute the ANSI program, keep only the glyphs.
            rabbithole_art::render_plain(&rabbithole_art::ansi::parse(logo.as_bytes()))
        };
        t.write_str("\n").await?;
        t.write_str(art.trim_end_matches(['\r', '\n'])).await?;
        t.write_str("\n").await?;
    }
    let screen = crate::handlers5::compose_welcome(
        shared,
        authed.account.id,
        authed.subject.role == Role::Guest,
        authed.subject.role,
        session_id,
    )
    .await;
    let mut out = String::new();
    for widget in &screen.widgets {
        match widget {
            pw::WelcomeWidget::Motd(motd) => {
                out.push_str(&format!("\n{}\n", motd.trim_end()));
            }
            pw::WelcomeWidget::UnreadDms(n) => {
                out.push_str(&format!(
                    "\nYou have {n} unread direct message(s) — [D] reads them.\n"
                ));
            }
            pw::WelcomeWidget::OnlineNow { count, sample } => {
                if sample.is_empty() {
                    out.push_str(&format!("\nOnline now: {count}.\n"));
                } else {
                    out.push_str(&format!("\nOnline now ({count}): {}\n", sample.join(", ")));
                }
            }
            pw::WelcomeWidget::Featured { title, body } => {
                out.push_str(&format!("\n*** {} ***\n", title.trim()));
                if !body.trim().is_empty() {
                    out.push_str(&format!("{}\n", body.trim_end()));
                }
            }
            pw::WelcomeWidget::Ticker(line) => {
                out.push_str(&format!("\nNews: {}\n", line.trim()));
            }
            _ => {} // widgets from the future render nowhere on telnet
        }
    }
    if !out.is_empty() {
        t.write_str(&out).await?;
    }
    Ok(())
}

/// The main menu. `[O]` appears only when door hosting is switched on, but
/// the commands always answer (with a polite refusal when disabled).
/// A one-line, ASCII-only now-playing banner for the telnet MAIN MENU — safe
/// across CP437/UTF-8 TTYPEs (no note glyph, no em dash), or `None` when
/// nothing is on the air. A live DJ's station is featured over playlist
/// automation; ties break on the slug.
fn radio_now_playing_line(
    stations: &[rabbithole_server_core::presence::RadioStatus],
) -> Option<String> {
    let s = stations
        .iter()
        .filter(|s| s.live)
        .min_by_key(|s| &s.station)
        .or_else(|| stations.iter().min_by_key(|s| &s.station))?;
    let track = if s.artist.is_empty() {
        s.title.clone()
    } else {
        format!("{} - {}", s.title, s.artist)
    };
    let mut line = format!("Now playing on {}: {}", s.station, track);
    if s.live && !s.dj.is_empty() {
        line.push_str(&format!(" (DJ {})", s.dj));
    }
    line.push_str(&format!(" [{} listening]", s.listeners));
    Some(line)
}

async fn menu_loop<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    session_id: u64,
    peer_ip: Option<IpAddr>,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // A `/go` teleport bubbles up from whichever prompt saw it and lands
    // here; jumping may itself yield the next hop.
    let mut pending: Option<GoTarget> = None;
    loop {
        if let Some(target) = pending.take() {
            pending = jump(t, shared, authed, session_id, peer_ip, target).await?;
            continue;
        }
        let mut menu = String::from("\n=== MAIN MENU ===\n");
        if let Some(np) = radio_now_playing_line(&shared.presence.radio_now_playing()) {
            menu.push_str(&format!(" {np}\n\n"));
        }
        menu.push_str(
            " [B] Message boards\n [C] Chat (the lobby)\n \
             [D] Direct mail\n [F] Files\n",
        );
        if shared.config.read().qwk_enabled {
            menu.push_str(" [M] QWK offline mail\n");
        }
        if shared.doors.enabled() {
            menu.push_str(" [O] Doors\n");
        }
        menu.push_str(" [Q] Quit\n\nType /go <keyword> to jump anywhere.\n\nCommand: ");
        t.write_str(&menu).await?;
        let Some(line) = t.read_line(Echo::On).await? else {
            return Ok(()); // peer went away
        };
        // Coarse per-IP legacy command budget: refuse the command, keep the
        // session.
        if let Some(ip) = peer_ip {
            if !shared.rate_allow(Scope::Ip(ip), rl::LEGACY) {
                t.write_str("\nRate limited; slow down.\n").await?;
                continue;
            }
        }
        // Lowercase only the verb — door ids and keywords match exactly.
        let (verb, arg) = split_command(&line);
        let arg_opt = (!arg.is_empty()).then_some(arg);
        match (verb.as_str(), arg_opt) {
            ("q" | "quit" | "g" | "goodbye", _) => {
                t.write_str(&format!("\nGoodbye, {}!\n", authed.persona.screen_name))
                    .await?;
                return Ok(());
            }
            ("", _) => {}
            ("b" | "boards", _) => {
                pending = boards_shell(t, shared, authed, peer_ip).await?;
            }
            ("c" | "chat", _) => {
                pending = chat_shell(t, shared, authed, session_id, LOBBY).await?;
            }
            ("d" | "dm" | "dms" | "mail", None) => {
                pending = dm_shell(t, shared, authed, peer_ip).await?;
            }
            ("d" | "dm" | "dms" | "mail", Some(peer)) => {
                pending = dm_thread(t, shared, authed, peer_ip, peer).await?;
            }
            ("f" | "files", _) => {
                pending = browse_files(t, shared, authed, peer_ip, None).await?;
            }
            ("m" | "qwk", _) => qwk_packet(t, shared, authed).await?,
            ("o" | "doors", None) => list_doors(t, shared).await?,
            ("door" | "doors" | "open", Some(id)) => {
                doors::run_door(t, shared, authed, id).await?;
            }
            ("/go" | "go", word) => {
                pending = go_command(t, shared, word.unwrap_or("")).await?;
            }
            ("help" | "?", _) => {
                t.write_str(
                    "\nb boards   c chat   d mail   f files   doors   q quit\n\
                     /go <keyword> jumps to a board, file area, door, room, or user.\n",
                )
                .await?;
            }
            (other, _) => {
                t.write_str(&format!("\nUnknown command: {other}\n"))
                    .await?;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// `/go` keyword teleports (Wave 2.3's resolver, extended with the legacy
// surfaces: boards, file areas, doors).

/// Where a telnet `/go` can land.
enum GoTarget {
    Board(String),
    FileArea(String),
    Door(String),
    Room(String),
    User(String),
    Url(String),
}

/// Handle a `/go [word]` command at any prompt: no word lists the
/// keywords, an unknown word explains itself, a resolved word returns the
/// target for the caller to bubble up to [`menu_loop`].
async fn go_command<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    word: &str,
) -> io::Result<Option<GoTarget>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let word = word.trim();
    if word.is_empty() {
        return list_keywords(t, shared).await.map(|_| None);
    }
    match resolve_go(shared, word).await {
        Some(target) => Ok(Some(target)),
        None => {
            t.write_str(&format!(
                "\nNothing answers to `{word}`. `/go` alone lists keywords.\n"
            ))
            .await?;
            Ok(None)
        }
    }
}

/// `/go` with no argument: the operator keyword map, plus what else works.
async fn list_keywords<S>(t: &mut TelnetStream<S>, shared: &Arc<Shared>) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let keywords = shared.config.read().keywords;
    let mut out = String::from("\n--- Keywords ---\n");
    if keywords.is_empty() {
        out.push_str("(none configured)\n");
    } else {
        let mut sorted: Vec<(&String, &String)> = keywords.iter().collect();
        sorted.sort();
        for (word, target) in sorted {
            out.push_str(&format!("  {word:<20} {target}\n"));
        }
    }
    out.push_str(
        "Any board slug, file area, door id, room, or user name works too.\n\
         Usage: /go <keyword>\n",
    );
    t.write_str(&out).await
}

/// Resolve a `/go` word: the operator keyword map first (`board:`,
/// `area:`/`files:`, `door:`, `room:`, `user:`, `url:` targets), then a
/// direct board / file area / door match, then the Wave 2.3 room/user/url
/// resolver. `None` = nothing answers.
async fn resolve_go(shared: &Arc<Shared>, word: &str) -> Option<GoTarget> {
    let lower = word.to_lowercase();
    if let Some(mapped) = shared.config.read().keywords.get(&lower).cloned() {
        if let Some(b) = mapped.strip_prefix("board:") {
            return Some(GoTarget::Board(b.to_string()));
        }
        if let Some(a) = mapped
            .strip_prefix("area:")
            .or_else(|| mapped.strip_prefix("files:"))
        {
            return Some(GoTarget::FileArea(a.to_string()));
        }
        if let Some(d) = mapped.strip_prefix("door:") {
            return Some(GoTarget::Door(d.to_string()));
        }
        if let Some(r) = mapped.strip_prefix("room:") {
            return Some(GoTarget::Room(r.to_string()));
        }
        if let Some(u) = mapped.strip_prefix("user:") {
            return Some(GoTarget::User(u.to_string()));
        }
        if let Some(u) = mapped.strip_prefix("url:") {
            return Some(GoTarget::Url(u.to_string()));
        }
    }
    // Direct matches on the legacy surfaces.
    if let Ok(Some(board)) = shared.boards.board(&lower).await {
        if board.kind == 2 {
            return Some(GoTarget::Board(board.slug));
        }
    }
    if let Ok(areas) = shared.files.areas().await {
        if areas.iter().any(|a| a.slug == lower) {
            return Some(GoTarget::FileArea(lower));
        }
    }
    if shared.doors.enabled() && shared.doors.get(word).is_some() {
        return Some(GoTarget::Door(word.to_string()));
    }
    // The Wave 2.3 keyword service: rooms and personas.
    match crate::handlers5::resolve_keyword(shared, word).await {
        Ok(target) => match target.kind {
            pw::KeywordKind::Room => Some(GoTarget::Room(target.target)),
            pw::KeywordKind::User => Some(GoTarget::User(target.target)),
            pw::KeywordKind::Url => Some(GoTarget::Url(target.target)),
            _ => None,
        },
        Err(_) => None,
    }
}

/// Land a teleport on its surface. May itself return the *next* hop (a
/// `/go` typed inside the destination shell).
async fn jump<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    session_id: u64,
    peer_ip: Option<IpAddr>,
    target: GoTarget,
) -> io::Result<Option<GoTarget>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match target {
        GoTarget::Board(slug) => board_view(t, shared, authed, peer_ip, &slug).await,
        GoTarget::FileArea(area) => browse_files(t, shared, authed, peer_ip, Some(area)).await,
        GoTarget::Door(id) => {
            doors::run_door(t, shared, authed, &id).await?;
            Ok(None)
        }
        GoTarget::Room(room) => chat_shell(t, shared, authed, session_id, &room).await,
        GoTarget::User(name) => dm_thread(t, shared, authed, peer_ip, &name).await,
        GoTarget::Url(url) => {
            t.write_str(&format!("\nThat keyword points at: {url}\n"))
                .await?;
            Ok(None)
        }
    }
}

/// Split a prompt line into a lowercased verb and its (trimmed, original
/// case) argument rest.
fn split_command(line: &str) -> (String, &str) {
    let line = line.trim();
    match line.split_once(char::is_whitespace) {
        Some((verb, rest)) => (verb.to_ascii_lowercase(), rest.trim()),
        None => (line.to_ascii_lowercase(), ""),
    }
}

// ---------------------------------------------------------------------------
// The file browser: areas → folders → files, with an HTTP-link `get` handoff
// (see the module docs — no bytes are ever streamed over telnet).

/// Rows shown per `ls` page before the More prompt.
const FILES_PAGE_ROWS: usize = 18;

/// The ACL resource string for an area/path, matching the native and Hotline
/// handlers (`files/<area>` for the root, `files/<area>/<path>` within).
fn file_resource(area: &str, path: &str) -> String {
    if path.is_empty() {
        format!("files/{area}")
    } else {
        format!("files/{area}/{path}")
    }
}

/// Where the browser currently is: nowhere (area list) or a folder path
/// (possibly empty = area root) within an area.
#[derive(Default)]
struct FileCursor {
    area: Option<String>,
    path: Vec<String>,
}

impl FileCursor {
    /// The folder path within the area, `""` at the area root.
    fn folder(&self) -> String {
        self.path.join("/")
    }

    /// Prompt-friendly location: `/`, `/area`, `/area/folder/...`.
    fn location(&self) -> String {
        match &self.area {
            None => "/".to_string(),
            Some(a) if self.path.is_empty() => format!("/{a}"),
            Some(a) => format!("/{a}/{}", self.folder()),
        }
    }
}

/// The `files` sub-shell. Commands: `ls`, `cd <name>` / `cd ..`,
/// `get <name>`, `/go <keyword>`, `help`, `q`. Every command spends from
/// the same per-IP legacy budget as the main menu. `start_area` (a `/go`
/// teleport) opens the browser inside that area.
async fn browse_files<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    peer_ip: Option<IpAddr>,
    start_area: Option<String>,
) -> io::Result<Option<GoTarget>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if !shared
        .perms
        .allows(&authed.subject, "files", Caps::FILE_LIST)
    {
        t.write_str("\nYou do not have access to the file library.\n")
            .await?;
        return Ok(None);
    }
    let mut cur = FileCursor::default();
    if let Some(area) = start_area {
        match shared.files.areas().await {
            Ok(areas) if areas.iter().any(|a| a.slug == area) => cur.area = Some(area),
            _ => {
                t.write_str(&format!("\nNo such area: {area}\n")).await?;
            }
        }
    }
    t.write_str(
        "\n--- File Library ---\n\
         Commands: ls, cd <name>, cd .., get <name>, zget <name>, zput, \
         q (back to menu)\n",
    )
    .await?;
    list_level(t, shared, authed, &cur).await?;
    loop {
        t.write_str(&format!("\nfiles {}> ", cur.location()))
            .await?;
        let Some(line) = t.read_line(Echo::On).await? else {
            return Ok(None); // peer went away
        };
        if let Some(ip) = peer_ip {
            if !shared.rate_allow(Scope::Ip(ip), rl::LEGACY) {
                t.write_str("\nRate limited; slow down.\n").await?;
                continue;
            }
        }
        // Lowercase only the verb — names are matched exactly.
        let (verb, arg) = split_command(&line);
        match verb.as_str() {
            "" => {}
            "q" | "quit" | "x" | "exit" => return Ok(None),
            "help" | "?" => {
                t.write_str(
                    "\nls           list this level (paged)\n\
                     cd <name>    enter an area or folder (cd .. goes up)\n\
                     get <name>   print an HTTP link for a file\n\
                     zget <name>  send a file over ZMODEM (start your receive)\n\
                     zput         receive a ZMODEM upload into this folder\n\
                     /go <word>   keyword teleport\n\
                     q            back to the main menu\n",
                )
                .await?;
            }
            "ls" | "list" | "dir" => list_level(t, shared, authed, &cur).await?,
            "cd" => change_dir(t, shared, authed, &mut cur, arg).await?,
            "get" => hand_off(t, shared, authed, &cur, arg).await?,
            "zget" | "sz" => zget_cmd(t, shared, authed, &cur, arg).await?,
            "zput" | "rz" => zput_cmd(t, shared, authed, &cur).await?,
            "/go" => {
                if let Some(target) = go_command(t, shared, arg).await? {
                    return Ok(Some(target));
                }
            }
            other => {
                t.write_str(&format!("\nUnknown command: {other} (try `help`)\n"))
                    .await?;
            }
        }
    }
}

/// `ls`: the area table at the root, or a folder listing within an area —
/// paged, plain ASCII, hiding what the caller can't SEE and the contents of
/// drop boxes (mirroring Hotline's GetFileNameList).
async fn list_level<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    cur: &FileCursor,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let Some(area) = &cur.area else {
        let areas = match shared.files.areas().await {
            Ok(a) => a,
            Err(_) => return t.write_str("\nThe file library is unavailable.\n").await,
        };
        if areas.is_empty() {
            return t.write_str("\nNo file areas yet.\n").await;
        }
        let mut rows = vec![format!("{:<28} {}", "AREA", "TITLE")];
        for a in &areas {
            rows.push(format!("{:<28} {}", a.slug, a.title));
        }
        return page_out(t, &rows).await;
    };

    let folder = cur.folder();
    let resource = file_resource(area, &folder);
    if !shared
        .perms
        .allows(&authed.subject, &resource, Caps::FILE_LIST)
    {
        return t
            .write_str("\nYou do not have access to that folder.\n")
            .await;
    }
    // A drop box hides its contents unless the caller can view drop boxes.
    if !folder.is_empty() {
        if let Ok(Some(node)) = shared.files.node_by_path(area, &folder).await {
            if node.is_dropbox
                && !shared
                    .perms
                    .allows(&authed.subject, &resource, Caps::DROPBOX_VIEW)
            {
                return t.write_str("\n(drop box: contents are hidden)\n").await;
            }
        }
    }
    let folder_opt = (!folder.is_empty()).then_some(folder.as_str());
    let nodes = match shared.files.list(area, folder_opt).await {
        Ok(n) => n,
        Err(e) => return t.write_str(&format!("\n{e}\n")).await,
    };
    let visible: Vec<&FileNodeRow> = nodes
        .iter()
        .filter(|n| {
            shared
                .perms
                .allows(&authed.subject, &file_resource(area, &n.path), Caps::SEE)
        })
        .collect();
    if visible.is_empty() {
        return t.write_str("\n(empty)\n").await;
    }
    let mut rows = vec![format!("{:<32} {:>10} {}", "NAME", "SIZE", "UPLOADED")];
    for n in visible {
        let is_folder = n.kind == rabbithole_server_core::files::KIND_FOLDER;
        let name = if is_folder {
            format!("{}/", n.name)
        } else {
            n.name.clone()
        };
        let size = if is_folder {
            "<dir>".to_string()
        } else {
            fmt_size(n.size)
        };
        rows.push(format!(
            "{:<32} {:>10} {}",
            name,
            size,
            fmt_date(n.created_at)
        ));
    }
    page_out(t, &rows).await
}

/// `cd`: enter an area (from the root) or a folder (within an area);
/// `cd ..` walks up. Nodes the caller can't SEE read as nonexistent — hide,
/// don't tease.
async fn change_dir<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    cur: &mut FileCursor,
    arg: &str,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if arg.is_empty() {
        return t.write_str("\nUsage: cd <name> (or cd ..)\n").await;
    }
    if arg == ".." {
        if cur.path.pop().is_none() {
            cur.area = None;
        }
        return Ok(());
    }
    if arg == "/" {
        cur.area = None;
        cur.path.clear();
        return Ok(());
    }
    let Some(area) = cur.area.clone() else {
        // Entering an area from the root list.
        match shared.files.areas().await {
            Ok(areas) if areas.iter().any(|a| a.slug == arg) => {
                cur.area = Some(arg.to_string());
                Ok(())
            }
            _ => t.write_str(&format!("\nNo such area: {arg}\n")).await,
        }?;
        return Ok(());
    };
    let folder = cur.folder();
    let target = if folder.is_empty() {
        arg.to_string()
    } else {
        format!("{folder}/{arg}")
    };
    match shared.files.node_by_path(&area, &target).await {
        Ok(Some(node))
            if node.kind == rabbithole_server_core::files::KIND_FOLDER
                && shared.perms.allows(
                    &authed.subject,
                    &file_resource(&area, &node.path),
                    Caps::SEE,
                ) =>
        {
            cur.path.push(arg.to_string());
            Ok(())
        }
        _ => t.write_str(&format!("\nNo such folder: {arg}\n")).await,
    }
}

/// Resolve and authorize `arg` (relative to the cursor) for download, the
/// checks every byte-serving path applies: hidden without [`Caps::SEE`],
/// one alias hop, files only, [`Caps::FILE_DOWNLOAD`], the drop-box rule
/// Hotline's DownloadFile enforces, and the moderation gates the HTTP serve
/// path runs (quarantined-for-review and hash-denied blobs read as absent —
/// checks the link-minting `get` previously left entirely to the HTTP hop).
/// Writes the refusal itself; `None` means refused.
async fn authorize_download<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    area: &str,
    folder: &str,
    arg: &str,
) -> io::Result<Option<FileNodeRow>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let full = if folder.is_empty() {
        arg.to_string()
    } else {
        format!("{folder}/{arg}")
    };
    // Resolve (following one alias hop), hiding what the caller can't SEE.
    let node = match shared.files.node_by_path(area, &full).await {
        Ok(Some(n))
            if shared
                .perms
                .allows(&authed.subject, &file_resource(area, &n.path), Caps::SEE) =>
        {
            n
        }
        _ => {
            t.write_str(&format!("\nNo such file: {arg}\n")).await?;
            return Ok(None);
        }
    };
    let target = match shared.files.resolve(node.id).await {
        Ok(n) => n,
        Err(_) => {
            t.write_str(&format!("\nNo such file: {arg}\n")).await?;
            return Ok(None);
        }
    };
    if target.kind != rabbithole_server_core::files::KIND_FILE {
        t.write_str(&format!("\n{arg} is a folder — `cd` into it instead.\n"))
            .await?;
        return Ok(None);
    }
    let resource = file_resource(&target.area, &target.path);
    if !shared
        .perms
        .allows(&authed.subject, &resource, Caps::FILE_DOWNLOAD)
    {
        t.write_str("\nYou do not have permission to download that file.\n")
            .await?;
        return Ok(None);
    }
    // Drop-boxed content is not downloadable without view/manage rights
    // (the same rule Hotline's DownloadFile applies).
    let in_dropbox = shared.files.in_dropbox(&target).await.unwrap_or(false);
    if in_dropbox
        && !shared
            .perms
            .allows(&authed.subject, &resource, Caps::DROPBOX_VIEW)
        && !shared.perms.allows(
            &authed.subject,
            &file_resource(&target.area, ""),
            Caps::FILE_MANAGE,
        )
    {
        t.write_str("\nYou do not have permission to download that file.\n")
            .await?;
        return Ok(None);
    }
    // Moderation: quarantined and hash-denied content reads as absent, the
    // same non-teasing 404 the HTTP serve path gives.
    if shared.moderation.file_quarantined(target.blob_id.as_ref())
        || target
            .blob_id
            .as_ref()
            .is_some_and(|b| shared.moderation.is_denied(b))
    {
        t.write_str(&format!("\nNo such file: {arg}\n")).await?;
        return Ok(None);
    }
    Ok(Some(target))
}

/// `get`: authorize like a Hotline download (see [`authorize_download`]),
/// then print the HTTP handoff link — or explain that link handoffs aren't
/// available when `files_http_base` is unset (zget still works).
async fn hand_off<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    cur: &FileCursor,
    arg: &str,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if arg.is_empty() {
        return t.write_str("\nUsage: get <name>\n").await;
    }
    let Some(area) = &cur.area else {
        return t
            .write_str("\nEnter an area first (cd <area>), then `get <name>`.\n")
            .await;
    };
    let folder = cur.folder();
    let Some(target) = authorize_download(t, shared, authed, area, &folder, arg).await? else {
        return Ok(());
    };
    let base = shared.config.read().files_http_base;
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() {
        return t
            .write_str(
                "\nHTTP handoff links are not configured here; try \
                 `zget <name>` for an in-band ZMODEM transfer.\n",
            )
            .await;
    }
    t.write_str(&format!(
        "\nFetch it over HTTP (valid while the file exists):\n  {}/files/{}/{}\n",
        base,
        url_encode_path(&target.area),
        url_encode_path(&target.path)
    ))
    .await
}

/// `zget`: the same download authorization as `get`, then stream the bytes
/// in-band over ZMODEM (see [`crate::zmodem::send_file`]). Starts spend
/// from the shared per-account `transfer` budget, like every transfer-open.
async fn zget_cmd<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    cur: &FileCursor,
    arg: &str,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if arg.is_empty() {
        return t.write_str("\nUsage: zget <name>\n").await;
    }
    let Some(area) = &cur.area else {
        return t
            .write_str("\nEnter an area first (cd <area>), then `zget <name>`.\n")
            .await;
    };
    if !shared.rate_allow(Scope::Account(authed.account.id), rl::TRANSFER) {
        return t.write_str("\nRate limited; slow down.\n").await;
    }
    let folder = cur.folder();
    let Some(target) = authorize_download(t, shared, authed, area, &folder, arg).await? else {
        return Ok(());
    };
    crate::zmodem::send_file(t, shared, authed, &target).await
}

/// `zput`: receive a ZMODEM upload into the current folder (see
/// [`crate::zmodem::receive_files`]). Gated like a Hotline upload-open:
/// no guests, [`Caps::FILE_UPLOAD`] on the destination resource (drop
/// boxes very much included — uploading *into* one is the classic use),
/// and the shared per-account `transfer` budget.
async fn zput_cmd<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    cur: &FileCursor,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let Some(area) = &cur.area else {
        return t
            .write_str("\nEnter an area first (cd <area>), then `zput`.\n")
            .await;
    };
    if authed.subject.role == Role::Guest {
        return t
            .write_str("\nUploads need a member account. Ask your sysop.\n")
            .await;
    }
    let folder = cur.folder();
    let resource = file_resource(area, &folder);
    if !shared
        .perms
        .allows(&authed.subject, &resource, Caps::FILE_UPLOAD)
    {
        return t
            .write_str("\nYou do not have permission to upload here.\n")
            .await;
    }
    if !shared.rate_allow(Scope::Account(authed.account.id), rl::TRANSFER) {
        return t.write_str("\nRate limited; slow down.\n").await;
    }
    let folder_opt = (!folder.is_empty()).then_some(folder.as_str());
    crate::zmodem::receive_files(t, shared, authed, area, folder_opt).await
}

/// Write `rows` in pages of [`FILES_PAGE_ROWS`], pausing with a More prompt
/// between pages (Enter continues, `q` stops).
async fn page_out<S>(t: &mut TelnetStream<S>, rows: &[String]) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    t.write_str("\n").await?;
    let mut shown = 0usize;
    for chunk in rows.chunks(FILES_PAGE_ROWS) {
        for row in chunk {
            t.write_str(&format!("{row}\n")).await?;
        }
        shown += chunk.len();
        if shown < rows.len() {
            t.write_str(&format!(
                "-- More ({shown}/{}) [Enter continues, q stops] -- ",
                rows.len()
            ))
            .await?;
            match t.read_line(Echo::On).await? {
                Some(ans) if !ans.trim().to_ascii_lowercase().starts_with('q') => {}
                _ => return Ok(()),
            }
        }
    }
    Ok(())
}

/// Human file size for the ASCII table (exact bytes under 10K, then K/M/G
/// with one decimal).
fn fmt_size(bytes: i64) -> String {
    let b = bytes.max(0) as f64;
    if b < 10_240.0 {
        format!("{bytes}B")
    } else if b < 1_048_576.0 {
        format!("{:.1}K", b / 1024.0)
    } else if b < 1_073_741_824.0 {
        format!("{:.1}M", b / 1_048_576.0)
    } else {
        format!("{:.1}G", b / 1_073_741_824.0)
    }
}

/// `YYYY-MM-DD` from unix milliseconds.
fn fmt_date(unix_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(unix_ms)
        .unwrap_or_default()
        .format("%Y-%m-%d")
        .to_string()
}

/// Percent-encode a virtual path for the handoff URL: RFC 3986 unreserved
/// characters and `/` pass through, everything else (spaces, UTF-8, `%`)
/// is `%XX`-encoded byte-wise.
fn url_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for &b in path.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The `qwk` command: build an offline-mail packet (see [`crate::qwk`]) and
/// print one HTTP handoff link per raw member — telnet never streams bytes,
/// exactly the file browser's `get` discipline. Refusals come first, and
/// **before** the build, so read pointers never advance for a packet the
/// caller can't fetch: `qwk_enabled` off → polite notice; `files_http_base`
/// unset → the no-transfers-on-telnet notice. ZIP bundling, HTTP serving of
/// the spool, and a zmodem path are documented follow-ups in [`crate::qwk`].
async fn qwk_packet<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let cfg = shared.config.read();
    if !cfg.qwk_enabled {
        return t
            .write_str("\nQWK offline mail is not enabled on this system.\n")
            .await;
    }
    let base = cfg.files_http_base.trim().trim_end_matches('/').to_string();
    if base.is_empty() {
        return t
            .write_str(
                "\nQWK packet transfers are not available on telnet; ask your \
                 sysop about the web interface.\n",
            )
            .await;
    }
    drop(cfg);
    let build = match crate::qwk::build_for(shared, &authed.account).await {
        Ok(b) => b,
        Err(crate::qwk::QwkGateError::Disabled) => {
            return t
                .write_str("\nQWK offline mail is not enabled on this system.\n")
                .await;
        }
        Err(crate::qwk::QwkGateError::Forbidden) => {
            return t
                .write_str("\nYou do not have permission to download mail packets.\n")
                .await;
        }
        Err(e) => {
            tracing::warn!("telnet qwk build failed: {e}");
            return t
                .write_str("\nThe QWK packer is unavailable right now; try again later.\n")
                .await;
        }
    };
    let mut out = format!(
        "\n--- QWK packet for {}: {} new message(s) in {} conference(s) ---\n\
         Fetch each member over HTTP (raw QWK members; ZIP bundling is a \
         follow-up):\n",
        authed.account.login,
        build.total_messages,
        build.conferences.len()
    );
    for m in &build.members {
        out.push_str(&format!(
            "  {:<12} {:>8}  {}/qwk/{}/{}\n",
            m.name,
            fmt_size(m.size as i64),
            base,
            url_encode_path(&authed.account.login),
            url_encode_path(&m.name)
        ));
    }
    out.push_str("Read pointers advanced; the next packet starts after this mail.\n");
    t.write_str(&out).await
}

/// Print the door menu (insertion order = the sysop's `[[doors]]` order).
async fn list_doors<S>(t: &mut TelnetStream<S>, shared: &Arc<Shared>) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if !shared.doors.enabled() {
        return t
            .write_str("\nDoors are not enabled on this system.\n")
            .await;
    }
    let list = shared.doors.list();
    if list.is_empty() {
        return t.write_str("\nNo doors are installed.\n").await;
    }
    let mut out = String::from("\n--- Door Games ---\n");
    for d in list {
        out.push_str(&format!("  {:<12} {}\n", d.id, d.title));
    }
    out.push_str("\nType `door <id>` to play.\n");
    t.write_str(&out).await
}

// ---------------------------------------------------------------------------
// Message boards: list → paged threads → paged posts, with line-editor
// posting through the shared BoardService (same author seed as QWK ingest,
// same BOARD_POST + `post` budget as every surface).

/// Boards the caller may see and read: postable (`kind == 2`) and passing
/// `SEE | BOARD_READ` on `board/<slug>`.
async fn visible_boards(
    shared: &Arc<Shared>,
    authed: &AuthedUser,
) -> Vec<rabbithole_store_server::repo4::BoardRow> {
    let Ok(all) = shared.boards.boards().await else {
        return Vec::new();
    };
    all.into_iter()
        .filter(|b| {
            b.kind == 2
                && shared.perms.allows(
                    &authed.subject,
                    &format!("board/{}", b.slug),
                    Caps::SEE.union(Caps::BOARD_READ),
                )
        })
        .collect()
}

/// The `[B]` sub-shell: pick a board by number or slug.
async fn boards_shell<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    peer_ip: Option<IpAddr>,
) -> io::Result<Option<GoTarget>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if !shared
        .perms
        .allows(&authed.subject, "board", Caps::BOARD_READ)
    {
        t.write_str("\nYou do not have access to the message boards.\n")
            .await?;
        return Ok(None);
    }
    t.write_str(
        "\n--- Message Boards ---\n\
         Commands: ls, <n> or <slug> to open, q (back to menu), /go <keyword>\n",
    )
    .await?;
    let mut boards = visible_boards(shared, authed).await;
    print_board_list(t, &boards).await?;
    loop {
        t.write_str("\nboards> ").await?;
        let Some(line) = t.read_line(Echo::On).await? else {
            return Ok(None); // peer went away
        };
        if let Some(ip) = peer_ip {
            if !shared.rate_allow(Scope::Ip(ip), rl::LEGACY) {
                t.write_str("\nRate limited; slow down.\n").await?;
                continue;
            }
        }
        let (verb, arg) = split_command(&line);
        match verb.as_str() {
            "" => {}
            "q" | "quit" | "x" | "exit" => return Ok(None),
            "ls" | "list" => {
                boards = visible_boards(shared, authed).await;
                print_board_list(t, &boards).await?;
            }
            "help" | "?" => {
                t.write_str(
                    "\nls        list boards\n<n>/<slug> open a board\n\
                     q         back to the main menu\n/go <word> keyword teleport\n",
                )
                .await?;
            }
            "/go" => {
                if let Some(target) = go_command(t, shared, arg).await? {
                    return Ok(Some(target));
                }
            }
            _ => {
                let choice = line.trim();
                let picked = choice
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| boards.get(n.wrapping_sub(1)))
                    .map(|b| b.slug.clone())
                    .or_else(|| {
                        boards
                            .iter()
                            .find(|b| b.slug.eq_ignore_ascii_case(choice))
                            .map(|b| b.slug.clone())
                    });
                match picked {
                    Some(slug) => {
                        if let Some(target) = board_view(t, shared, authed, peer_ip, &slug).await? {
                            return Ok(Some(target));
                        }
                    }
                    None => {
                        t.write_str(&format!("\nNo such board: {choice}\n")).await?;
                    }
                }
            }
        }
    }
}

/// The board list as a paged table.
async fn print_board_list<S>(
    t: &mut TelnetStream<S>,
    boards: &[rabbithole_store_server::repo4::BoardRow],
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if boards.is_empty() {
        return t.write_str("\nNo boards yet.\n").await;
    }
    let mut rows = vec![format!("  #  {:<28} {}", "BOARD", "TITLE")];
    for (i, b) in boards.iter().enumerate() {
        rows.push(format!("{:>3}  {:<28} {}", i + 1, b.slug, b.title));
    }
    page_out(t, &rows).await
}

/// One board: paged thread list, `n` new thread, `<n>` read a thread.
async fn board_view<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    peer_ip: Option<IpAddr>,
    slug: &str,
) -> io::Result<Option<GoTarget>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // Hide, don't tease: an unreadable or non-postable board reads as absent.
    let board = match shared.boards.board(slug).await {
        Ok(Some(b))
            if b.kind == 2
                && shared.perms.allows(
                    &authed.subject,
                    &format!("board/{}", b.slug),
                    Caps::SEE.union(Caps::BOARD_READ),
                ) =>
        {
            b
        }
        _ => {
            t.write_str(&format!("\nNo such board: {slug}\n")).await?;
            return Ok(None);
        }
    };
    let mut header = format!("\n--- {} ({}) ---\n", board.title, board.slug);
    if !board.description.trim().is_empty() {
        header.push_str(&format!("{}\n", board.description.trim()));
    }
    header.push_str("Commands: ls, <n> to read, n (new thread), q (back), /go <keyword>\n");
    t.write_str(&header).await?;
    let mut threads = render_threads(t, shared, &board.slug).await?;
    loop {
        t.write_str(&format!("\nboard {}> ", board.slug)).await?;
        let Some(line) = t.read_line(Echo::On).await? else {
            return Ok(None); // peer went away
        };
        if let Some(ip) = peer_ip {
            if !shared.rate_allow(Scope::Ip(ip), rl::LEGACY) {
                t.write_str("\nRate limited; slow down.\n").await?;
                continue;
            }
        }
        let (verb, arg) = split_command(&line);
        match verb.as_str() {
            "" => {}
            "q" | "quit" | "x" | "exit" => return Ok(None),
            "ls" | "list" => threads = render_threads(t, shared, &board.slug).await?,
            "n" | "new" | "post" => {
                post_editor(t, shared, authed, &board.slug, None, "").await?;
                threads = render_threads(t, shared, &board.slug).await?;
            }
            "help" | "?" => {
                t.write_str(
                    "\nls   re-list threads\n<n>  read thread n\n\
                     n    start a new thread\nq    back\n/go <word> keyword teleport\n",
                )
                .await?;
            }
            "/go" => {
                if let Some(target) = go_command(t, shared, arg).await? {
                    return Ok(Some(target));
                }
            }
            other => match other
                .parse::<usize>()
                .ok()
                .and_then(|n| threads.get(n.wrapping_sub(1)))
            {
                Some((root, _, _)) => {
                    let root_id = root.root_id.unwrap_or(root.event_id);
                    if let Some(target) =
                        thread_view(t, shared, authed, peer_ip, &board.slug, root_id).await?
                    {
                        return Ok(Some(target));
                    }
                }
                None => {
                    t.write_str(&format!("\nNo such thread: {other}\n")).await?;
                }
            },
        }
    }
}

/// Most threads listed per board screen (newest first).
const THREAD_LIST_LIMIT: i64 = 200;

/// Print the paged thread table; returns the rows for number selection.
async fn render_threads<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    slug: &str,
) -> io::Result<Vec<(PostRow, i64, i64)>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let threads = match shared.boards.threads(slug, THREAD_LIST_LIMIT).await {
        Ok(list) => list,
        Err(e) => {
            t.write_str(&format!("\n{e}\n")).await?;
            return Ok(Vec::new());
        }
    };
    if threads.is_empty() {
        t.write_str("\n(no threads yet — `n` starts one)\n").await?;
        return Ok(threads);
    }
    let now = chrono::Utc::now().timestamp_millis();
    let mut rows = vec![format!(
        "  #  {:<36} {:<18} {:>4}  {}",
        "SUBJECT", "AUTHOR", "RE", "AGE"
    )];
    for (i, (root, replies, last)) in threads.iter().enumerate() {
        let subject = if root.tombstoned {
            "(deleted)".to_string()
        } else {
            clip(&root.subject, 36)
        };
        rows.push(format!(
            "{:>3}  {:<36} {:<18} {:>4}  {}",
            i + 1,
            subject,
            clip(&root.author, 18),
            replies,
            fmt_age(now - (*last).max(root.created_at))
        ));
    }
    page_out(t, &rows).await?;
    Ok(threads)
}

/// One thread: every post, paged; `r` replies.
async fn thread_view<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    peer_ip: Option<IpAddr>,
    slug: &str,
    root: [u8; 32],
) -> io::Result<Option<GoTarget>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let subject = render_posts(t, shared, &root).await?;
    let Some(root_subject) = subject else {
        t.write_str("\nThat thread is gone.\n").await?;
        return Ok(None);
    };
    loop {
        t.write_str("\nthread> ").await?;
        let Some(line) = t.read_line(Echo::On).await? else {
            return Ok(None); // peer went away
        };
        if let Some(ip) = peer_ip {
            if !shared.rate_allow(Scope::Ip(ip), rl::LEGACY) {
                t.write_str("\nRate limited; slow down.\n").await?;
                continue;
            }
        }
        let (verb, arg) = split_command(&line);
        match verb.as_str() {
            "" => {}
            "q" | "quit" | "x" | "exit" => return Ok(None),
            "ls" | "list" => {
                render_posts(t, shared, &root).await?;
            }
            "r" | "reply" => {
                let default = if root_subject.to_lowercase().starts_with("re:") {
                    root_subject.clone()
                } else {
                    format!("Re: {root_subject}")
                };
                post_editor(t, shared, authed, slug, Some(root), &default).await?;
            }
            "help" | "?" => {
                t.write_str(
                    "\nls  re-read the thread\nr   reply\nq   back\n\
                     /go <word> keyword teleport\n",
                )
                .await?;
            }
            "/go" => {
                if let Some(target) = go_command(t, shared, arg).await? {
                    return Ok(Some(target));
                }
            }
            other => {
                t.write_str(&format!("\nUnknown command: {other} (try `help`)\n"))
                    .await?;
            }
        }
    }
}

/// Most posts rendered per thread read.
const THREAD_POSTS_LIMIT: i64 = 500;

/// Print a thread's posts (paged); returns the root subject, or `None`
/// when the thread has no posts.
async fn render_posts<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    root: &[u8; 32],
) -> io::Result<Option<String>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let posts = match shared.boards.thread(root, THREAD_POSTS_LIMIT).await {
        Ok(p) => p,
        Err(e) => {
            t.write_str(&format!("\n{e}\n")).await?;
            return Ok(None);
        }
    };
    let Some(first) = posts.first() else {
        return Ok(None);
    };
    let subject = first.subject.clone();
    let mut rows = Vec::new();
    for post in &posts {
        rows.push(format!(
            "From: {}   {}",
            post.author,
            fmt_datetime(post.created_at)
        ));
        rows.push(format!("Subj: {}", post.subject));
        if post.tombstoned {
            rows.push("(this post was removed)".to_string());
        } else {
            for body_line in post.body.lines() {
                rows.push(body_line.to_string());
            }
        }
        rows.push("-".repeat(40));
    }
    page_out(t, &rows).await?;
    Ok(Some(subject))
}

/// Longest accepted post body, in bytes (the classic editor keeps going
/// until `.`; something has to bound it).
const MAX_POST_BYTES: usize = 32 * 1024;

/// The classic line editor: subject prompt, then body lines until a lone
/// `.` — gated on `BOARD_POST` (guests never post) and the shared per-
/// account `post` budget, posting through BoardService as the logged-in
/// user (QWK-ingest author-seed derivation).
async fn post_editor<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    slug: &str,
    parent: Option<[u8; 32]>,
    default_subject: &str,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if authed.subject.role == Role::Guest
        || !shared
            .perms
            .allows(&authed.subject, &format!("board/{slug}"), Caps::BOARD_POST)
    {
        return t
            .write_str("\nYou do not have permission to post here.\n")
            .await;
    }
    if !shared.rate_allow(Scope::Account(authed.account.id), rl::POST) {
        return t.write_str("\nRate limited; slow down.\n").await;
    }
    let subject = if default_subject.is_empty() {
        t.write_str("\nSubject: ").await?;
        let Some(s) = t.read_line(Echo::On).await? else {
            return Ok(());
        };
        s.trim().to_string()
    } else {
        t.write_str(&format!("\nSubject [{default_subject}]: "))
            .await?;
        let Some(s) = t.read_line(Echo::On).await? else {
            return Ok(());
        };
        let s = s.trim();
        if s.is_empty() {
            default_subject.to_string()
        } else {
            s.to_string()
        }
    };
    t.write_str("Enter your message. End with a single `.` on its own line (`/abort` cancels).\n")
        .await?;
    let mut body = String::new();
    loop {
        let Some(line) = t.read_line(Echo::On).await? else {
            return Ok(()); // peer went away mid-edit: nothing posts
        };
        let trimmed = line.trim_end();
        if trimmed == "." {
            break;
        }
        if trimmed == "/abort" {
            return t.write_str("Post abandoned.\n").await;
        }
        if body.len() + trimmed.len() > MAX_POST_BYTES {
            t.write_str("Message too long; posting what fits.\n")
                .await?;
            break;
        }
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(trimmed);
    }
    if subject.trim().is_empty() && body.trim().is_empty() {
        return t.write_str("Empty post abandoned.\n").await;
    }
    let author = format!("{}@{}", authed.persona.screen_name, shared.origin_name());
    let seed = crate::qwk::author_seed(shared, authed.account.id);
    let now = chrono::Utc::now().timestamp_millis();
    match shared
        .boards
        .post(
            slug,
            parent,
            &author,
            &seed,
            &subject,
            &body,
            "text/plain",
            now,
        )
        .await
    {
        Ok(row) => {
            shared.bus.publish(ServerEvent::BoardPost {
                board: row.board_slug.clone(),
                id: row.event_id,
                root: row.root_id,
            });
            t.write_str("Posted.\n").await
        }
        Err(e) => t.write_str(&format!("Could not post: {e}\n")).await,
    }
}

// ---------------------------------------------------------------------------
// Live chat: scrollback, then a select! between typed lines (cancel-safe
// read_line) and bus events — the same lobby every surface shares.

/// Scrollback lines printed when entering a room.
const CHAT_SCROLLBACK: usize = 15;

/// The `[C]` chat screen for `room` (the lobby from the menu; any joinable
/// room via `/go`). Typed lines send (`CHAT_SEND` + the per-account `msg`
/// budget); `/q` leaves; incoming bus lines stream in between keystrokes.
async fn chat_shell<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    session_id: u64,
    room: &str,
) -> io::Result<Option<GoTarget>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let resource = format!("chat/{room}");
    if !shared
        .perms
        .allows(&authed.subject, &resource, Caps::CHAT_READ)
    {
        t.write_str("\nYou do not have access to chat.\n").await?;
        return Ok(None);
    }
    let is_lobby = room.eq_ignore_ascii_case(LOBBY);
    if !is_lobby {
        if let Err(e) = shared.chat.join(
            room,
            session_id,
            authed.account.id,
            &authed.persona.screen_name,
        ) {
            t.write_str(&format!("\nCannot join {room}: {e}\n")).await?;
            return Ok(None);
        }
    }
    // Subscribe before printing scrollback so nothing said in between is
    // lost (a line landing in that instant may print twice; better twice
    // than never).
    let mut rx = shared.bus.subscribe();
    t.write_str(&format!(
        "\n--- Chat: {room} ---\nType to talk. /q leaves, /go <keyword> jumps.\n"
    ))
    .await?;
    match shared.chat.history(room, session_id, CHAT_SCROLLBACK) {
        Ok(lines) if !lines.is_empty() => {
            for line in &lines {
                t.write_str(&format!("<{}> {}\n", line.from, line.text))
                    .await?;
            }
        }
        Ok(_) => t.write_str("(no recent chat)\n").await?,
        Err(e) => t.write_str(&format!("({e})\n")).await?,
    }

    let result = loop {
        tokio::select! {
            line = t.read_line(Echo::On) => {
                let Some(line) = line? else {
                    break Ok(None); // peer went away
                };
                let text = line.trim();
                if text.is_empty() {
                    continue;
                }
                if let Some(rest) = text.strip_prefix('/') {
                    let (cmd, arg) = split_command(rest);
                    match cmd.as_str() {
                        "q" | "quit" | "exit" => break Ok(None),
                        "go" => {
                            if let Some(target) = go_command(t, shared, arg).await? {
                                break Ok(Some(target));
                            }
                        }
                        _ => {
                            t.write_str("Commands: /q leaves, /go <keyword> jumps.\n")
                                .await?;
                        }
                    }
                    continue;
                }
                if !shared.rate_allow(Scope::Account(authed.account.id), rl::MSG) {
                    t.write_str("Rate limited; slow down.\n").await?;
                    continue;
                }
                if !shared
                    .perms
                    .allows(&authed.subject, &resource, Caps::CHAT_SEND)
                {
                    t.write_str("You do not have permission to speak here.\n")
                        .await?;
                    continue;
                }
                // The service's mute / slow-mode refusals (Wave 13) print as
                // the parenthesized refusal line like any other send error.
                let sender = Sender {
                    session_id,
                    account_id: authed.account.id,
                    is_moderator: shared
                        .perms
                        .allows(&authed.subject, "chat", Caps::CHAT_MODERATE),
                    screen_name: &authed.persona.screen_name,
                };
                if let Err(e) = shared.chat.send(room, sender, text, now_ms()) {
                    t.write_str(&format!("({e})\n")).await?;
                }
            }
            event = rx.recv() => {
                use tokio::sync::broadcast::error::RecvError;
                match event {
                    Ok(ServerEvent::Chat { room: r, from, text })
                        if r.eq_ignore_ascii_case(room) =>
                    {
                        t.write_str(&format!("<{from}> {text}\n")).await?;
                    }
                    Ok(ServerEvent::Shutdown) => {
                        t.write_str("The server is going down. Goodbye.\n").await?;
                        break Ok(None);
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => {
                        t.write_str("(chat resynced; some lines were missed)\n")
                            .await?;
                    }
                    Err(RecvError::Closed) => break Ok(None),
                }
            }
        }
    };
    if !is_lobby {
        let _ = shared.chat.leave(room, session_id);
    }
    result
}

// ---------------------------------------------------------------------------
// Direct mail: conversation list → paged thread → single-line replies over
// the same durable store + bus + away-auto-response path the native DM
// handlers use.

/// The `[D]` sub-shell: conversation list.
async fn dm_shell<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    peer_ip: Option<IpAddr>,
) -> io::Result<Option<GoTarget>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if authed.subject.role == Role::Guest {
        t.write_str("\nDirect mail needs a member account. Ask your sysop.\n")
            .await?;
        return Ok(None);
    }
    t.write_str(
        "\n--- Direct Mail ---\n\
         Commands: ls, <n> or <name> to open, w <name> (write), q (back), /go <keyword>\n",
    )
    .await?;
    let mut peers = print_dm_threads(t, shared, authed.account.id).await?;
    loop {
        t.write_str("\nmail> ").await?;
        let Some(line) = t.read_line(Echo::On).await? else {
            return Ok(None); // peer went away
        };
        if let Some(ip) = peer_ip {
            if !shared.rate_allow(Scope::Ip(ip), rl::LEGACY) {
                t.write_str("\nRate limited; slow down.\n").await?;
                continue;
            }
        }
        let (verb, arg) = split_command(&line);
        match verb.as_str() {
            "" => {}
            "q" | "quit" | "x" | "exit" => return Ok(None),
            "ls" | "list" => peers = print_dm_threads(t, shared, authed.account.id).await?,
            "w" | "write" | "send" => {
                if arg.is_empty() {
                    t.write_str("\nUsage: w <screen name>\n").await?;
                } else if let Some(target) = dm_thread(t, shared, authed, peer_ip, arg).await? {
                    return Ok(Some(target));
                } else {
                    peers = print_dm_threads(t, shared, authed.account.id).await?;
                }
            }
            "help" | "?" => {
                t.write_str(
                    "\nls        list conversations\n<n>/<name> open one\n\
                     w <name>  write someone new\nq         back\n\
                     /go <word> keyword teleport\n",
                )
                .await?;
            }
            "/go" => {
                if let Some(target) = go_command(t, shared, arg).await? {
                    return Ok(Some(target));
                }
            }
            _ => {
                let choice = line.trim();
                let picked = choice
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| peers.get(n.wrapping_sub(1)).cloned())
                    .or_else(|| {
                        peers
                            .iter()
                            .find(|p| p.eq_ignore_ascii_case(choice))
                            .cloned()
                    })
                    .unwrap_or_else(|| choice.to_string());
                if let Some(target) = dm_thread(t, shared, authed, peer_ip, &picked).await? {
                    return Ok(Some(target));
                }
                peers = print_dm_threads(t, shared, authed.account.id).await?;
            }
        }
    }
}

/// Print the conversation table; returns peer names for number selection.
async fn print_dm_threads<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    account_id: i64,
) -> io::Result<Vec<String>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let threads = match DmsRepo(&shared.pool).threads(account_id).await {
        Ok(list) => list,
        Err(e) => {
            t.write_str(&format!("\nThe mail store is unavailable: {e}\n"))
                .await?;
            return Ok(Vec::new());
        }
    };
    if threads.is_empty() {
        t.write_str("\nNo conversations yet — `w <name>` starts one.\n")
            .await?;
        return Ok(Vec::new());
    }
    let now = chrono::Utc::now().timestamp_millis();
    let mut rows = vec![format!(
        "  #  {:<20} {:>6}  {:<6} {}",
        "WITH", "UNREAD", "AGE", "LAST"
    )];
    let mut peers = Vec::new();
    for (i, (partner_account, last, unread)) in threads.iter().enumerate() {
        let with = if last.from_account == *partner_account {
            last.from_persona.clone()
        } else {
            last.to_persona.clone()
        };
        rows.push(format!(
            "{:>3}  {:<20} {:>6}  {:<6} {}",
            i + 1,
            clip(&with, 20),
            unread,
            fmt_age(now - last.at_ms),
            clip(last.text.lines().next().unwrap_or(""), 40)
        ));
        peers.push(with);
    }
    page_out(t, &rows).await?;
    Ok(peers)
}

/// Most messages shown per DM thread read (newest N, oldest first).
const DM_PAGE_LIMIT: i64 = 100;

/// One conversation: page it (marking it read, with receipts when the
/// account has them on), then `r` to reply.
async fn dm_thread<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    peer_ip: Option<IpAddr>,
    peer: &str,
) -> io::Result<Option<GoTarget>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if authed.subject.role == Role::Guest {
        t.write_str("\nDirect mail needs a member account. Ask your sysop.\n")
            .await?;
        return Ok(None);
    }
    let partner = match PersonasRepo(&shared.pool).by_screen_name(peer).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            t.write_str(&format!("\nNo such user: {peer}\n")).await?;
            return Ok(None);
        }
        Err(e) => {
            t.write_str(&format!("\nThe mail store is unavailable: {e}\n"))
                .await?;
            return Ok(None);
        }
    };
    if partner.account_id == authed.account.id {
        t.write_str("\nThat would be talking to yourself.\n")
            .await?;
        return Ok(None);
    }
    render_dm_history(t, shared, authed, &partner).await?;
    loop {
        t.write_str(&format!("\ndm {}> ", partner.screen_name))
            .await?;
        let Some(line) = t.read_line(Echo::On).await? else {
            return Ok(None); // peer went away
        };
        if let Some(ip) = peer_ip {
            if !shared.rate_allow(Scope::Ip(ip), rl::LEGACY) {
                t.write_str("\nRate limited; slow down.\n").await?;
                continue;
            }
        }
        let (verb, arg) = split_command(&line);
        match verb.as_str() {
            "" => {}
            "q" | "quit" | "x" | "exit" => return Ok(None),
            "ls" | "list" => render_dm_history(t, shared, authed, &partner).await?,
            "r" | "reply" => {
                t.write_str("Message: ").await?;
                let Some(text) = t.read_line(Echo::On).await? else {
                    return Ok(None);
                };
                send_dm(t, shared, authed, &partner, text.trim()).await?;
            }
            "help" | "?" => {
                t.write_str(
                    "\nls  re-read the conversation\nr   reply\nq   back\n\
                     /go <word> keyword teleport\n",
                )
                .await?;
            }
            "/go" => {
                if let Some(target) = go_command(t, shared, arg).await? {
                    return Ok(Some(target));
                }
            }
            other => {
                t.write_str(&format!("\nUnknown command: {other} (try `help`)\n"))
                    .await?;
            }
        }
    }
}

/// Print a conversation oldest-first and mark it read (publishing the read
/// receipt when the account has receipts enabled, like the native path).
async fn render_dm_history<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    partner: &rabbithole_store_server::repo2::PersonaRow,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let dms = DmsRepo(&shared.pool);
    let messages = match dms
        .thread(authed.account.id, partner.account_id, 0, DM_PAGE_LIMIT)
        .await
    {
        Ok(m) => m,
        Err(e) => {
            return t
                .write_str(&format!("\nThe mail store is unavailable: {e}\n"))
                .await;
        }
    };
    if messages.is_empty() {
        t.write_str(&format!(
            "\nNo messages with {} yet — `r` writes one.\n",
            partner.screen_name
        ))
        .await?;
        return Ok(());
    }
    let mut rows = Vec::new();
    for m in &messages {
        let auto = if m.is_auto { " (auto-reply)" } else { "" };
        rows.push(format!(
            "[{}] {}{auto}: {}",
            fmt_datetime(m.at_ms),
            m.from_persona,
            m.text
        ));
    }
    page_out(t, &rows).await?;
    // Reading marks read — and receipts fire exactly like the native path.
    let last_id = messages.last().map(|m| m.id).unwrap_or(0);
    let newly = dms
        .mark_read(authed.account.id, partner.account_id, last_id)
        .await
        .unwrap_or(0);
    if newly > 0
        && dm_receipts_enabled(&shared.pool, authed.account.id)
            .await
            .unwrap_or(false)
    {
        shared.bus.publish(ServerEvent::DmRead {
            to_account: partner.account_id,
            by: authed.persona.screen_name.clone(),
            up_to_id: last_id,
        });
    }
    Ok(())
}

/// Send one DM: `DM_SEND` + the shared `msg` budget + block checks, then
/// the durable store, the bus, and the away auto-response — the identical
/// path the native DmSend handler walks.
async fn send_dm<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    partner: &rabbithole_store_server::repo2::PersonaRow,
    text: &str,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if text.is_empty() {
        return t.write_str("Nothing sent (empty message).\n").await;
    }
    if !shared.perms.allows(&authed.subject, "dm", Caps::DM_SEND) {
        return t
            .write_str("You do not have permission to send direct messages.\n")
            .await;
    }
    if !shared.rate_allow(Scope::Account(authed.account.id), rl::MSG) {
        return t.write_str("Rate limited; slow down.\n").await;
    }
    if text.len() > shared.config.read().chat_max_len {
        return t.write_str("Message too long.\n").await;
    }
    // Blocked either way = refused, without revealing which side.
    let blocks = BlocksRepo(&shared.pool);
    let blocked = blocks
        .is_blocked(authed.account.id, partner.account_id)
        .await
        .unwrap_or(true)
        || blocks
            .is_blocked(partner.account_id, authed.account.id)
            .await
            .unwrap_or(true);
    if blocked {
        return t.write_str("You cannot message that user.\n").await;
    }
    let at_ms = chrono::Utc::now().timestamp_millis();
    let id = match DmsRepo(&shared.pool)
        .insert(
            authed.account.id,
            &authed.persona.screen_name,
            partner.account_id,
            &partner.screen_name,
            text,
            None,
            &[],
            at_ms,
            false,
            None,
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            return t
                .write_str(&format!("The mail store is unavailable: {e}\n"))
                .await;
        }
    };
    shared.bus.publish(ServerEvent::Dm {
        to_account: partner.account_id,
        message: rabbithole_proto::dm::DmMessage::new(
            id,
            authed.persona.screen_name.clone(),
            partner.screen_name.clone(),
            text,
            None,
            Vec::new(),
            at_ms,
            false,
        ),
    });
    t.write_str("Sent.\n").await?;
    // Away auto-response (once per sender→recipient away period).
    if let Err(e) = crate::handlers3::maybe_auto_respond(
        shared,
        authed.account.id,
        &authed.persona.screen_name,
        &partner.screen_name,
        partner.account_id,
    )
    .await
    {
        tracing::debug!("telnet dm auto-response failed: {e}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Small formatting helpers shared by the new screens.

/// Clip to `max` characters with an ellipsis marker.
fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// Compact age: `now`, `12m`, `5h`, `3d`.
fn fmt_age(delta_ms: i64) -> String {
    let mins = delta_ms.max(0) / 60_000;
    if mins == 0 {
        "now".to_string()
    } else if mins < 60 {
        format!("{mins}m")
    } else if mins < 60 * 24 {
        format!("{}h", mins / 60)
    } else {
        format!("{}d", mins / (60 * 24))
    }
}

/// `YYYY-MM-DD HH:MM` from unix milliseconds.
fn fmt_datetime(unix_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(unix_ms)
        .unwrap_or_default()
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::radio_now_playing_line;
    use rabbithole_server_core::presence::RadioStatus;

    fn status(
        station: &str,
        title: &str,
        artist: &str,
        dj: &str,
        listeners: usize,
        live: bool,
    ) -> RadioStatus {
        RadioStatus {
            station: station.into(),
            title: title.into(),
            artist: artist.into(),
            dj: dj.into(),
            listeners,
            live,
        }
    }

    #[test]
    fn off_air_has_no_banner() {
        assert_eq!(radio_now_playing_line(&[]), None);
    }

    #[test]
    fn automation_banner_is_ascii_and_dj_less() {
        let line =
            radio_now_playing_line(&[status("ambient", "Warren Dawn", "", "rotation", 3, false)])
                .unwrap();
        assert_eq!(line, "Now playing on ambient: Warren Dawn [3 listening]");
        assert!(line.is_ascii(), "safe on CP437/UTF-8 TTYPEs alike");
    }

    #[test]
    fn a_live_dj_is_featured_over_automation() {
        // Auto "ambient" sorts first alphabetically, but the live "night" wins.
        let line = radio_now_playing_line(&[
            status("ambient", "Drift", "Eno", "rotation", 9, false),
            status("night", "Request Hour", "The Lagomorphs", "Robin", 2, true),
        ])
        .unwrap();
        assert_eq!(
            line,
            "Now playing on night: Request Hour - The Lagomorphs (DJ Robin) [2 listening]"
        );
    }
}
