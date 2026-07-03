//! FidoNet (FTN) gateway (Wave 10): a **binkp** TCP mailer wired into `burrow`,
//! bridging classic FidoNet mail to the native community.
//!
//! Two codec crates do the heavy lifting; this module is only the *transport +
//! bridge* glue that drives them over real tokio sockets and maps their output
//! onto the shared server services:
//!
//! - [`rabbithole-legacy-binkp`](rabbithole_legacy_binkp) — the sans-IO binkp
//!   session FSM (originating + answering). [`drive_session`] pumps its
//!   [`Action`]s onto a socket and feeds decoded wire blocks back in as
//!   [`Event`]s, so the protocol logic stays in the crate.
//! - [`rabbithole-legacy-ftn`](rabbithole_legacy_ftn) — the PKT codec plus the
//!   [`Tosser`] (inbound split/dedupe) and [`scan`] (outbound BSO framing).
//!
//! # Data flow
//!
//! ```text
//!   uplink ──binkp──▶ [answering FSM] ──▶ inbound spool ──▶ Tosser ──┐
//!                                                                    ├▶ echomail → BoardService.post
//!                                                                    └▶ netmail  → DM (DmsRepo + bus)
//!
//!   local board post ─▶ [outbound scanner] ─▶ PKT ─▶ BSO outbound ──binkp──▶ uplink
//!                          (config AREA↔slug map)      (scanner naming)   (originating FSM)
//! ```
//!
//! MSGID de-duplication is the tosser's ([`TossedBatch::duplicates`]); a message
//! already tossed is never posted twice. The outbound scanner only stages
//! *locally* authored posts (`@{origin}`), so echomail injected inbound is not
//! reflected straight back — the echomail↔board loop is broken by author origin.
//!
//! Opt-in via config (`ftn_enabled`) and off by default. RBAC is respected: the
//! gateway posts/DMs only when a member-baseline subject holds the relevant
//! capability on the `board` / `dm` resource.
//!
//! ## Deliberately deferred
//!
//! - **ARCmail bundle decompression**: only raw `.PKT` files are tossed; a
//!   compressed bundle is left in the spool (logged, not an error).
//! - **Answering-side sending**: the answering FSM here receives only; queued
//!   mail is flushed by an outbound poll ([`poll_uplink`]) to the uplink.
//! - **Crash-recovery resume / `M_GET`**: whole-file offset-0 transfers only.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use rabbithole_legacy_binkp::{
    decode_block, Action, Address as BinkpAddress, Command, Event, FileInfo, FrameError, RawBlock,
    Session, SessionConfig, BLOCK_MAX,
};
use rabbithole_legacy_ftn::{
    bso_file_name, scan, BsoKind, Flavor, FtnAddress, Message as FtnMessage, PackedMessage,
    PacketHeader, Tosser,
};
use rabbithole_server_core::ratelimit::{class as rl, Scope};
use rabbithole_server_core::{Caps, Role, ServerEvent, Subject};
use rabbithole_store_server::repo2::PersonasRepo;
use rabbithole_store_server::repo3::DmsRepo;
use rabbithole_store_server::repo4::PostRow;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use crate::Shared;

/// Synthetic sender account id for netmail delivered as a DM. The `dms` table
/// has no account foreign key, so a well-known id keeps gateway mail grouped
/// without impersonating a real account.
const FTN_GATEWAY_ACCOUNT: i64 = 0;

/// Bind + serve the FTN binkp surface. Returns the bound address (useful when
/// the config asked for port 0) and the accept-loop task handle. Mirrors the
/// `spawn_nntp` / `spawn_hotline` helpers.
pub async fn spawn_ftn(
    shared: Arc<Shared>,
    addr: SocketAddr,
    inbound_dir: PathBuf,
    outbound_dir: PathBuf,
) -> Result<(SocketAddr, JoinHandle<()>)> {
    let gateway = FtnGateway::from_shared(shared, inbound_dir, outbound_dir);
    tokio::fs::create_dir_all(&gateway.inbound_dir).await?;
    tokio::fs::create_dir_all(&gateway.outbound_dir).await?;

    let listener = TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;

    // Outbound echomail scanner: local board posts -> staged BSO packets.
    tokio::spawn(outbound_scanner(gateway.clone()));

    let handle = tokio::spawn(async move {
        loop {
            let Ok((sock, peer)) = listener.accept().await else {
                break;
            };
            // Over the per-IP connection budget: drop it on the floor.
            if !gateway.shared.rate_allow(Scope::Ip(peer.ip()), rl::CONN) {
                continue;
            }
            let gateway = gateway.clone();
            tokio::spawn(async move {
                if let Err(e) = serve_inbound(gateway, sock).await {
                    tracing::debug!("ftn inbound session error: {e}");
                }
            });
        }
    });
    Ok((local, handle))
}

