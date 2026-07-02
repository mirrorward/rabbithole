//! Burrow's telnet BBS shell: banner → login → MAIN MENU (files, doors,
//! quit).
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
//! ## The file browser and the HTTP handoff
//!
//! `files` opens a small sub-shell over the shared
//! [`FileService`](rabbithole_server_core::FileService): `ls` walks areas →
//! folders → files as a paged plain-ASCII table, `cd` moves around, and
//! `get <name>` **does not stream bytes** — telnet is a terrible transfer
//! channel — it prints an HTTP(S) handoff link
//! `<files_http_base>/files/<area>/<path>` for the caller to fetch out of
//! band. This module only mints the link; serving it belongs to the web
//! slice. With `files_http_base` unset, `get` explains transfers aren't
//! available on telnet. RBAC mirrors the Hotline surface exactly: `files`
//! root needs [`Caps::FILE_LIST`], folder listings check `FILE_LIST` on the
//! `files/<area>/<path>` resource and hide entries the caller can't
//! [`Caps::SEE`], drop-box contents stay hidden without
//! [`Caps::DROPBOX_VIEW`], and `get` needs [`Caps::FILE_DOWNLOAD`].

use std::io;
use std::net::IpAddr;
use std::sync::Arc;

use rabbithole_legacy_telnet::{Echo, Encoding, TelnetStream};
use rabbithole_server_core::ratelimit::{class as rl, Scope};
use rabbithole_server_core::{AuthedUser, Caps, Role};
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
    greet(&mut t, &authed).await?;
    menu_loop(&mut t, shared, &authed, peer_ip).await
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

/// The main menu. `[D]` appears only when door hosting is switched on, but
/// the commands always answer (with a polite refusal when disabled).
async fn menu_loop<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    peer_ip: Option<IpAddr>,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        let mut menu = String::from("\n=== MAIN MENU ===\n [F] Files\n");
        if shared.config.read().qwk_enabled {
            menu.push_str(" [M] QWK offline mail\n");
        }
        if shared.doors.enabled() {
            menu.push_str(" [D] Doors\n");
        }
        menu.push_str(" [Q] Quit\n\nCommand: ");
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
        // Lowercase only the verb — door ids are matched exactly.
        let mut words = line.split_whitespace();
        let verb = words.next().unwrap_or("").to_ascii_lowercase();
        let arg = words.next();
        match (verb.as_str(), arg) {
            ("q" | "quit" | "g" | "goodbye", _) => {
                t.write_str(&format!("\nGoodbye, {}!\n", authed.persona.screen_name))
                    .await?;
                return Ok(());
            }
            ("", _) => {}
            ("f" | "files", _) => browse_files(t, shared, authed, peer_ip).await?,
            ("m" | "qwk" | "mail", _) => qwk_packet(t, shared, authed).await?,
            ("d" | "doors", None) => list_doors(t, shared).await?,
            ("d" | "door" | "doors" | "open", Some(id)) => {
                doors::run_door(t, shared, authed, id).await?;
            }
            (other, _) => {
                t.write_str(&format!("\nUnknown command: {other}\n"))
                    .await?;
            }
        }
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
/// `get <name>`, `help`, `q`. Every command spends from the same per-IP
/// legacy budget as the main menu.
async fn browse_files<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    peer_ip: Option<IpAddr>,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if !shared
        .perms
        .allows(&authed.subject, "files", Caps::FILE_LIST)
    {
        return t
            .write_str("\nYou do not have access to the file library.\n")
            .await;
    }
    let mut cur = FileCursor::default();
    t.write_str(
        "\n--- File Library ---\n\
         Commands: ls, cd <name>, cd .., get <name>, q (back to menu)\n",
    )
    .await?;
    list_level(t, shared, authed, &cur).await?;
    loop {
        t.write_str(&format!("\nfiles {}> ", cur.location()))
            .await?;
        let Some(line) = t.read_line(Echo::On).await? else {
            return Ok(()); // peer went away
        };
        if let Some(ip) = peer_ip {
            if !shared.rate_allow(Scope::Ip(ip), rl::LEGACY) {
                t.write_str("\nRate limited; slow down.\n").await?;
                continue;
            }
        }
        // Lowercase only the verb — names are matched exactly.
        let line = line.trim();
        let (verb, arg) = match line.split_once(char::is_whitespace) {
            Some((v, rest)) => (v.to_ascii_lowercase(), rest.trim()),
            None => (line.to_ascii_lowercase(), ""),
        };
        match verb.as_str() {
            "" => {}
            "q" | "quit" | "x" | "exit" => return Ok(()),
            "help" | "?" => {
                t.write_str(
                    "\nls           list this level (paged)\n\
                     cd <name>    enter an area or folder (cd .. goes up)\n\
                     get <name>   print an HTTP link for a file\n\
                     q            back to the main menu\n",
                )
                .await?;
            }
            "ls" | "list" | "dir" => list_level(t, shared, authed, &cur).await?,
            "cd" => change_dir(t, shared, authed, &mut cur, arg).await?,
            "get" => hand_off(t, shared, authed, &cur, arg).await?,
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

/// `get`: authorize like a Hotline download (FILE_DOWNLOAD + the drop-box
/// rule), then print the HTTP handoff link — or explain that transfers
/// aren't available on telnet when `files_http_base` is unset.
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
        _ => return t.write_str(&format!("\nNo such file: {arg}\n")).await,
    };
    let target = match shared.files.resolve(node.id).await {
        Ok(n) => n,
        Err(_) => return t.write_str(&format!("\nNo such file: {arg}\n")).await,
    };
    if target.kind != rabbithole_server_core::files::KIND_FILE {
        return t
            .write_str(&format!("\n{arg} is a folder — `cd` into it instead.\n"))
            .await;
    }
    let resource = file_resource(&target.area, &target.path);
    if !shared
        .perms
        .allows(&authed.subject, &resource, Caps::FILE_DOWNLOAD)
    {
        return t
            .write_str("\nYou do not have permission to download that file.\n")
            .await;
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
        return t
            .write_str("\nYou do not have permission to download that file.\n")
            .await;
    }
    let base = shared.config.read().files_http_base;
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() {
        return t
            .write_str(
                "\nFile transfers are not available on telnet; ask your sysop \
                 about the web interface.\n",
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
