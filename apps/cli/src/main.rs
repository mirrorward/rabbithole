//! `rabbit` — the RabbitHole command-line client.
//!
//! `rabbit login` establishes a session and caches it (endpoint, pinned
//! fingerprint, resume token) in the user's data dir; every other command
//! dials with the cached session, does its work, and exits. `--json` turns
//! output machine-readable for scripting.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use rabbithole_core::Client;
use rabbithole_proto::chat::ChatMessage;
use rabbithole_proto::presence::{UserJoined, UserLeft};
use serde::{Deserialize, Serialize};

#[derive(Parser)]
#[command(name = "rabbit", version, about = "RabbitHole client", long_about = None)]
struct Cli {
    /// Machine-readable JSON output.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Dial a server and perform only the hello handshake (diagnostics).
    Hello {
        endpoint: String,
        #[arg(long)]
        fingerprint: Option<String>,
        #[arg(long)]
        server_name: Option<String>,
    },
    /// Sign in and cache the session for the other commands.
    Login {
        /// host:port (QUIC) or ws:// URL (WebSocket).
        endpoint: String,
        /// Server cert fingerprint (hex) — required for QUIC.
        #[arg(long)]
        fingerprint: Option<String>,
        #[arg(long)]
        server_name: Option<String>,
        /// Account login (omit for --guest).
        #[arg(long, conflicts_with = "guest")]
        user: Option<String>,
        /// Password (or set RABBIT_PASSWORD).
        #[arg(long)]
        password: Option<String>,
        /// Sign in as a guest.
        #[arg(long)]
        guest: bool,
        /// Guest display name.
        #[arg(long)]
        name: Option<String>,
    },
    /// Forget the cached session.
    Logout,
    /// Show the cached session.
    Status,
    /// List who's online.
    Who,
    /// Say something in the lobby.
    Say { text: Vec<String> },
    /// Print recent lobby scrollback.
    History {
        #[arg(default_value_t = 25)]
        limit: u32,
    },
    /// Stream lobby chat and presence until interrupted.
    Tail,
    /// List the board tree (with unread counts).
    Boards,
    /// List threads in a board.
    Threads {
        board: String,
        #[arg(default_value_t = 25)]
        limit: u32,
    },
    /// Post a new thread to a board.
    Post {
        board: String,
        subject: String,
        body: Vec<String>,
    },
    /// Pull the board tree and threads into the local cache and flush any
    /// replies queued while offline.
    Sync {
        /// Limit the pull to one board slug (default: all boards).
        board: Option<String>,
    },
    /// Read cached threads for a board — works with no connection.
    Read { board: String },
    /// Reply to a thread; queues to the outbox if the server is unreachable.
    Reply {
        board: String,
        /// Parent post id (hex, full or unambiguous prefix from `read`).
        parent: String,
        text: Vec<String>,
    },
    /// The Wishing Well — a request board for wanted files/boards/features.
    Wish {
        #[command(subcommand)]
        action: WishAction,
    },
    /// File libraries — browse, upload, download, search.
    File {
        #[command(subcommand)]
        action: FileAction,
    },
}

#[derive(Subcommand)]
enum FileAction {
    /// List file areas.
    Areas,
    /// List a folder's contents (path optional).
    Ls { area: String, path: Option<String> },
    /// Create a library (needs manage rights).
    Mkarea {
        slug: String,
        title: String,
        description: Vec<String>,
    },
    /// Create a folder (`--parent PATH`, `--dropbox` for write-only).
    Mkdir {
        area: String,
        name: String,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long)]
        dropbox: bool,
    },
    /// Upload a local file.
    Put {
        area: String,
        /// Local file to upload.
        local: PathBuf,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long)]
        comment: Option<String>,
    },
    /// Download a file by id to a local path.
    Get { id: i64, local: PathBuf },
    /// Show a node's metadata.
    Info { id: i64 },
    /// Delete a node.
    Rm { id: i64 },
    /// Rate a file 1..5.
    Rate { id: i64, stars: u8 },
    /// Search files by name/comment/uploader.
    Search {
        query: String,
        #[arg(long)]
        area: Option<String>,
    },
}