/// The FidoNet gateway state shared across binkp sessions and the scanner.
pub struct FtnGateway {
    shared: Arc<Shared>,
    /// This system's FTN node address (from `ftn_node`), if parseable.
    node: Option<FtnAddress>,
    /// The uplink/boss node (from `ftn_uplink`), if parseable.
    uplink: Option<FtnAddress>,
    /// Uplink binkp `host:port` to dial for outbound polls.
    uplink_host: String,
    /// binkp session password ("" / "-" = unsecured).
    password: String,
    /// `UPPERCASE(area)` → board slug.
    area_to_board: HashMap<String, String>,
    /// board slug → `area` (original case), for outbound scanning.
    board_to_area: HashMap<String, String>,
    /// Inbound spool for received PKT/bundle files.
    inbound_dir: PathBuf,
    /// Outbound BSO directory for staged PKT files.
    outbound_dir: PathBuf,
    /// Rolling MSGID dupe set, shared across all inbound sessions.
    tosser: tokio::sync::Mutex<Tosser>,
}

impl FtnGateway {
    /// Build a gateway from the live config snapshot and resolved spool dirs.
    pub fn from_shared(
        shared: Arc<Shared>,
        inbound_dir: PathBuf,
        outbound_dir: PathBuf,
    ) -> Arc<FtnGateway> {
        let cfg = shared.config.read();
        let node = cfg.ftn_node.parse::<FtnAddress>().ok();
        let uplink = cfg.ftn_uplink.parse::<FtnAddress>().ok();
        let uplink_host = cfg.ftn_uplink_host.clone();
        let password = cfg.ftn_password.clone();
        let mut area_to_board = HashMap::new();
        let mut board_to_area = HashMap::new();
        for (area, slug) in &cfg.ftn_areas {
            area_to_board.insert(area.to_uppercase(), slug.clone());
            board_to_area.insert(slug.clone(), area.clone());
        }
        drop(cfg);
        Arc::new(FtnGateway {
            shared,
            node,
            uplink,
            uplink_host,
            password,
            area_to_board,
            board_to_area,
            inbound_dir,
            outbound_dir,
            tosser: tokio::sync::Mutex::new(Tosser::new()),
        })
    }

    fn board_for_area(&self, area: &str) -> Option<&str> {
        self.area_to_board
            .get(&area.to_uppercase())
            .map(String::as_str)
    }

    fn area_for_board(&self, slug: &str) -> Option<&str> {
        self.board_to_area.get(slug).map(String::as_str)
    }

    /// Decode + toss a PKT byte buffer, filing echomail to boards and netmail
    /// to DMs. Returns `(echomail_posted, netmail_delivered)`. Non-PKT input
    /// (e.g. a compressed bundle) yields an error, which the caller logs.
    pub async fn ingest_pkt_bytes(&self, bytes: &[u8]) -> Result<(usize, usize)> {
        let batch = {
            let mut tosser = self.tosser.lock().await;
            tosser
                .toss_bytes(bytes)
                .map_err(|e| anyhow!("ftn toss: {e}"))?
        };
        let mut posted = 0;
        let mut delivered = 0;
        for em in &batch.echomail {
            if self.deliver_echomail(em).await? {
                posted += 1;
            }
        }
        for nm in &batch.netmail {
            if self.deliver_netmail(nm).await? {
                delivered += 1;
            }
        }
        Ok((posted, delivered))
    }