#[derive(Subcommand)]
enum WishAction {
    /// List wishes (optionally by status: open|claimed|fulfilled|declined).
    List {
        #[arg(long)]
        status: Option<String>,
        #[arg(default_value_t = 50)]
        limit: u32,
    },
    /// Make a wish (kind: file|board|feature|other).
    Make {
        kind: String,
        title: String,
        details: Vec<String>,
    },
    /// Toggle your vote on a wish.
    Vote { id: i64 },
    /// Claim a wish (you intend to fulfill it).
    Claim { id: i64 },
    /// Mark a wish fulfilled, with an optional link/note.
    Fulfill { id: i64, note: Vec<String> },
    /// Decline or withdraw a wish.
    Decline { id: i64 },
    /// Reopen a wish.
    Reopen { id: i64 },
}

/// The cached session (written by `login`).
#[derive(Debug, Serialize, Deserialize)]
struct Session {
    endpoint: String,
    server_name: Option<String>,
    fingerprint: Option<String>,
    /// Resume token; None = guest (re-login each invocation).
    token: Option<String>,
    guest_name: Option<String>,
    screen_name: String,
    replay_cursor: u64,
}

fn session_path() -> Result<PathBuf> {
    let dir = dirs::data_dir()
        .context("no data dir on this platform")?
        .join("rabbithole");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("session.json"))
}

fn load_session() -> Result<Session> {
    let path = session_path()?;
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("no session — run `rabbit login` first ({})", path.display()))?;
    Ok(serde_json::from_str(&raw)?)
}

fn save_session(s: &Session) -> Result<()> {
    std::fs::write(session_path()?, serde_json::to_string_pretty(s)?)?;
    Ok(())
}

const CLIENT_NAME: &str = "rabbit";

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Hello {
            endpoint,
            fingerprint,
            server_name,
        } => {
            let mut c = Client::connect(
                &endpoint,
                server_name.as_deref(),
                fingerprint.as_deref(),
                CLIENT_NAME,
                env!("CARGO_PKG_VERSION"),
            )
            .await?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "server_name": c.server.server_name,
                        "server_version": c.server.server_version,
                        "server_key": hex::encode(c.server.server_key),
                    })
                );
            } else {
                println!("connected to \"{}\"", c.server.server_name);
                println!("server software: {}", c.server.server_version);
                println!("server identity key: {}", hex::encode(c.server.server_key));
            }
            c.close().await;
            Ok(())
        }
        Cmd::Login {
            endpoint,
            fingerprint,
            server_name,
            user,
            password,
            guest,
            name,
        } => {
            let mut c = Client::connect(
                &endpoint,
                server_name.as_deref(),
                fingerprint.as_deref(),
                CLIENT_NAME,
                env!("CARGO_PKG_VERSION"),
            )
            .await?;
            let ok = if guest {
                c.auth_guest(name.clone()).await?
            } else {
                let user = user.context("--user LOGIN (or --guest)")?;
                let password = password
                    .or_else(|| std::env::var("RABBIT_PASSWORD").ok())
                    .context("--password or RABBIT_PASSWORD")?;
                c.auth_password(&user, &password).await?
            };
            let welcome = c.expect_welcome().await?;

            let session = Session {
                endpoint,
                server_name,
                fingerprint,
                token: (!ok.token.is_empty()).then(|| ok.token.clone()),
                guest_name: guest
                    .then(|| name.unwrap_or_default())
                    .filter(|s| !s.is_empty()),
                screen_name: ok.screen_name.clone(),
                replay_cursor: c.replay_cursor,
            };
            save_session(&session)?;

            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "screen_name": ok.screen_name,
                        "server": c.server.server_name,
                        "resumable": session.token.is_some(),
                        "motd": welcome.motd,
                        "agreement_pending": welcome.agreement.is_some(),
                    })
                );
            } else {
                println!(
                    "signed in to \"{}\" as {}",
                    c.server.server_name, ok.screen_name
                );
                if !welcome.motd.is_empty() {
                    println!("\n{}\n", welcome.motd);
                }
                if welcome.agreement.is_some() {
                    println!("(this server has an agreement; commands will auto-accept — read it with `rabbit status`)");
                }
            }
            c.close().await;
            Ok(())
        }
        Cmd::Logout => {
            let path = session_path()?;
            if path.exists() {
                std::fs::remove_file(&path)?;
            }
            if !cli.json {
                println!("logged out");
            }
            Ok(())
        }
        Cmd::Status => {
            let s = load_session()?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&s)?);
            } else {
                println!("endpoint:    {}", s.endpoint);
                println!("screen name: {}", s.screen_name);
                println!("resumable:   {}", s.token.is_some());
            }
            Ok(())
        }
        Cmd::Who => {
            let (mut c, _) = reconnect().await?;
            let users = c.who().await?;
            if cli.json {
                let rows: Vec<_> = users
                    .iter()
                    .map(|u| {
                        serde_json::json!({
                            "screen_name": u.screen_name,
                            "role": u.role,
                            "transport": u.transport,
                            "connected_secs": u.connected_secs,
                        })
                    })
                    .collect();
                println!("{}", serde_json::Value::Array(rows));
            } else {
                println!("{} online:", users.len());
                for u in users {
                    println!(
                        "  {:24} {:10} {:>6}s",
                        u.screen_name, u.transport, u.connected_secs
                    );
                }
            }
            c.close().await;
            Ok(())
        }
        Cmd::Say { text } => {
            let line = text.join(" ");
            if line.trim().is_empty() {
                bail!("nothing to say");
            }
            let (mut c, _) = reconnect().await?;
            c.chat_send("lobby", &line).await?;
            c.close().await;
            persist_cursor(&c);
            Ok(())
        }
        Cmd::History { limit } => {
            let (mut c, _) = reconnect().await?;
            let messages = c.chat_history("lobby", limit).await?;
            if cli.json {
                let rows: Vec<_> = messages
                    .iter()
                    .map(
                        |m| serde_json::json!({"from": m.from, "text": m.text, "at": m.at_unix_ms}),
                    )
                    .collect();
                println!("{}", serde_json::Value::Array(rows));
            } else {
                for m in messages {
                    println!("<{}> {}", m.from, m.text);
                }
            }
            c.close().await;
            Ok(())
        }
        Cmd::Tail => {
            let (mut c, _) = reconnect().await?;
            eprintln!("tailing lobby — Ctrl-C to stop");
            loop {
                tokio::select! {
                    push = c.next_push() => {
                        let Some(frame) = push? else { break };
                        if let Some(Ok(m)) = frame.decode::<ChatMessage>() {
                            if cli.json {
                                println!("{}", serde_json::json!({"type": "chat", "from": m.from, "text": m.text, "at": m.at_unix_ms}));
                            } else {
                                println!("<{}> {}", m.from, m.text);
                            }
                        } else if let Some(Ok(j)) = frame.decode::<UserJoined>() {
                            if !cli.json {
                                println!("* {} joined", j.user.screen_name);
                            }
                        } else if let Some(Ok(l)) = frame.decode::<UserLeft>() {
                            if !cli.json && !l.screen_name.is_empty() {
                                println!("* {} left", l.screen_name);
                            }
                        }
                    }
                    _ = tokio::signal::ctrl_c() => break,
                }
            }
            persist_cursor(&c);
            c.close().await;
            Ok(())
        }
        Cmd::Boards => {
            let (mut c, _) = reconnect().await?;
            let boards = c.boards().await?;
            if cli.json {
                let rows: Vec<_> = boards
                    .iter()
                    .map(|b| serde_json::json!({"slug": b.slug, "title": b.title, "kind": b.kind, "unread": b.unread}))
                    .collect();
                println!("{}", serde_json::Value::Array(rows));
            } else {
                for b in boards {
                    let kind = match b.kind {
                        0 => "category",
                        1 => "bundle",
                        _ => "board",
                    };
                    let unread = if b.unread > 0 {
                        format!("  ({} unread)", b.unread)
                    } else {
                        String::new()
                    };
                    println!("{:22} {:8} {}{}", b.slug, kind, b.title, unread);
                }
            }
            c.close().await;
            Ok(())
        }
        Cmd::Threads { board, limit } => {
            let (mut c, _) = reconnect().await?;
            let threads = c.threads(&board, limit).await?;
            if cli.json {
                let rows: Vec<_> = threads
                    .iter()
                    .map(|t| serde_json::json!({"id": hex::encode(t.root.id), "subject": t.root.subject, "author": t.root.author, "replies": t.replies}))
                    .collect();
                println!("{}", serde_json::Value::Array(rows));
            } else if threads.is_empty() {
                println!("(no threads in {board})");
            } else {
                for t in threads {
                    println!(
                        "{}  {} — {} ({} repl.)",
                        &hex::encode(t.root.id)[..8],
                        t.root.subject,
                        t.root.author,
                        t.replies
                    );
                }
            }
            c.close().await;
            Ok(())
        }
        Cmd::Post {
            board,
            subject,
            body,
        } => {
            let body = body.join(" ");
            let (mut c, _) = reconnect().await?;
            let post = c
                .post(&rabbithole_proto::board::PostCreate::new(
                    &board, &subject, &body,
                ))
                .await?;
            if cli.json {
                println!(
                    "{}",
                    serde_json::json!({"id": hex::encode(post.id), "board": post.board})
                );
            } else {
                println!("posted to {} ({})", post.board, &hex::encode(post.id)[..8]);
            }
            c.close().await;
            Ok(())
        }
        Cmd::Sync { board } => cmd_sync(cli.json, board).await,
        Cmd::Read { board } => cmd_read(cli.json, &board),
        Cmd::Reply {
            board,
            parent,
            text,
        } => cmd_reply(cli.json, board, parent, text.join(" ")).await,
        Cmd::Wish { action } => cmd_wish(cli.json, action).await,
        Cmd::File { action } => cmd_file(cli.json, action).await,
    }
}