    /// Post one echomail message into its mapped board (if any).
    async fn deliver_echomail(&self, em: &rabbithole_legacy_ftn::tosser::EchoMail) -> Result<bool> {
        let Some(slug) = self.board_for_area(&em.area).map(str::to_string) else {
            tracing::debug!(area = %em.area, "ftn: no board mapped for echo area; skipping");
            return Ok(false);
        };
        if !self
            .shared
            .perms
            .allows(&gateway_subject(), "board", Caps::BOARD_POST)
        {
            tracing::warn!("ftn: gateway lacks BOARD_POST; echomail dropped");
            return Ok(false);
        }
        match self.shared.boards.board(&slug).await {
            Ok(Some(b)) if b.kind == 2 => {}
            _ => {
                tracing::warn!(%slug, "ftn: echo target board missing or not postable");
                return Ok(false);
            }
        }
        let from = non_empty(&em.message.from, "Unknown");
        let author = format!("{from}@fidonet");
        let seed = ftn_author_seed(&self.shared, &format!("{}|{}", em.area, from));
        let subject = non_empty(&em.message.subject, "(no subject)");
        let body = em.parsed.text_str();
        let now = chrono::Utc::now().timestamp_millis();
        match self
            .shared
            .boards
            .post(
                &slug,
                None,
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
                self.shared.stats.incr("ftn", "echomail_posts");
                self.shared.bus.publish(ServerEvent::BoardPost {
                    board: row.board_slug.clone(),
                    id: row.event_id,
                    root: row.root_id,
                });
                Ok(true)
            }
            Err(e) => {
                tracing::warn!("ftn: echomail board post failed: {e}");
                Ok(false)
            }
        }
    }

    /// Deliver one netmail message as a DM to the persona named in its `To`
    /// field (if that persona exists locally).
    async fn deliver_netmail(&self, nm: &rabbithole_legacy_ftn::tosser::NetMail) -> Result<bool> {
        let to = nm.message.to.trim();
        if to.is_empty() {
            return Ok(false);
        }
        let Some(recipient) = PersonasRepo(&self.shared.pool).by_screen_name(to).await? else {
            tracing::debug!(%to, "ftn: netmail recipient not found locally; skipping");
            return Ok(false);
        };
        if !self
            .shared
            .perms
            .allows(&gateway_subject(), "dm", Caps::DM_SEND)
        {
            tracing::warn!("ftn: gateway lacks DM_SEND; netmail dropped");
            return Ok(false);
        }
        let from = non_empty(&nm.message.from, "Unknown");
        let from_persona = format!("{from}@fidonet");
        let text = nm.parsed.text_str();
        let at = chrono::Utc::now().timestamp_millis();
        let id = DmsRepo(&self.shared.pool)
            .insert(
                FTN_GATEWAY_ACCOUNT,
                &from_persona,
                recipient.account_id,
                &recipient.screen_name,
                &text,
                None,
                &[],
                at,
                false,
            )
            .await?;
        let message = rabbithole_proto::dm::DmMessage::new(
            id,
            from_persona,
            recipient.screen_name.clone(),
            text,
            None,
            vec![],
            at,
            false,
        );
        self.shared.bus.publish(ServerEvent::Dm {
            to_account: recipient.account_id,
            message,
        });
        Ok(true)
    }

    /// If `board_id` names a *locally authored* post in a board mapped to an
    /// echo area, scan it into an outbound BSO packet. Injected echomail
    /// (author `…@fidonet`) is skipped so the gateway does not loop.
    async fn handle_local_board_post(&self, board_id: &[u8; 32]) -> Result<Option<PathBuf>> {
        let Some(post) = self
            .shared
            .boards
            .post_by_id(board_id)
            .await
            .map_err(|e| anyhow!("ftn: post lookup: {e}"))?
        else {
            return Ok(None);
        };
        let local_suffix = format!("@{}", self.shared.origin_name());
        if !post.author.ends_with(&local_suffix) {
            return Ok(None); // remote/injected content is never re-scanned
        }
        self.scan_local_post(&post).await
    }

    /// Scan a single local post into an outbound PKT staged in the BSO outbound
    /// directory (named by the scanner). Returns the staged path, or `None`
    /// when the board is unmapped or node/uplink are unconfigured.
    pub async fn scan_local_post(&self, post: &PostRow) -> Result<Option<PathBuf>> {
        let Some(area) = self.area_for_board(&post.board_slug).map(str::to_string) else {
            return Ok(None);
        };
        let (Some(node), Some(uplink)) = (self.node, self.uplink) else {
            return Ok(None);
        };
        let from = post.author.split('@').next().unwrap_or("Sysop").to_string();
        let name = non_empty(&self.shared.config.read().name, "RabbitHole");
        let model = FtnMessage {
            area: Some(area),
            kludges: vec![format!(
                "MSGID: {node} {}",
                hex::encode(&post.event_id[..4])
            )],
            text: post.body.clone().into_bytes(),
            tearline: Some("RabbitHole".to_string()),
            origin: Some(format!("{name} ({node})")),
            seen_by: vec![format!("{}/{}", node.net, node.node)],
            path: vec![format!("{}/{}", node.net, node.node)],
        };
        let mut pm = PackedMessage {
            orig_net: node.net,
            orig_node: node.node,
            dest_net: uplink.net,
            dest_node: uplink.node,
            from,
            to: "All".to_string(),
            subject: non_empty(&post.subject, "(no subject)"),
            date_time: ftn_datetime(post.created_at),
            ..Default::default()
        };
        pm.set_body(&model);

        let template = PacketHeader {
            orig_zone: node.zone,
            dest_zone: uplink.zone,
            ..Default::default()
        };
        let bundles = scan(&template, &node, [(uplink, pm)]);
        let Some(bundle) = bundles.into_iter().next() else {
            return Ok(None);
        };
        tokio::fs::create_dir_all(&self.outbound_dir).await?;
        // BSO naming: for a netmail-style packet file the `.?ut` file is the
        // packet itself (Normal flavor -> `.out`).
        let fname = bso_file_name(uplink.net, uplink.node, BsoKind::Packet, Flavor::Normal);
        let path = self.outbound_dir.join(fname);
        tokio::fs::write(&path, bundle.encode()).await?;
        tracing::info!(board = %post.board_slug, file = %path.display(), "ftn: staged outbound packet");
        Ok(Some(path))
    }
}