fn file_kind(kind: u8) -> &'static str {
    match kind {
        0 => "dir",
        1 => "file",
        2 => "alias",
        _ => "?",
    }
}

async fn cmd_file(json: bool, action: FileAction) -> Result<()> {
    use rabbithole_proto::filelib as pf;
    let (mut c, _) = reconnect().await?;
    let result: Result<()> = async {
        match action {
            FileAction::Areas => {
                let areas = c.file_areas().await?;
                if json {
                    let rows: Vec<_> = areas
                        .iter()
                        .map(|a| serde_json::json!({"slug": a.slug, "title": a.title}))
                        .collect();
                    println!("{}", serde_json::Value::Array(rows));
                } else if areas.is_empty() {
                    println!("(no file areas)");
                } else {
                    for a in areas {
                        println!("{:16} {}", a.slug, a.title);
                    }
                }
            }
            FileAction::Ls { area, path } => {
                let nodes = c.folder_list(&area, path).await?;
                if json {
                    let rows: Vec<_> = nodes
                        .iter()
                        .map(|n| {
                            serde_json::json!({
                                "id": n.id, "kind": file_kind(n.kind), "name": n.name,
                                "size": n.size, "downloads": n.downloads,
                            })
                        })
                        .collect();
                    println!("{}", serde_json::Value::Array(rows));
                } else if nodes.is_empty() {
                    println!("(empty)");
                } else {
                    for n in nodes {
                        println!(
                            "{:>6}  {:5}  {:24} {:>9}  ↓{}",
                            n.id,
                            file_kind(n.kind),
                            n.name,
                            n.size,
                            n.downloads
                        );
                    }
                }
            }
            FileAction::Mkarea {
                slug,
                title,
                description,
            } => {
                let a = c.area_create(&slug, &title, &description.join(" ")).await?;
                println!("created area {}", a.slug);
            }
            FileAction::Mkdir {
                area,
                name,
                parent,
                dropbox,
            } => {
                let mut req = pf::FolderCreate::new(&area, parent, &name);
                if dropbox {
                    req = req.dropbox();
                }
                let n = c.folder_create(&req).await?;
                println!(
                    "created {} {}",
                    if dropbox { "dropbox" } else { "folder" },
                    n.path
                );
            }
            FileAction::Put {
                area,
                local,
                parent,
                comment,
            } => {
                let name = local
                    .file_name()
                    .and_then(|s| s.to_str())
                    .context("bad local file name")?
                    .to_string();
                let mime = mime_guess_simple(&name);
                // The resumable transfer engine (dedicated stream on QUIC,
                // ranged chunks on WS) — handles files of any size.
                let n = c
                    .transfer_upload(
                        &area,
                        parent,
                        &name,
                        &local,
                        mime,
                        &comment.unwrap_or_default(),
                    )
                    .await?;
                if json {
                    println!(
                        "{}",
                        serde_json::json!({"id": n.id, "path": n.path, "size": n.size})
                    );
                } else {
                    println!("uploaded {} ({} bytes) as #{}", n.path, n.size, n.id);
                }
            }
            FileAction::Get { id, local } => {
                let n = c.transfer_download(id, &local).await?;
                println!("downloaded {} bytes → {}", n, local.display());
            }
            FileAction::Info { id } => {
                let n = c.node_get(id).await?;
                if json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "id": n.id, "kind": file_kind(n.kind), "name": n.name, "path": n.path,
                            "size": n.size, "mime": n.mime, "comment": n.comment,
                            "uploader": n.uploader, "downloads": n.downloads,
                            "rating_avg": n.rating_avg, "rating_count": n.rating_count,
                        })
                    );
                } else {
                    println!("#{} {} ({})", n.id, n.name, file_kind(n.kind));
                    println!("path:     {}", n.path);
                    println!("size:     {}", n.size);
                    println!("uploader: {}", n.uploader);
                    println!("downloads:{}", n.downloads);
                    if n.rating_count > 0 {
                        println!("rating:   {:.1} ({} votes)", n.rating_avg, n.rating_count);
                    }
                    if !n.comment.is_empty() {
                        println!("comment:  {}", n.comment);
                    }
                }
            }
            FileAction::Rm { id } => {
                c.node_delete(id).await?;
                println!("deleted #{id}");
            }
            FileAction::Rate { id, stars } => {
                let n = c.rate_file(id, stars).await?;
                println!(
                    "rated #{}: {:.1} ({} votes)",
                    n.id, n.rating_avg, n.rating_count
                );
            }
            FileAction::Search { query, area } => {
                let nodes = c.file_search(area, &query, 50).await?;
                if json {
                    let rows: Vec<_> = nodes
                        .iter()
                        .map(|n| serde_json::json!({"id": n.id, "path": n.path, "area": n.area}))
                        .collect();
                    println!("{}", serde_json::Value::Array(rows));
                } else if nodes.is_empty() {
                    println!("(no matches)");
                } else {
                    for n in nodes {
                        println!("{:>6}  {}/{}", n.id, n.area, n.path);
                    }
                }
            }
        }
        Ok(())
    }
    .await;
    c.close().await;
    result
}