/// A member-baseline subject used for the gateway's RBAC checks: an ACL that
/// revokes posting/DM from ordinary members also gags the gateway.
fn gateway_subject() -> Subject {
    Subject {
        account_id: FTN_GATEWAY_ACCOUNT,
        role: Role::User,
        class_id: None,
        class_mask: 0,
        grant_mask: 0,
        revoke_mask: 0,
    }
}

/// A stable author signing seed for gateway-authored board posts, namespaced
/// away from the native per-account seed so the two never collide.
fn ftn_author_seed(shared: &Shared, key: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rabbithole-ftn-author-seed-v1");
    hasher.update(&shared.server_signing_seed);
    hasher.update(key.as_bytes());
    *hasher.finalize().as_bytes()
}

/// A short-lived CRAM-MD5 challenge derived from the server seed + wall clock
/// (no extra RNG dependency; uniqueness per session is what matters here).
fn make_challenge(shared: &Shared) -> Vec<u8> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rabbithole-binkp-challenge-v1");
    hasher.update(&shared.server_signing_seed);
    hasher.update(&nanos.to_le_bytes());
    hasher.finalize().as_bytes()[..16].to_vec()
}

/// The binkp 5D address for an FTN node, tagged with the `fidonet` domain.
fn binkp_addr(a: &FtnAddress) -> BinkpAddress {
    BinkpAddress::new(a.zone, a.net, a.node, a.point).with_domain("fidonet")
}

/// `s` if non-blank, else `fallback` — as an owned `String`.
fn non_empty(s: &str, fallback: &str) -> String {
    if s.trim().is_empty() {
        fallback.to_string()
    } else {
        s.to_string()
    }
}

/// FTS-0001 date/time field, e.g. `02 Jul 26  13:30:45`.
fn ftn_datetime(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .format("%d %b %y  %H:%M:%S")
        .to_string()
}

/// Reduce a binkp file name to a safe basename for the local spool.
fn sanitize_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
    if base.is_empty() || base == "." || base == ".." {
        "inbound.pkt".to_string()
    } else {
        base.to_string()
    }
}

/// Convert a decoded wire block into a session [`Event`].
fn block_to_event(block: RawBlock) -> Result<Event> {
    Ok(match block {
        RawBlock::Command { id, args } => {
            Event::Command(Command::parse(id, &args).map_err(|e| anyhow!("binkp command: {e}"))?)
        }
        RawBlock::Data(d) => Event::Data(d),
    })
}

/// Handle one inbound (answering) binkp connection: receive files into the
/// spool, then toss any PKTs among them.
async fn serve_inbound(gateway: Arc<FtnGateway>, stream: TcpStream) -> Result<()> {
    let name = non_empty(&gateway.shared.config.read().name, "RabbitHole");
    let mut addresses = Vec::new();
    if let Some(node) = gateway.node {
        addresses.push(binkp_addr(&node));
    }
    let password = gateway.password.clone();
    let challenge = if password.is_empty() || password == "-" {
        None
    } else {
        Some(make_challenge(&gateway.shared))
    };
    let session = Session::answering(SessionConfig {
        addresses,
        system_info: vec![format!("SYS {name}"), "VER RabbitHole/binkp".to_string()],
        password,
        challenge,
        outgoing: Vec::new(), // answering-side send is deferred (see module docs)
    });

    let received = drive_session(stream, session, HashMap::new(), &gateway.inbound_dir).await?;
    for path in received {
        match tokio::fs::read(&path).await {
            Ok(bytes) => match gateway.ingest_pkt_bytes(&bytes).await {
                Ok((echo, netmail)) if echo > 0 || netmail > 0 => {
                    tracing::info!(echo, netmail, file = %path.display(), "ftn: tossed inbound packet");
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!(file = %path.display(), "ftn: inbound file not a PKT (left in spool): {e}");
                }
            },
            Err(e) => tracing::warn!("ftn: could not read inbound file: {e}"),
        }
    }
    Ok(())
}

/// Connect to the configured uplink and flush the outbound BSO queue over an
/// originating binkp session. Files are removed after a clean session; any mail
/// the uplink sends back is tossed. Callable from a scheduler or `ctl`.
pub async fn poll_uplink(gateway: Arc<FtnGateway>) -> Result<()> {
    let Some(node) = gateway.node else {
        return Err(anyhow!("ftn: local node address not configured"));
    };
    if gateway.uplink_host.is_empty() {
        return Err(anyhow!("ftn: uplink host not configured"));
    }
    let files = collect_outbound(&gateway.outbound_dir).await;
    if files.is_empty() {
        tracing::debug!("ftn: nothing queued for uplink poll");
        return Ok(());
    }
    let stream = TcpStream::connect(&gateway.uplink_host).await?;
    let received = run_originating(
        stream,
        vec![binkp_addr(&node)],
        gateway.password.clone(),
        files.clone(),
        &gateway.inbound_dir,
    )
    .await?;

    // A clean finish means every file was acked; remove the staged copies.
    for (info, _) in &files {
        let path = gateway.outbound_dir.join(&info.name);
        if let Err(e) = tokio::fs::remove_file(&path).await {
            tracing::debug!("ftn: could not remove sent file {}: {e}", path.display());
        }
    }
    for path in received {
        if let Ok(bytes) = tokio::fs::read(&path).await {
            let _ = gateway.ingest_pkt_bytes(&bytes).await;
        }
    }
    Ok(())
}

/// Drive an **originating** binkp session over `stream`, sending `files` and
/// spooling any received files into `inbound_dir`. The mirror of the answering
/// path in [`serve_inbound`]; exposed for outbound polls and tests.
pub async fn run_originating(
    stream: TcpStream,
    addresses: Vec<BinkpAddress>,
    password: String,
    files: Vec<(FileInfo, Vec<u8>)>,
    inbound_dir: &Path,
) -> Result<Vec<PathBuf>> {
    let outgoing: HashMap<String, Vec<u8>> = files
        .iter()
        .map(|(i, b)| (i.name.clone(), b.clone()))
        .collect();
    let session = Session::originating(SessionConfig {
        addresses,
        system_info: vec!["SYS RabbitHole".to_string()],
        password,
        challenge: None,
        outgoing: files.into_iter().map(|(i, _)| i).collect(),
    });
    drive_session(stream, session, outgoing, inbound_dir).await
}

/// Read the outbound BSO directory into `(FileInfo, bytes)` pairs.
async fn collect_outbound(dir: &Path) -> Vec<(FileInfo, Vec<u8>)> {
    let mut out = Vec::new();
    let Ok(mut rd) = tokio::fs::read_dir(dir).await else {
        return out;
    };
    while let Ok(Some(entry)) = rd.next_entry().await {
        let meta = match entry.metadata().await {
            Ok(m) if m.is_file() => m,
            _ => continue,
        };
        let path = entry.path();
        let Ok(bytes) = tokio::fs::read(&path).await else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().to_string();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        out.push((FileInfo::new(name, bytes.len() as u64, mtime), bytes));
    }
    out
}