/// A tiny extension→MIME guess (enough for the CLI; the server stores it).
fn mime_guess_simple(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "txt" | "md" | "nfo" | "diz" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        _ => "application/octet-stream",
    }
}

/// Pull the board tree and full threads into the local cache, and flush any
/// replies queued while offline. Requires a connection.
async fn cmd_sync(json: bool, only: Option<String>) -> Result<()> {
    use rabbithole_store_client::boards::BoardCache;
    let cache = open_cache()?;
    let (mut c, _) = reconnect().await?;

    // Flush the outbox first so freshly-sent replies show in the pull below.
    let pending = BoardCache(&cache).pending()?;
    let mut flushed = 0u32;
    for entry in &pending {
        let mut req =
            rabbithole_proto::board::PostCreate::new(&entry.board, &entry.subject, &entry.body);
        req.mime = entry.mime.clone();
        if let Some(parent) = entry.parent {
            req = req.reply_to(parent);
        }
        match c.post(&req).await {
            Ok(post) => {
                BoardCache(&cache).mark_sent(entry.id, post.id)?;
                flushed += 1;
            }
            Err(e) => {
                eprintln!("outbox: reply to {} failed, left queued: {e}", entry.board);
            }
        }
    }

    // Pull the tree, then delta-pull each board's threads into the cache.
    let boards = c.boards().await?;
    BoardCache(&cache).put_boards(&boards, now_secs())?;
    let mut pulled_posts = 0u32;
    for b in &boards {
        if b.kind != 2 {
            continue; // only real boards hold posts
        }
        if let Some(slug) = &only {
            if &b.slug != slug {
                continue;
            }
        }
        let threads = c.threads(&b.slug, 200).await?;
        for t in threads {
            let posts = c.thread(t.root.id, 500).await?;
            pulled_posts += posts.len() as u32;
            BoardCache(&cache).put_posts(&posts)?;
        }
    }
    persist_cursor(&c);
    c.close().await;

    if json {
        println!(
            "{}",
            serde_json::json!({"flushed": flushed, "boards": boards.len(), "posts": pulled_posts})
        );
    } else {
        println!(
            "synced {} boards, {} posts cached{}",
            boards.len(),
            pulled_posts,
            if flushed > 0 {
                format!(", {flushed} queued replies sent")
            } else {
                String::new()
            }
        );
    }
    Ok(())
}

/// Read cached threads for a board straight from the local store — no network.
fn cmd_read(json: bool, board: &str) -> Result<()> {
    use rabbithole_store_client::boards::BoardCache;
    let cache = open_cache()?;
    let roots = BoardCache(&cache).threads(board)?;
    if json {
        let rows: Vec<_> = roots
            .iter()
            .map(|p| {
                serde_json::json!({
                    "id": hex::encode(p.id),
                    "subject": p.subject,
                    "author": p.author,
                    "tombstoned": p.tombstoned,
                })
            })
            .collect();
        println!("{}", serde_json::Value::Array(rows));
    } else if roots.is_empty() {
        println!("(nothing cached for {board} — run `rabbit sync {board}`)");
    } else {
        for p in roots {
            let subject = if p.tombstoned {
                "[deleted]"
            } else {
                &p.subject
            };
            println!("{}  {} — {}", &hex::encode(p.id)[..8], subject, p.author);
        }
    }
    Ok(())
}

/// Reply to a thread. Posts immediately when online; if the server can't be
/// reached, the reply is queued in the outbox for the next `rabbit sync`.
async fn cmd_reply(json: bool, board: String, parent: String, text: String) -> Result<()> {
    use rabbithole_store_client::boards::BoardCache;
    if text.trim().is_empty() {
        bail!("nothing to say");
    }
    let cache = open_cache()?;
    let parent_id = resolve_post_id(&cache, &board, &parent)?;

    match reconnect().await {
        Ok((mut c, _)) => {
            let req =
                rabbithole_proto::board::PostCreate::new(&board, "", &text).reply_to(parent_id);
            let post = c.post(&req).await?;
            // Cache our own reply so `read` reflects it right away.
            BoardCache(&cache).put_posts(std::slice::from_ref(&post))?;
            persist_cursor(&c);
            c.close().await;
            if json {
                println!(
                    "{}",
                    serde_json::json!({"id": hex::encode(post.id), "queued": false})
                );
            } else {
                println!("replied to {} ({})", board, &hex::encode(post.id)[..8]);
            }
        }
        Err(e) => {
            let id = BoardCache(&cache).enqueue(
                &board,
                Some(parent_id),
                "",
                &text,
                "text/plain",
                now_secs(),
            )?;
            if json {
                println!("{}", serde_json::json!({"outbox_id": id, "queued": true}));
            } else {
                println!("offline ({e}) — reply queued (#{id}); send it later with `rabbit sync`");
            }
        }
    }
    Ok(())
}

async fn cmd_wish(json: bool, action: WishAction) -> Result<()> {
    use rabbithole_proto::wish::WishSetStatus;
    let (mut c, _) = reconnect().await?;
    let result: Result<()> = async {
        match action {
            WishAction::List { status, limit } => {
                let code = status.as_deref().map(wish_status_code).transpose()?;
                let wishes = c.wishes(code, limit).await?;
                if json {
                    let rows: Vec<_> = wishes
                        .iter()
                        .map(|w| {
                            serde_json::json!({
                                "id": w.id,
                                "title": w.title,
                                "status": wish_status_label(w.status),
                                "votes": w.votes,
                                "requester": w.requester,
                                "claimed_by": w.claimed_by,
                                "fulfillment": w.fulfillment,
                            })
                        })
                        .collect();
                    println!("{}", serde_json::Value::Array(rows));
                } else if wishes.is_empty() {
                    println!("(no wishes)");
                } else {
                    for w in wishes {
                        println!(
                            "#{:<4} [{:^9}] {:>3}▲  {}  — {}",
                            w.id,
                            wish_status_label(w.status),
                            w.votes,
                            w.title,
                            w.requester
                        );
                    }
                }
            }
            WishAction::Make {
                kind,
                title,
                details,
            } => {
                let w = c
                    .wish_create(wish_kind_code(&kind)?, &title, &details.join(" "))
                    .await?;
                report_wish(json, "wished", &w);
            }
            WishAction::Vote { id } => {
                let w = c.wish_vote(id).await?;
                report_wish(json, "voted", &w);
            }
            WishAction::Claim { id } => {
                let w = c.wish_set_status(&WishSetStatus::new(id, 1)).await?;
                report_wish(json, "claimed", &w);
            }
            WishAction::Fulfill { id, note } => {
                let mut req = WishSetStatus::new(id, 2);
                if !note.is_empty() {
                    req = req.with_fulfillment(note.join(" "));
                }
                let w = c.wish_set_status(&req).await?;
                report_wish(json, "fulfilled", &w);
            }
            WishAction::Decline { id } => {
                let w = c.wish_set_status(&WishSetStatus::new(id, 3)).await?;
                report_wish(json, "declined", &w);
            }
            WishAction::Reopen { id } => {
                let w = c.wish_set_status(&WishSetStatus::new(id, 0)).await?;
                report_wish(json, "reopened", &w);
            }
        }
        Ok(())
    }
    .await;
    c.close().await;
    result
}