/// The socket pump shared by both roles: run `session` to completion over
/// `stream`, sending `outgoing` file bodies on demand and writing received
/// files into `inbound_dir`. Returns the paths of files received.
async fn drive_session(
    stream: TcpStream,
    mut session: Session,
    outgoing: HashMap<String, Vec<u8>>,
    inbound_dir: &Path,
) -> Result<Vec<PathBuf>> {
    let (rd, mut wr) = stream.into_split();
    let mut rd = BufReader::new(rd);
    let mut inbuf: Vec<u8> = Vec::new();
    let mut pos = 0usize;
    let mut cur_file: Option<(PathBuf, tokio::fs::File)> = None;
    let mut received: Vec<PathBuf> = Vec::new();

    let mut finished = perform_actions(
        session.start(),
        &mut wr,
        &outgoing,
        inbound_dir,
        &mut cur_file,
        &mut received,
    )
    .await?;

    while !finished {
        match decode_block(&inbuf[pos..]) {
            Ok((block, used)) => {
                pos += used;
                let event = block_to_event(block)?;
                let actions = session
                    .advance(event)
                    .map_err(|e| anyhow!("binkp session: {e}"))?;
                finished = perform_actions(
                    actions,
                    &mut wr,
                    &outgoing,
                    inbound_dir,
                    &mut cur_file,
                    &mut received,
                )
                .await?;
            }
            Err(FrameError::Incomplete) => {
                if pos > 0 {
                    inbuf.drain(..pos);
                    pos = 0;
                }
                let mut tmp = [0u8; 8192];
                let n = rd.read(&mut tmp).await?;
                if n == 0 {
                    break; // peer closed
                }
                inbuf.extend_from_slice(&tmp[..n]);
            }
            Err(e) => return Err(anyhow!("binkp frame: {e}")),
        }
    }
    let _ = wr.flush().await;
    Ok(received)
}

/// Perform a batch of session [`Action`]s against the socket + spool. Returns
/// `true` once the session has finished.
async fn perform_actions<W: AsyncWriteExt + Unpin>(
    actions: Vec<Action>,
    wr: &mut W,
    outgoing: &HashMap<String, Vec<u8>>,
    inbound_dir: &Path,
    cur_file: &mut Option<(PathBuf, tokio::fs::File)>,
    received: &mut Vec<PathBuf>,
) -> Result<bool> {
    for action in actions {
        match action {
            Action::SendCommand(cmd) => {
                let block = cmd
                    .to_block()
                    .encode()
                    .map_err(|e| anyhow!("binkp encode: {e}"))?;
                wr.write_all(&block).await?;
            }
            Action::StreamFile(info) => match outgoing.get(&info.name) {
                Some(bytes) => {
                    for chunk in bytes.chunks(BLOCK_MAX) {
                        let block = RawBlock::Data(chunk.to_vec())
                            .encode()
                            .map_err(|e| anyhow!("binkp encode: {e}"))?;
                        wr.write_all(&block).await?;
                    }
                }
                None => tracing::warn!(name = %info.name, "ftn: outbound file missing at send"),
            },
            Action::ExpectFile(info) => {
                tokio::fs::create_dir_all(inbound_dir).await?;
                let path = inbound_dir.join(sanitize_name(&info.name));
                let file = tokio::fs::File::create(&path).await?;
                *cur_file = Some((path, file));
            }
            Action::WriteData(bytes) => {
                let Some((_, file)) = cur_file.as_mut() else {
                    return Err(anyhow!("binkp: data block with no open inbound file"));
                };
                file.write_all(&bytes).await?;
            }
            Action::FileComplete(_) => {
                if let Some((path, mut file)) = cur_file.take() {
                    file.flush().await?;
                    received.push(path);
                }
            }
            Action::Authenticated => tracing::debug!("ftn: binkp session authenticated"),
            Action::Finished => {
                let _ = wr.flush().await;
                return Ok(true);
            }
            Action::Aborted(reason) => return Err(anyhow!("binkp aborted: {reason}")),
        }
    }
    wr.flush().await?;
    Ok(false)
}

/// Subscribe to board posts and stage locally authored ones for the uplink.
async fn outbound_scanner(gateway: Arc<FtnGateway>) {
    use tokio::sync::broadcast::error::RecvError;
    let mut rx = gateway.shared.bus.subscribe();
    loop {
        match rx.recv().await {
            Ok(ServerEvent::Shutdown) => break,
            Ok(ServerEvent::BoardPost { id, .. }) => {
                if let Err(e) = gateway.handle_local_board_post(&id).await {
                    tracing::debug!("ftn: outbound scan failed: {e}");
                }
            }
            Ok(_) => {}
            Err(RecvError::Lagged(n)) => {
                tracing::warn!(missed = n, "ftn outbound scanner lagged behind the bus");
            }
            Err(RecvError::Closed) => break,
        }
    }
}