fn report_wish(json: bool, verb: &str, w: &rabbithole_proto::wish::WishView) {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "id": w.id,
                "title": w.title,
                "status": wish_status_label(w.status),
                "votes": w.votes,
            })
        );
    } else {
        println!(
            "{verb}: #{} \"{}\" [{}] {}▲",
            w.id,
            w.title,
            wish_status_label(w.status),
            w.votes
        );
    }
}

/// Re-establish a session from the cache: token resume for accounts,
/// fresh guest sign-in for guests. Auto-accepts a pending agreement
/// (the login command surfaced it to the human).
async fn reconnect() -> Result<(Client, Session)> {
    let mut s = load_session()?;
    let mut c = Client::connect(
        &s.endpoint,
        s.server_name.as_deref(),
        s.fingerprint.as_deref(),
        CLIENT_NAME,
        env!("CARGO_PKG_VERSION"),
    )
    .await?;
    let ok = match &s.token {
        Some(token) => c.auth_resume(token, s.replay_cursor).await?,
        None => c.auth_guest(s.guest_name.clone()).await?,
    };
    s.screen_name = ok.screen_name.clone();
    let welcome = c.expect_welcome().await?;
    if welcome.agreement.is_some() {
        c.agreement_accept().await?;
    }
    Ok((c, s))
}

/// Best-effort: remember the replay cursor for the next resume.
fn persist_cursor(c: &Client) {
    if let Ok(mut s) = load_session() {
        if s.replay_cursor < c.replay_cursor {
            s.replay_cursor = c.replay_cursor;
            let _ = save_session(&s);
        }
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Open (creating if needed) the local offline cache in the data dir.
fn open_cache() -> Result<rabbithole_store_client::Connection> {
    let dir = dirs::data_dir()
        .context("no data dir on this platform")?
        .join("rabbithole");
    std::fs::create_dir_all(&dir)?;
    Ok(rabbithole_store_client::open(&dir.join("cache.db"))?)
}

/// Parse a full or prefix hex post id against the cached posts of a board.
fn resolve_post_id(
    cache: &rabbithole_store_client::Connection,
    board: &str,
    prefix: &str,
) -> Result<[u8; 32]> {
    // A full 64-char hex id needs no lookup.
    if prefix.len() == 64 {
        let bytes = hex::decode(prefix).context("invalid hex post id")?;
        return bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("post id must be 32 bytes"));
    }
    use rabbithole_store_client::boards::BoardCache;
    let matches: Vec<[u8; 32]> = BoardCache(cache)
        .threads(board)?
        .into_iter()
        .map(|p| p.id)
        .filter(|id| hex::encode(id).starts_with(&prefix.to_lowercase()))
        .collect();
    match matches.as_slice() {
        [one] => Ok(*one),
        [] => bail!("no cached thread in {board} matches id prefix {prefix} (try `rabbit sync`)"),
        _ => bail!("ambiguous id prefix {prefix} — use more characters"),
    }
}

fn wish_status_code(name: &str) -> Result<u8> {
    Ok(match name.to_lowercase().as_str() {
        "open" => 0,
        "claimed" => 1,
        "fulfilled" => 2,
        "declined" => 3,
        other => bail!("unknown status '{other}' (open|claimed|fulfilled|declined)"),
    })
}

fn wish_kind_code(name: &str) -> Result<u8> {
    Ok(match name.to_lowercase().as_str() {
        "file" => 0,
        "board" => 1,
        "feature" => 2,
        "other" => 3,
        other => bail!("unknown kind '{other}' (file|board|feature|other)"),
    })
}

fn wish_status_label(code: u8) -> &'static str {
    match code {
        0 => "open",
        1 => "claimed",
        2 => "fulfilled",
        3 => "declined",
        _ => "?",
    }
}
