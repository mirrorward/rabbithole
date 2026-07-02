//! Hotline-compatible server surface (Wave 7.3): a TCP listener that speaks the
//! classic **Hotline** binary protocol so vintage Hotline clients can join the
//! same community as native RabbitHole, telnet, and NNTP users.
//!
//! The wire codec lives in [`rabbithole-legacy-hotline`](rabbithole_legacy_hotline)
//! — handshake, the 20-byte transaction header, TLV parameter fields, and
//! fragment reassembly. This module is the *bridge*: it drives a live socket
//! through that codec and maps Hotline transactions onto the shared server
//! services already used by every other door:
//!
//! - [`AuthService`](rabbithole_server_core::AuthService) — the obfuscated
//!   Hotline login (transaction 107) is de-obfuscated and checked against real
//!   accounts; empty credentials fall through to a guest sign-in when the
//!   server allows guests.
//! - [`PresenceRegistry`](rabbithole_server_core::PresenceRegistry) — a
//!   logged-in Hotline client joins presence with transport `"hotline"`, so it
//!   shows up in the native who-list, finger, and every other Hotline client's
//!   user list. Join/leave/rename flow over the [`EventBus`] as user-list
//!   deltas (transactions 301/302).
//! - [`ChatService`](rabbithole_server_core::ChatService) — public chat
//!   (transaction 105 → 106) is bridged to the lobby room, so a line typed in a
//!   Hotline client lands in the same room as native and telnet chat and vice
//!   versa.
//!
//! Private instant messages (transaction 108) are routed directly between
//! online Hotline clients through the per-server [`Hub`].
//!
//! # Scope
//!
//! The core subset — **login, presence, public chat, and IM** — plus the
//! Wave 7.4 additions: **threaded news** (transactions 370-411) and **flat
//! news** (101/102) bridged onto the [`BoardService`](rabbithole_server_core::BoardService),
//! and **file transactions** (200-213) bridged onto the
//! [`FileService`](rabbithole_server_core::FileService) — directory browse
//! (GetFileNameList), file info (GetFileInfo), and download negotiation
//! (DownloadFile) whose bulk bytes ride the classic **HTXF** data channel
//! (control port + 1) as a flattened file object (INFO + DATA forks).
//!
//! The account-admin slice adds the classic **admin transactions**:
//!
//! - **NewUser/DeleteUser/GetUser/SetUser (350-353)** — Hotline account
//!   records (obfuscated login/password, name, 64-bit access bitmap) mapped
//!   onto the shared [`AuthService`](rabbithole_server_core::AuthService)
//!   accounts and RBAC classes. The access bitmap is a *projection* of
//!   RabbitHole roles/capabilities — see [`access_mask_for`] and
//!   [`role_for_access`] for the exact (documented, lossy) mapping.
//!   DeleteUser is a **soft delete**: the account is disabled (it can no
//!   longer log in and reads as absent on this surface) rather than having
//!   its row destroyed; hard removal is a follow-up.
//! - **DisconnectUser (110)** — kick via the same
//!   [`ServerEvent::Kick`] bus path the native admin Kick uses (same
//!   capability + role-ordering checks); the optional ban rides an
//!   **in-memory temporary ban list** (see [`Hub`]; persisting bans across
//!   restarts is a documented follow-up). The kicked client receives a
//!   DisconnectMsg (111) before its connection closes.
//! - **UserBroadcast (355)** — admin broadcast published as
//!   [`ServerEvent::Notice`], the same shared path native broadcasts use, so
//!   native/telnet clients see it too; Hotline clients receive it as a
//!   ServerMsg (104).
//!
//! All admin operations are audit-logged with the same conventions as the
//! native admin family.
//!
//! Still deferred (tolerated with an empty success reply so probing clients
//! keep working): HTXF **upload** and fork-offset **resume**, folder
//! downloads, and private chat rooms.
//! The listener is opt-in via config (`hotline_enabled`) and off by default.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use parking_lot::Mutex;
use rabbithole_blobs::BlobId;
use rabbithole_identity::hash_password;
use rabbithole_legacy_hotline::constants::{field, transaction};
use rabbithole_legacy_hotline::{read_int, Field, Handshake, HandshakeReply, Reassembler};
use rabbithole_legacy_hotline::{AccessMask, Privilege, Transaction, TransactionHeader};
use rabbithole_server_core::chat::LOBBY;
use rabbithole_server_core::files::{KIND_ALIAS, KIND_FILE, KIND_FOLDER};
use rabbithole_server_core::{AuthError, Caps, PresenceEntry, Role, ServerEvent, Subject};
use rabbithole_store_server::repo::{Account, AccountsRepo, AuditRepo};
use rabbithole_store_server::repo4::{BoardRow, PostRow};
use rabbithole_store_server::repo6::FileNodeRow;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::Shared;

/// Field id for the packed "user name with info" record returned in the user
/// name list (transaction 300). Each value is a fixed little struct:
/// `id(2) icon(2) flags(2) name_len(2) name(name_len)`, all big-endian. This is
/// the classic Hotline user-list encoding real clients parse.
const FIELD_USER_NAME_WITH_INFO: u16 = 300;

/// Default Hotline icon id when a client doesn't send one (the classic "person"
/// icon). Also used for non-Hotline (native/telnet) users in the user list.
const DEFAULT_ICON: u16 = 128;

/// Server version advertised in the login reply (classic clients accept any).
const SERVER_VERSION: u32 = 151;

/// `Options` value marking a server message as a private instant message.
const OPTIONS_IM: u32 = 1;

/// Largest single frame body we will read off the wire before giving up on a
/// connection (belt-and-suspenders; reassembly enforces the real body cap).
const MAX_FRAME_BODY: usize = 16 * 1024 * 1024;

/// How long a DisconnectUser ban lasts (the classic "temporary ban"). The ban
/// list is in-memory only — it does not survive a server restart. Persisting
/// bans (and exposing an unban admin op) is a documented follow-up.
const TEMP_BAN: Duration = Duration::from_secs(30 * 60);

/// The per-server Hotline hub: the set of currently-connected Hotline clients,
/// keyed by their wire user id, used to route private instant messages and to
/// answer icon lookups for the user list. A field of [`Shared`] alongside
/// `radio`.
#[derive(Default)]
pub struct Hub {
    inner: Mutex<HashMap<u32, ClientHandle>>,
    /// Staged file downloads keyed by reference number: a client negotiates a
    /// download on the control channel (transaction 202), then opens the HTXF
    /// data channel and quotes the reference to pull the pre-built flattened
    /// file object. Consumed (removed) on the HTXF read.
    transfers: Mutex<HashMap<u32, Vec<u8>>>,
    /// Monotonic reference-number source for staged transfers.
    next_ref: AtomicU32,
    /// Temporary ban list: namespaced key (`"login:<name>"` / `"ip:<addr>"`)
    /// to the instant the ban expires. In-memory only (see [`TEMP_BAN`]);
    /// expired entries are pruned lazily on lookup.
    bans: Mutex<HashMap<String, Instant>>,
}

/// A connected Hotline client's routing handle.
struct ClientHandle {
    /// Pre-encoded transactions to write to this client (IM delivery).
    tx: mpsc::UnboundedSender<Vec<u8>>,
    /// The client's current icon id (for the user list).
    icon: u16,
    /// The client's remote IP (for the DisconnectUser ban option).
    ip: Option<IpAddr>,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stage a flattened file object for HTXF pickup; returns its reference
    /// number (never zero).
    fn stage_download(&self, ffo: Vec<u8>) -> u32 {
        let refnum = self
            .next_ref
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        self.transfers.lock().insert(refnum, ffo);
        refnum
    }

    /// Take (and remove) a staged download by reference number.
    fn take_download(&self, refnum: u32) -> Option<Vec<u8>> {
        self.transfers.lock().remove(&refnum)
    }

    fn register(&self, id: u32, tx: mpsc::UnboundedSender<Vec<u8>>, icon: u16, ip: Option<IpAddr>) {
        self.inner.lock().insert(id, ClientHandle { tx, icon, ip });
    }

    fn set_icon(&self, id: u32, icon: u16) {
        if let Some(h) = self.inner.lock().get_mut(&id) {
            h.icon = icon;
        }
    }

    fn unregister(&self, id: u32) {
        self.inner.lock().remove(&id);
    }

    /// This client's icon, or the default (native users and unknown ids).
    fn icon_for(&self, id: u32) -> u16 {
        self.inner
            .lock()
            .get(&id)
            .map(|h| h.icon)
            .unwrap_or(DEFAULT_ICON)
    }

    /// Deliver a pre-encoded transaction to a client by id; `false` if the
    /// target isn't a connected Hotline client.
    fn deliver(&self, id: u32, bytes: Vec<u8>) -> bool {
        match self.inner.lock().get(&id) {
            Some(h) => h.tx.send(bytes).is_ok(),
            None => false,
        }
    }

    /// The remote IP of a connected Hotline client, if known.
    fn ip_of(&self, id: u32) -> Option<IpAddr> {
        self.inner.lock().get(&id).and_then(|h| h.ip)
    }

    /// Add a temporary ban entry (see [`TEMP_BAN`]). Keys are namespaced:
    /// `"login:<lowercase login>"` or `"ip:<addr>"`.
    fn ban(&self, key: String) {
        self.bans.lock().insert(key, Instant::now() + TEMP_BAN);
    }

    /// Whether `key` is currently banned; expired entries are pruned.
    fn is_banned(&self, key: &str) -> bool {
        let mut bans = self.bans.lock();
        match bans.get(key) {
            Some(&until) if until > Instant::now() => true,
            Some(_) => {
                bans.remove(key);
                false
            }
            None => false,
        }
    }
}

/// Bind + serve the Hotline surface. Returns the bound address (useful when the
/// config asked for port 0) and the accept-loop task handle. Mirrors the
/// telnet/finger/nntp/radio spawn helpers.
pub async fn spawn_hotline(
    shared: Arc<Shared>,
    addr: SocketAddr,
) -> Result<(SocketAddr, JoinHandle<()>)> {
    let mut listener = TcpListener::bind(addr).await?;
    let mut local = listener.local_addr()?;

    // The classic HTXF bulk-transfer channel is the control port + 1. With a
    // fixed control port (5500 -> 5501) a bind failure there just disables
    // downloads. With an OS-assigned control port (port 0, as tests use), the
    // adjacent port is frequently already owned — Windows/macOS hand out
    // ephemeral ports sequentially — and clients that derive port+1 would then
    // reach a stranger's socket (early EOF mid-download). So when the caller
    // asked for port 0, re-roll BOTH ports together until port+1 also binds.
    let mut htxf_listener = None;
    for _ in 0..16 {
        let htxf_addr = SocketAddr::new(local.ip(), local.port().wrapping_add(1));
        match TcpListener::bind(htxf_addr).await {
            Ok(l) => {
                htxf_listener = Some(l);
                break;
            }
            Err(e) if addr.port() == 0 => {
                tracing::debug!("hotline HTXF bind on {htxf_addr} taken ({e}); re-rolling pair");
                listener = TcpListener::bind(addr).await?;
                local = listener.local_addr()?;
            }
            Err(e) => {
                tracing::warn!("hotline HTXF bind on {htxf_addr} failed: {e}; downloads disabled");
                break;
            }
        }
    }
    if htxf_listener.is_none() && addr.port() == 0 {
        tracing::warn!("hotline HTXF pair bind kept failing; downloads disabled");
    }

    let handle = tokio::spawn(async move {
        let ctrl_shared = shared.clone();
        let ctrl = async move {
            loop {
                let Ok((sock, _peer)) = listener.accept().await else {
                    break;
                };
                let shared = ctrl_shared.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve(sock, shared).await {
                        tracing::debug!("hotline session error: {e}");
                    }
                });
            }
        };
        let data = async move {
            let Some(htxf) = htxf_listener else { return };
            loop {
                let Ok((sock, _peer)) = htxf.accept().await else {
                    break;
                };
                let shared = shared.clone();
                tokio::spawn(async move {
                    if let Err(e) = serve_htxf(sock, shared).await {
                        tracing::debug!("hotline htxf error: {e}");
                    }
                });
            }
        };
        tokio::join!(ctrl, data);
    });
    Ok((local, handle))
}

/// Serve one HTXF (bulk data) connection: read the 16-byte transfer header
/// (`"HTXF" ref(4) size(4) rsvd(4)`), then stream the staged flattened file
/// object for that reference number. Download only — upload and fork-offset
/// resume are deferred (see the module scope note).
async fn serve_htxf(sock: TcpStream, shared: Arc<Shared>) -> Result<()> {
    sock.set_nodelay(true).ok();
    let (rd, mut wr) = sock.into_split();
    let mut rd = BufReader::new(rd);
    let mut hdr = [0u8; 16];
    rd.read_exact(&mut hdr).await?;
    if &hdr[0..4] != b"HTXF" {
        return Ok(()); // not a transfer handshake; drop the connection
    }
    let refnum = u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
    if let Some(bytes) = shared.hotline.take_download(refnum) {
        wr.write_all(&bytes).await?;
    }
    // Gracefully shut the write half down (FIN) rather than letting the socket
    // drop close it. On Windows a bare drop can RST the connection and discard
    // still-buffered bytes, so the client sees an early EOF mid-download; an
    // explicit shutdown drains the send buffer first. (`flush` alone does not
    // wait for delivery on a TCP stream.)
    wr.shutdown().await?;
    // shutdown() alone is still not enough on Windows: closesocket right after
    // shutdown(SD_SEND) can abort delivery of in-flight data. The documented
    // pattern is shutdown -> keep receiving until the peer's FIN (read == 0) ->
    // close, so drain the read half (bounded) before dropping the socket.
    let mut sink = [0u8; 1024];
    let drain = async {
        while let Ok(n) = rd.read(&mut sink).await {
            if n == 0 {
                break;
            }
        }
    };
    let _ = tokio::time::timeout(std::time::Duration::from_secs(30), drain).await;
    Ok(())
}

/// The mutable per-connection state once a client is logged in.
struct Active {
    /// Shared server session id (presence/chat membership key).
    session_id: u64,
    /// Hotline wire user id (the low 32 bits of `session_id`).
    user_id: u32,
    /// Permission subject captured at login.
    subject: Subject,
    /// Account login (audit-log actor for admin operations).
    login: String,
    /// Current display name (updatable via SetClientUserInfo).
    screen_name: String,
    /// Current icon id.
    icon: u16,
    /// Whether the agreement (if any) has been accepted.
    agreed: bool,
}

/// The accept handler for one Hotline client.
async fn serve(sock: tokio::net::TcpStream, shared: Arc<Shared>) -> Result<()> {
    sock.set_nodelay(true).ok();
    let peer_ip = sock.peer_addr().ok().map(|a| a.ip());
    let (rd, mut wr) = sock.into_split();
    let mut rd = BufReader::new(rd);

    // 1. Handshake: read the 12-byte TRTP/HOTL frame, reply with 8-byte OK.
    let mut hs = [0u8; Handshake::LEN];
    rd.read_exact(&mut hs).await?;
    Handshake::decode(&hs)?; // validates the TRTP/HOTL magic
    wr.write_all(&HandshakeReply::ok().encode()).await?;

    // 2. Spawn a dedicated reader task. It owns the read half + reassembler and
    //    is never cancelled, so partial frames can't be lost when the main loop
    //    selects on other sources. Completed transactions arrive on `rx_in`.
    let (tx_in, mut rx_in) = mpsc::unbounded_channel::<Transaction>();
    tokio::spawn(read_loop(rd, tx_in));

    // 3. Wait for the login transaction (tolerating early keep-alives).
    let login_txn = loop {
        match rx_in.recv().await {
            Some(t) if t.header.type_ == transaction::LOGIN => break t,
            Some(t) if t.header.type_ == transaction::KEEP_ALIVE => {
                wr.write_all(&empty_reply(transaction::KEEP_ALIVE, t.header.id))
                    .await?;
            }
            Some(_) => {} // ignore pre-login noise
            None => return Ok(()),
        }
    };

    // De-obfuscate the login/password (each byte is bitwise-complemented on the
    // wire) and read the optional name/icon the client supplied inline.
    let login = field_text_deobf(&login_txn, field::LOGIN);
    let password = field_text_deobf(&login_txn, field::PASSWORD);
    let want_name = field_text(&login_txn, field::USER_NAME).filter(|s| !s.trim().is_empty());
    let icon = field_int(&login_txn, field::USER_ICON_ID)
        .map(|v| v as u16)
        .unwrap_or(DEFAULT_ICON);

    // Guest sign-in when both credentials are empty; otherwise a real login.
    let guests = shared.config.read().guest_enabled;
    let authed = if login.is_empty() && password.is_empty() {
        shared.auth.login_guest(guests, want_name.as_deref()).await
    } else {
        shared.auth.login_password(&login, &password, None).await
    };
    let authed = match authed {
        Ok(u) => u,
        Err(e) => {
            tracing::debug!("hotline login failed: {e}");
            wr.write_all(
                &Transaction::reply(
                    transaction::LOGIN,
                    login_txn.header.id,
                    1,
                    vec![Field::text(field::ERROR_TEXT, "login failed")],
                )
                .encode(),
            )
            .await?;
            wr.shutdown().await.ok();
            return Ok(());
        }
    };

    // Temporary ban gate (DisconnectUser's ban option): refuse banned
    // accounts and banned client addresses.
    let login_banned = shared
        .hotline
        .is_banned(&format!("login:{}", authed.account.login.to_lowercase()));
    let ip_banned = peer_ip
        .map(|ip| shared.hotline.is_banned(&format!("ip:{ip}")))
        .unwrap_or(false);
    if login_banned || ip_banned {
        wr.write_all(
            &Transaction::reply(
                transaction::LOGIN,
                login_txn.header.id,
                1,
                vec![Field::text(field::ERROR_TEXT, "you are banned")],
            )
            .encode(),
        )
        .await?;
        wr.shutdown().await.ok();
        return Ok(());
    }

    let screen_name = want_name.unwrap_or_else(|| authed.persona.screen_name.clone());
    let session_id = shared.next_session_id();
    let user_id = session_id as u32;

    // 4. Login reply: success carries a server version + name, plus the
    //    session's access bitmap (projected from role + capabilities) so real
    //    clients enable/grey their admin menus correctly.
    let access = access_mask_for(authed.subject.role, authed.subject.base_caps());
    let server_name = shared.config.read().name;
    wr.write_all(
        &Transaction::reply(
            transaction::LOGIN,
            login_txn.header.id,
            0,
            vec![
                Field::int(field::VERSION, SERVER_VERSION),
                Field::text(field::SERVER_NAME, &server_name),
                Field::new(field::USER_ACCESS, access.to_bytes().to_vec()),
            ],
        )
        .encode(),
    )
    .await?;

    // Show the agreement if one is configured; the client answers AGREED (121).
    let agreement = shared.config.read().agreement;
    let agreed = agreement.is_empty();
    if !agreement.is_empty() {
        wr.write_all(
            &Transaction::request(
                transaction::SHOW_AGREEMENT,
                0,
                vec![Field::text(field::DATA, &agreement)],
            )
            .encode(),
        )
        .await?;
    }

    // 5. Join the shared world. Subscribe to the bus BEFORE joining presence so
    //    no roster delta falls into a gap.
    let mut bus_rx = shared.bus.subscribe();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    shared.hotline.register(user_id, out_tx, icon, peer_ip);
    shared.presence.join(PresenceEntry {
        session_id,
        account_id: authed.account.id,
        screen_name: screen_name.clone(),
        role: Role::from_ordinal(authed.account.role),
        transport: "hotline".into(),
        connected_at: Instant::now(),
        state: 0,
        status: None,
    });
    shared.chat.join_lobby(session_id, &screen_name);
    tracing::info!(user_id, name = %screen_name, "hotline client logged in");

    let mut active = Active {
        session_id,
        user_id,
        subject: authed.subject,
        login: authed.account.login.clone(),
        screen_name,
        icon,
        agreed,
    };

    // 6. The active loop: client requests, bus deltas, and routed IMs.
    let result: Result<()> = async {
        loop {
            tokio::select! {
                inbound = rx_in.recv() => {
                    let Some(txn) = inbound else { break };
                    handle_txn(&mut wr, &txn, &shared, &mut active).await?;
                }
                ev = bus_rx.recv() => {
                    match ev {
                        Ok(ServerEvent::Shutdown) => break,
                        // An operator kicked this session (native admin Kick
                        // or Hotline DisconnectUser both land here): deliver
                        // the classic DisconnectMsg, then close.
                        Ok(ServerEvent::Kick { session_id: target, reason }) if target == active.session_id => {
                            let bytes = Transaction::request(
                                transaction::DISCONNECT_MSG,
                                0,
                                vec![Field::text(field::DATA, &reason)],
                            )
                            .encode();
                            let _ = wr.write_all(&bytes).await;
                            break;
                        }
                        Ok(event) => {
                            if let Some(bytes) = project_event(&shared, &event) {
                                wr.write_all(&bytes).await?;
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(user_id, missed = n, "hotline client lagged behind the bus");
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                out = out_rx.recv() => {
                    match out {
                        Some(bytes) => wr.write_all(&bytes).await?,
                        None => break,
                    }
                }
            }
        }
        Ok(())
    }
    .await;

    // 7. Leave the shared world (publishes the roster delete), then shut the
    //    write half down gracefully (FIN) so buffered bytes — e.g. a final
    //    DisconnectMsg — drain instead of being discarded by an RST.
    shared.hotline.unregister(user_id);
    shared.chat.session_closed(session_id);
    shared.presence.leave(session_id);
    wr.shutdown().await.ok();
    tracing::info!(user_id, "hotline client disconnected");
    result
}

/// The reader task: decode frames off the wire into complete transactions.
///
/// Owns the read half and the [`Reassembler`]; runs until EOF or a protocol
/// error, then drops `tx` to signal the main loop.
async fn read_loop<R>(mut rd: R, tx: mpsc::UnboundedSender<Transaction>)
where
    R: AsyncReadExt + Unpin,
{
    let mut re = Reassembler::new();
    loop {
        let mut hdr = [0u8; TransactionHeader::LEN];
        if rd.read_exact(&mut hdr).await.is_err() {
            break;
        }
        let Ok(header) = TransactionHeader::decode(&hdr) else {
            break;
        };
        let data_size = header.data_size as usize;
        if data_size > MAX_FRAME_BODY {
            break;
        }
        let mut body = vec![0u8; data_size];
        if rd.read_exact(&mut body).await.is_err() {
            break;
        }
        match re.push(&header, &body) {
            Ok(Some(txn)) => {
                if tx.send(txn).is_err() {
                    break;
                }
            }
            Ok(None) => {}
            Err(_) => break,
        }
    }
}

/// Handle one client request, writing any reply/broadcast side effects.
async fn handle_txn(
    wr: &mut OwnedWriteHalf,
    txn: &Transaction,
    shared: &Arc<Shared>,
    active: &mut Active,
) -> Result<()> {
    match txn.header.type_ {
        transaction::KEEP_ALIVE => {
            wr.write_all(&empty_reply(transaction::KEEP_ALIVE, txn.header.id))
                .await?;
        }

        transaction::AGREED => {
            active.agreed = true;
        }

        transaction::SET_CLIENT_USER_INFO => {
            if let Some(name) = field_text(txn, field::USER_NAME).filter(|s| !s.trim().is_empty()) {
                active.screen_name = name;
            }
            if let Some(new_icon) = field_int(txn, field::USER_ICON_ID) {
                active.icon = new_icon as u16;
            }
            shared.hotline.set_icon(active.user_id, active.icon);
            // Republish identity so every other client's user list updates
            // (rename always emits a change event, even for the same name).
            shared
                .presence
                .rename(active.session_id, &active.screen_name);
        }

        transaction::GET_USER_NAME_LIST => {
            let mut fields = Vec::new();
            for e in shared.presence.snapshot() {
                if e.state == 3 {
                    continue; // hide invisible (Cheshire) users
                }
                let uid = e.session_id as u32;
                let icon = shared.hotline.icon_for(uid);
                let flags = flags_for_state(e.state);
                fields.push(Field::new(
                    FIELD_USER_NAME_WITH_INFO,
                    pack_user(uid, icon, flags, &e.screen_name),
                ));
            }
            wr.write_all(
                &Transaction::reply(transaction::GET_USER_NAME_LIST, txn.header.id, 0, fields)
                    .encode(),
            )
            .await?;
        }

        transaction::CHAT_SEND => {
            let text = field_text(txn, field::CHAT_TEXT).unwrap_or_default();
            let text = text.trim_end_matches(['\r', '\n']).to_string();
            let permitted = active.agreed
                && shared
                    .perms
                    .allows(&active.subject, "chat/lobby", Caps::CHAT_SEND);
            if permitted {
                // The bus broadcast (ServerEvent::Chat) fans the line out to
                // every subscriber — native, telnet, and Hotline alike.
                let _ = shared
                    .chat
                    .send(LOBBY, active.session_id, &active.screen_name, &text);
            }
            // ChatSend is a notify in the classic protocol: no reply expected.
        }

        transaction::SEND_INSTANT_MSG => {
            let target = field_int(txn, field::USER_ID).unwrap_or(0);
            let text = field_text(txn, field::DATA).unwrap_or_default();
            let msg = Transaction::request(
                transaction::SERVER_MSG,
                0,
                vec![
                    Field::int(field::USER_ID, active.user_id),
                    Field::text(field::USER_NAME, &active.screen_name),
                    Field::int(field::OPTIONS, OPTIONS_IM),
                    Field::new(field::DATA, text.into_bytes()),
                ],
            )
            .encode();
            let delivered = shared.hotline.deliver(target, msg);
            let (err, fields) = if delivered {
                (0, Vec::new())
            } else {
                (
                    1,
                    vec![Field::text(field::ERROR_TEXT, "user not available")],
                )
            };
            wr.write_all(
                &Transaction::reply(transaction::SEND_INSTANT_MSG, txn.header.id, err, fields)
                    .encode(),
            )
            .await?;
        }

        // ---- Threaded news (boards subsystem) ----------------------------
        transaction::GET_NEWS_CAT_NAME_LIST => {
            wr.write_all(&news_cat_name_list(shared, active, txn).await.encode())
                .await?;
        }
        transaction::GET_NEWS_ART_NAME_LIST => {
            wr.write_all(&news_art_name_list(shared, active, txn).await.encode())
                .await?;
        }
        transaction::GET_NEWS_ART_DATA => {
            wr.write_all(&news_art_data(shared, active, txn).await.encode())
                .await?;
        }
        transaction::POST_NEWS_ART => {
            wr.write_all(&post_news_art(shared, active, txn).await.encode())
                .await?;
        }
        transaction::DEL_NEWS_ART => {
            wr.write_all(&del_news_art(shared, active, txn).await.encode())
                .await?;
        }

        // ---- Flat news (a chosen board projected as one message board) ---
        transaction::GET_MESSAGES => {
            wr.write_all(&get_msgs(shared, active, txn).await.encode())
                .await?;
        }
        transaction::NEW_MESSAGE | transaction::OLD_POST_NEWS => {
            wr.write_all(&post_msg(shared, active, txn).await.encode())
                .await?;
        }

        // ---- Files (browse / info / download negotiation) ----------------
        transaction::GET_FILE_NAME_LIST => {
            wr.write_all(&get_file_name_list(shared, active, txn).await.encode())
                .await?;
        }
        transaction::GET_FILE_INFO => {
            wr.write_all(&get_file_info(shared, active, txn).await.encode())
                .await?;
        }
        transaction::DOWNLOAD_FILE => {
            wr.write_all(&download_file(shared, active, txn).await.encode())
                .await?;
        }

        // ---- Account admin + kick/ban + broadcast -------------------------
        transaction::NEW_USER => {
            wr.write_all(&new_user(shared, active, txn).await.encode())
                .await?;
        }
        transaction::DELETE_USER => {
            wr.write_all(&delete_user(shared, active, txn).await.encode())
                .await?;
        }
        transaction::GET_USER => {
            wr.write_all(&get_user(shared, active, txn).await.encode())
                .await?;
        }
        transaction::SET_USER => {
            wr.write_all(&set_user(shared, active, txn).await.encode())
                .await?;
        }
        transaction::DISCONNECT_USER => {
            wr.write_all(&disconnect_user(shared, active, txn).await.encode())
                .await?;
        }
        transaction::USER_BROADCAST => {
            wr.write_all(&user_broadcast(shared, active, txn).encode())
                .await?;
        }

        // Deferred transaction families (uploads/folder-download/private-chat):
        // reply with a bare success so a probing client keeps working.
        _ => {
            wr.write_all(&empty_reply(txn.header.type_, txn.header.id))
                .await?;
        }
    }
    Ok(())
}

// ========================================================================
// Threaded + flat news, mapped onto the board service
// ========================================================================
//
// Mapping: a **board** is a news **category** when it is postable (kind 2) and
// a news **bundle** (folder) otherwise (kind 0/1). The board tree is walked by
// slug: a news path's components are board slugs, so the last component names
// the category/bundle whose children (GetNewsCatNameList) or articles
// (GetNewsArtNameList) are being requested. A board post is a news **article**;
// a reply is a child article. Article ids are a stable projection of the post's
// 32-byte event id into the classic 32-bit article id.

/// Project a post's 32-byte event id into a classic 32-bit news article id.
/// Zero is reserved by the protocol (a new top-level article), so it maps to 1.
fn art_id(event_id: &[u8; 32]) -> u32 {
    let v = u32::from_be_bytes([event_id[0], event_id[1], event_id[2], event_id[3]]);
    if v == 0 {
        1
    } else {
        v
    }
}

/// A best-effort 8-byte classic Hotline date (`year(2) ms(2) seconds(4)`).
/// Clients display it rather than compute with it, so the low 32 bits of the
/// Unix epoch seconds in the `seconds` slot is enough; year/ms stay zero.
fn hotline_date(unix_ms: i64) -> [u8; 8] {
    let secs = (unix_ms / 1000) as u32;
    let mut out = [0u8; 8];
    out[4..8].copy_from_slice(&secs.to_be_bytes());
    out
}

/// A per-account author signing seed — the same derivation the native board
/// handlers use, so a Hotline post is indistinguishable from a native one.
fn author_seed(shared: &Shared, account_id: i64) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rabbithole-author-seed-v1");
    hasher.update(&shared.server_signing_seed);
    hasher.update(&account_id.to_le_bytes());
    *hasher.finalize().as_bytes()
}

/// An error reply carrying human-readable text (error code 1).
fn err_reply(type_: u16, id: u32, msg: &str) -> Transaction {
    Transaction::reply(type_, id, 1, vec![Field::text(field::ERROR_TEXT, msg)])
}

/// Decode a structured Hotline path (`FilePath`/`NewsPath`): a 2-byte count
/// then, per component, `rsvd(2) len(1) name(len)`. Returns the component
/// names in order.
fn parse_path(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    if bytes.len() < 2 {
        return out;
    }
    let count = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
    let mut pos = 2;
    for _ in 0..count {
        if pos + 3 > bytes.len() {
            break;
        }
        let len = bytes[pos + 2] as usize; // skip 2 rsvd bytes, read length
        pos += 3;
        if pos + len > bytes.len() {
            break;
        }
        out.push(String::from_utf8_lossy(&bytes[pos..pos + len]).into_owned());
        pos += len;
    }
    out
}

/// Metadata for one article row in a `NewsArtListData` blob.
struct ArtMeta {
    id: u32,
    parent: u32,
    date: [u8; 8],
    title: String,
    poster: String,
    size: usize,
}

fn art_meta(row: &PostRow, parent: u32) -> ArtMeta {
    ArtMeta {
        id: art_id(&row.event_id),
        parent,
        date: hotline_date(row.created_at),
        title: row.subject.clone(),
        poster: row.author.clone(),
        size: row.body.len(),
    }
}

/// Pack a `NewsCategoryListData15` record (field 323): `type(2) count(2)`
/// then, for a category, `guid(16) add_sn(4) delete_sn(4)`, then the
/// length-prefixed name. Type 3 = category (holds articles), 2 = bundle.
fn pack_news_cat(is_category: bool, count: u16, name: &str) -> Vec<u8> {
    let name_b = name.as_bytes();
    let name_len = name_b.len().min(u8::MAX as usize);
    let mut out = Vec::with_capacity(32 + name_len);
    let type_ = if is_category { 3u16 } else { 2u16 };
    out.extend_from_slice(&type_.to_be_bytes());
    out.extend_from_slice(&count.to_be_bytes());
    if is_category {
        out.extend_from_slice(&[0u8; 16]); // GUID
        out.extend_from_slice(&0u32.to_be_bytes()); // add serial
        out.extend_from_slice(&0u32.to_be_bytes()); // delete serial
    }
    out.push(name_len as u8);
    out.extend_from_slice(&name_b[..name_len]);
    out
}

/// Pack a `NewsArtListData` blob (field 321): `id(4)` then length-prefixed
/// name and description, a 4-byte article count, then each article record.
fn pack_news_art_list(name: &str, desc: &str, arts: &[ArtMeta]) -> Vec<u8> {
    fn push_len_str(out: &mut Vec<u8>, s: &str) {
        let b = s.as_bytes();
        let n = b.len().min(u8::MAX as usize);
        out.push(n as u8);
        out.extend_from_slice(&b[..n]);
    }
    let mut out = Vec::new();
    out.extend_from_slice(&0u32.to_be_bytes()); // list id (root)
    push_len_str(&mut out, name);
    push_len_str(&mut out, desc);
    out.extend_from_slice(&(arts.len() as u32).to_be_bytes());
    for a in arts {
        out.extend_from_slice(&a.id.to_be_bytes());
        out.extend_from_slice(&a.date);
        out.extend_from_slice(&a.parent.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // flags
        out.extend_from_slice(&1u16.to_be_bytes()); // flavor count
        push_len_str(&mut out, &a.title);
        push_len_str(&mut out, &a.poster);
        // one flavor: text/plain, then the article size.
        let flav = b"text/plain";
        out.push(flav.len() as u8);
        out.extend_from_slice(flav);
        out.extend_from_slice(&(a.size.min(u16::MAX as usize) as u16).to_be_bytes());
    }
    out
}

/// GetNewsCatNameList (370): the child categories/bundles of the board named by
/// the last news-path component (or the top-level boards for an empty path).
async fn news_cat_name_list(
    shared: &Arc<Shared>,
    active: &Active,
    txn: &Transaction,
) -> Transaction {
    let ty = transaction::GET_NEWS_CAT_NAME_LIST;
    let id = txn.header.id;
    if !shared
        .perms
        .allows(&active.subject, "board", Caps::BOARD_READ)
    {
        return err_reply(ty, id, "not permitted");
    }
    let path = field_bytes(txn, field::NEWS_PATH)
        .map(parse_path)
        .unwrap_or_default();
    let boards = match shared.boards.boards().await {
        Ok(b) => b,
        Err(_) => return err_reply(ty, id, "news unavailable"),
    };
    let parent = path.last().map(String::as_str);
    let mut fields = Vec::new();
    for b in &boards {
        let is_child = match parent {
            Some(p) => b.parent_slug.as_deref() == Some(p),
            None => b.parent_slug.is_none(),
        };
        if !is_child {
            continue;
        }
        // Count is a display hint only; left 0 to avoid a query per child.
        fields.push(Field::new(
            field::NEWS_CAT_LIST_DATA_15,
            pack_news_cat(b.kind == 2, 0, &b.slug),
        ));
    }
    Transaction::reply(ty, id, 0, fields)
}

/// GetNewsArtNameList (371): the flattened article list (threads + replies) of
/// the category named by the last news-path component.
async fn news_art_name_list(
    shared: &Arc<Shared>,
    active: &Active,
    txn: &Transaction,
) -> Transaction {
    let ty = transaction::GET_NEWS_ART_NAME_LIST;
    let id = txn.header.id;
    if !shared
        .perms
        .allows(&active.subject, "board", Caps::BOARD_READ)
    {
        return err_reply(ty, id, "not permitted");
    }
    let path = field_bytes(txn, field::NEWS_PATH)
        .map(parse_path)
        .unwrap_or_default();
    let Some(slug) = path.last() else {
        return err_reply(ty, id, "no news path");
    };
    let board = match shared.boards.board(slug).await {
        Ok(Some(b)) => b,
        Ok(None) => return err_reply(ty, id, "no such category"),
        Err(_) => return err_reply(ty, id, "news unavailable"),
    };
    let mut arts = Vec::new();
    if let Ok(threads) = shared.boards.threads(slug, 200).await {
        for (root, _replies, _last) in &threads {
            arts.push(art_meta(root, 0));
            if let Ok(posts) = shared.boards.thread(&root.event_id, 1000).await {
                for p in &posts {
                    if p.event_id == root.event_id {
                        continue;
                    }
                    let parent = p.parent_id.map(|pid| art_id(&pid)).unwrap_or(0);
                    arts.push(art_meta(p, parent));
                }
            }
        }
    }
    let data = pack_news_art_list(&board.title, &board.description, &arts);
    Transaction::reply(ty, id, 0, vec![Field::new(field::NEWS_ART_LIST_DATA, data)])
}

/// Find the post in `slug` whose projected article id equals `art`.
async fn find_post_by_art(shared: &Arc<Shared>, slug: &str, art: u32) -> Option<PostRow> {
    let threads = shared.boards.threads(slug, 200).await.ok()?;
    for (root, _, _) in &threads {
        let posts = shared.boards.thread(&root.event_id, 1000).await.ok()?;
        for p in posts {
            if art_id(&p.event_id) == art {
                return Some(p);
            }
        }
    }
    None
}

/// GetNewsArtData (400): one article's title/poster/date/body and its
/// threading links.
async fn news_art_data(shared: &Arc<Shared>, active: &Active, txn: &Transaction) -> Transaction {
    let ty = transaction::GET_NEWS_ART_DATA;
    let id = txn.header.id;
    if !shared
        .perms
        .allows(&active.subject, "board", Caps::BOARD_READ)
    {
        return err_reply(ty, id, "not permitted");
    }
    let path = field_bytes(txn, field::NEWS_PATH)
        .map(parse_path)
        .unwrap_or_default();
    let Some(slug) = path.last() else {
        return err_reply(ty, id, "no news path");
    };
    let art = field_int(txn, field::NEWS_ART_ID).unwrap_or(0);
    let Some(post) = find_post_by_art(shared, slug, art).await else {
        return err_reply(ty, id, "no such article");
    };
    let parent = post.parent_id.map(|p| art_id(&p)).unwrap_or(0);
    Transaction::reply(
        ty,
        id,
        0,
        vec![
            Field::text(field::NEWS_ART_TITLE, &post.subject),
            Field::text(field::NEWS_ART_POSTER, &post.author),
            Field::new(field::NEWS_ART_DATE, hotline_date(post.created_at).to_vec()),
            Field::int(field::NEWS_ART_PREV, 0),
            Field::int(field::NEWS_ART_NEXT, 0),
            Field::int(field::NEWS_ART_PARENT, parent),
            Field::int(field::NEWS_ART_FIRST_CHILD, 0),
            Field::text(field::NEWS_ART_DATA_FLAV, "text/plain"),
            Field::new(field::NEWS_ART_DATA, post.body.into_bytes()),
            Field::int(field::NEWS_ART_FLAGS, 0),
        ],
    )
}

/// PostNewsArt (410): post an article (a new thread when the parent article id
/// is 0, otherwise a reply) to the category named by the news path.
async fn post_news_art(shared: &Arc<Shared>, active: &Active, txn: &Transaction) -> Transaction {
    let ty = transaction::POST_NEWS_ART;
    let id = txn.header.id;
    if active.subject.role == Role::Guest
        || !active.agreed
        || !shared
            .perms
            .allows(&active.subject, "board", Caps::BOARD_POST)
    {
        return err_reply(ty, id, "not permitted");
    }
    let path = field_bytes(txn, field::NEWS_PATH)
        .map(parse_path)
        .unwrap_or_default();
    let Some(slug) = path.last() else {
        return err_reply(ty, id, "no news path");
    };
    let parent_art = field_int(txn, field::NEWS_ART_ID).unwrap_or(0);
    let title = field_text(txn, field::NEWS_ART_TITLE).unwrap_or_default();
    let body = field_text(txn, field::NEWS_ART_DATA).unwrap_or_default();
    let parent = if parent_art == 0 {
        None
    } else {
        match find_post_by_art(shared, slug, parent_art).await {
            Some(p) => Some(p.event_id),
            None => return err_reply(ty, id, "no such parent article"),
        }
    };
    let author = format!("{}@{}", active.screen_name, shared.origin_name());
    let seed = author_seed(shared, active.subject.account_id);
    let now = chrono::Utc::now().timestamp_millis();
    match shared
        .boards
        .post(
            slug,
            parent,
            &author,
            &seed,
            &title,
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
            Transaction::reply(ty, id, 0, Vec::new())
        }
        Err(e) => err_reply(ty, id, &format!("post failed: {e}")),
    }
}

/// DelNewsArt (411): tombstone an article (author or board moderator).
async fn del_news_art(shared: &Arc<Shared>, active: &Active, txn: &Transaction) -> Transaction {
    let ty = transaction::DEL_NEWS_ART;
    let id = txn.header.id;
    let path = field_bytes(txn, field::NEWS_PATH)
        .map(parse_path)
        .unwrap_or_default();
    let Some(slug) = path.last() else {
        return err_reply(ty, id, "no news path");
    };
    let art = field_int(txn, field::NEWS_ART_ID).unwrap_or(0);
    let Some(post) = find_post_by_art(shared, slug, art).await else {
        return err_reply(ty, id, "no such article");
    };
    let is_author = post.author.starts_with(&format!("{}@", active.screen_name));
    if !is_author
        && !shared
            .perms
            .allows(&active.subject, "board", Caps::BOARD_MODERATE)
    {
        return err_reply(ty, id, "not permitted");
    }
    match shared.boards.tombstone(post.event_id).await {
        Ok(()) => Transaction::reply(ty, id, 0, Vec::new()),
        Err(e) => err_reply(ty, id, &format!("delete failed: {e}")),
    }
}

/// The board projected as the classic flat message board: the first postable
/// board (lowest in the board listing).
async fn flat_board(shared: &Arc<Shared>) -> Option<BoardRow> {
    shared
        .boards
        .boards()
        .await
        .ok()?
        .into_iter()
        .find(|b| b.kind == 2)
}

/// The flat-news text blob: each thread root as a titled entry, newest first,
/// separated by the classic divider line.
async fn flat_news_text(shared: &Arc<Shared>, slug: &str) -> String {
    let mut out = String::new();
    if let Ok(threads) = shared.boards.threads(slug, 100).await {
        for (root, _, _) in &threads {
            if !out.is_empty() {
                out.push_str("\r_________________________________________\r\r");
            }
            out.push_str(&format!(
                "{}\rFrom: {}\r\r{}\r",
                root.subject, root.author, root.body
            ));
        }
    }
    out
}

/// GetMsgs (101): the flat message board as one text field.
async fn get_msgs(shared: &Arc<Shared>, active: &Active, txn: &Transaction) -> Transaction {
    let ty = transaction::GET_MESSAGES;
    let id = txn.header.id;
    if !shared
        .perms
        .allows(&active.subject, "board", Caps::BOARD_READ)
    {
        return err_reply(ty, id, "not permitted");
    }
    let text = match flat_board(shared).await {
        Some(b) => flat_news_text(shared, &b.slug).await,
        None => String::new(),
    };
    Transaction::reply(ty, id, 0, vec![Field::new(field::DATA, text.into_bytes())])
}

/// PostMsg (102/103): post a top-level article to the flat message board; the
/// first line becomes the subject.
async fn post_msg(shared: &Arc<Shared>, active: &Active, txn: &Transaction) -> Transaction {
    let ty = txn.header.type_;
    let id = txn.header.id;
    if active.subject.role == Role::Guest
        || !active.agreed
        || !shared
            .perms
            .allows(&active.subject, "board", Caps::BOARD_POST)
    {
        return err_reply(ty, id, "not permitted");
    }
    let Some(board) = flat_board(shared).await else {
        return err_reply(ty, id, "no message board");
    };
    let body = field_text(txn, field::DATA).unwrap_or_default();
    if body.trim().is_empty() {
        return err_reply(ty, id, "empty message");
    }
    let subject: String = body
        .lines()
        .next()
        .unwrap_or("(no subject)")
        .chars()
        .take(60)
        .collect();
    let author = format!("{}@{}", active.screen_name, shared.origin_name());
    let seed = author_seed(shared, active.subject.account_id);
    let now = chrono::Utc::now().timestamp_millis();
    match shared
        .boards
        .post(
            &board.slug,
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
            shared.bus.publish(ServerEvent::BoardPost {
                board: row.board_slug.clone(),
                id: row.event_id,
                root: row.root_id,
            });
            Transaction::reply(ty, id, 0, Vec::new())
        }
        Err(e) => err_reply(ty, id, &format!("post failed: {e}")),
    }
}

// ========================================================================
// Files, mapped onto the file-library service (+ HTXF download channel)
// ========================================================================

/// The ACL resource string for an area/path, matching the native handlers.
fn file_resource(area: &str, path: Option<&str>) -> String {
    match path {
        Some(p) if !p.is_empty() => format!("files/{area}/{p}"),
        _ => format!("files/{area}"),
    }
}

/// The classic 4-char type code and reported size for a node.
fn node_type_size(n: &FileNodeRow) -> ([u8; 4], u32) {
    let size = n.size.max(0) as u32;
    if n.kind == KIND_FOLDER {
        (*b"fldr", 0)
    } else if n.kind == KIND_ALIAS {
        (*b"alis", size)
    } else if n.mime.starts_with("text") {
        (*b"TEXT", size)
    } else {
        (*b"BINA", size)
    }
}

/// Pack a `FileNameWithInfo` record (field 200): `type(4) creator(4) size(4)
/// rsvd(4) name_script(2) name_len(2) name`.
fn pack_file_info(type_code: &[u8; 4], creator: &[u8; 4], size: u32, name: &str) -> Vec<u8> {
    let name_b = name.as_bytes();
    let name_len = name_b.len().min(u16::MAX as usize);
    let mut out = Vec::with_capacity(20 + name_len);
    out.extend_from_slice(type_code);
    out.extend_from_slice(creator);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(&[0u8; 4]); // rsvd
    out.extend_from_slice(&0u16.to_be_bytes()); // name script
    out.extend_from_slice(&(name_len as u16).to_be_bytes());
    out.extend_from_slice(&name_b[..name_len]);
    out
}

/// Build a flattened file object (FFO): a `FILP` header, an `INFO` fork
/// (platform/type/creator/name/comment), and a `DATA` fork carrying the raw
/// bytes. This is what the HTXF channel streams for a whole-file download.
fn build_ffo(
    name: &str,
    comment: &str,
    type_code: &[u8; 4],
    creator: &[u8; 4],
    data: &[u8],
) -> Vec<u8> {
    let name_b = name.as_bytes();
    let comment_b = comment.as_bytes();
    let name_len = name_b.len().min(u16::MAX as usize);
    let comment_len = comment_b.len().min(u16::MAX as usize);

    // INFO fork body.
    let mut info = Vec::new();
    info.extend_from_slice(b"AMAC"); // platform
    info.extend_from_slice(type_code);
    info.extend_from_slice(creator);
    info.extend_from_slice(&0u32.to_be_bytes()); // flags
    info.extend_from_slice(&0u32.to_be_bytes()); // platform flags
    info.extend_from_slice(&[0u8; 32]); // rsvd
    info.extend_from_slice(&[0u8; 8]); // create date
    info.extend_from_slice(&[0u8; 8]); // modify date
    info.extend_from_slice(&0u16.to_be_bytes()); // name script
    info.extend_from_slice(&(name_len as u16).to_be_bytes());
    info.extend_from_slice(&name_b[..name_len]);
    info.extend_from_slice(&(comment_len as u16).to_be_bytes());
    info.extend_from_slice(&comment_b[..comment_len]);

    let mut out = Vec::with_capacity(24 + 16 + info.len() + 16 + data.len());
    // FILP header.
    out.extend_from_slice(b"FILP");
    out.extend_from_slice(&1u16.to_be_bytes()); // version
    out.extend_from_slice(&[0u8; 16]); // rsvd
    out.extend_from_slice(&2u16.to_be_bytes()); // fork count (INFO + DATA)
                                                // INFO fork header + body.
    out.extend_from_slice(b"INFO");
    out.extend_from_slice(&0u32.to_be_bytes()); // compression type
    out.extend_from_slice(&0u32.to_be_bytes()); // rsvd
    out.extend_from_slice(&(info.len() as u32).to_be_bytes());
    out.extend_from_slice(&info);
    // DATA fork header + body.
    out.extend_from_slice(b"DATA");
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(data);
    out
}

/// GetFileNameList (200): browse a directory. An empty path lists the file
/// areas as folders; otherwise the first path component is the area slug and
/// the rest is the folder path within it.
async fn get_file_name_list(
    shared: &Arc<Shared>,
    active: &Active,
    txn: &Transaction,
) -> Transaction {
    let ty = transaction::GET_FILE_NAME_LIST;
    let id = txn.header.id;
    let path = field_bytes(txn, field::FILE_PATH)
        .map(parse_path)
        .unwrap_or_default();

    if path.is_empty() {
        if !shared
            .perms
            .allows(&active.subject, "files", Caps::FILE_LIST)
        {
            return err_reply(ty, id, "not permitted");
        }
        let areas = match shared.files.areas().await {
            Ok(a) => a,
            Err(_) => return err_reply(ty, id, "files unavailable"),
        };
        let fields = areas
            .iter()
            .map(|a| {
                Field::new(
                    field::FILE_NAME_WITH_INFO,
                    pack_file_info(b"fldr", b"RBBT", 0, &a.slug),
                )
            })
            .collect();
        return Transaction::reply(ty, id, 0, fields);
    }

    let area = &path[0];
    let folder = (path.len() > 1).then(|| path[1..].join("/"));
    let resource = file_resource(area, folder.as_deref());
    if !shared
        .perms
        .allows(&active.subject, &resource, Caps::FILE_LIST)
    {
        return err_reply(ty, id, "not permitted");
    }
    // A drop box hides its contents unless the caller can view drop boxes.
    if let Some(f) = folder.as_deref() {
        if let Ok(Some(node)) = shared.files.node_by_path(area, f).await {
            if node.is_dropbox
                && !shared
                    .perms
                    .allows(&active.subject, &resource, Caps::DROPBOX_VIEW)
            {
                return Transaction::reply(ty, id, 0, Vec::new());
            }
        }
    }
    let nodes = match shared.files.list(area, folder.as_deref()).await {
        Ok(n) => n,
        Err(e) => return err_reply(ty, id, &format!("{e}")),
    };
    let mut fields = Vec::new();
    for n in &nodes {
        // Hide entries the caller can't even SEE.
        if !shared.perms.allows(
            &active.subject,
            &file_resource(area, Some(&n.path)),
            Caps::SEE,
        ) {
            continue;
        }
        let (tc, size) = node_type_size(n);
        fields.push(Field::new(
            field::FILE_NAME_WITH_INFO,
            pack_file_info(&tc, b"RBBT", size, &n.name),
        ));
    }
    Transaction::reply(ty, id, 0, fields)
}

/// Resolve `(area, full_path)` from a FILE_NAME + FILE_PATH request pair.
fn file_target(txn: &Transaction) -> Option<(String, String)> {
    let name = field_text(txn, field::FILE_NAME).unwrap_or_default();
    let path = field_bytes(txn, field::FILE_PATH)
        .map(parse_path)
        .unwrap_or_default();
    let area = path.first()?.clone();
    let folder = if path.len() > 1 {
        path[1..].join("/")
    } else {
        String::new()
    };
    let full = if folder.is_empty() {
        name
    } else if name.is_empty() {
        folder
    } else {
        format!("{folder}/{name}")
    };
    if full.is_empty() {
        return None;
    }
    Some((area, full))
}

/// GetFileInfo (206): a node's name, type, size, comment, and dates.
async fn get_file_info(shared: &Arc<Shared>, active: &Active, txn: &Transaction) -> Transaction {
    let ty = transaction::GET_FILE_INFO;
    let id = txn.header.id;
    let Some((area, full)) = file_target(txn) else {
        return err_reply(ty, id, "no file path");
    };
    let resource = file_resource(&area, Some(&full));
    if !shared
        .perms
        .allows(&active.subject, &resource, Caps::FILE_LIST)
    {
        return err_reply(ty, id, "not permitted");
    }
    let node = match shared.files.node_by_path(&area, &full).await {
        Ok(Some(n)) => n,
        Ok(None) => return err_reply(ty, id, "no such file"),
        Err(e) => return err_reply(ty, id, &format!("{e}")),
    };
    let (tc, size) = node_type_size(&node);
    let type_str = if node.kind == KIND_FOLDER {
        "folder".to_string()
    } else if node.mime.is_empty() {
        "file".to_string()
    } else {
        node.mime.clone()
    };
    Transaction::reply(
        ty,
        id,
        0,
        vec![
            Field::text(field::FILE_NAME, &node.name),
            Field::new(field::FILE_TYPE, tc.to_vec()),
            Field::text(field::FILE_TYPE_STRING, &type_str),
            Field::int(field::FILE_SIZE, size),
            Field::new(
                field::FILE_CREATE_DATE,
                hotline_date(node.created_at).to_vec(),
            ),
            Field::new(
                field::FILE_MODIFY_DATE,
                hotline_date(node.created_at).to_vec(),
            ),
            Field::text(field::FILE_COMMENT, &node.comment),
        ],
    )
}

/// DownloadFile (202): authorize + stage a whole-file download, returning the
/// HTXF reference number and transfer/data sizes. The bytes themselves are
/// pulled over the HTXF channel (see [`serve_htxf`]).
async fn download_file(shared: &Arc<Shared>, active: &Active, txn: &Transaction) -> Transaction {
    let ty = transaction::DOWNLOAD_FILE;
    let id = txn.header.id;
    let Some((area, full)) = file_target(txn) else {
        return err_reply(ty, id, "no file path");
    };
    let node = match shared.files.node_by_path(&area, &full).await {
        Ok(Some(n)) => n,
        Ok(None) => return err_reply(ty, id, "no such file"),
        Err(e) => return err_reply(ty, id, &format!("{e}")),
    };
    // Follow one alias hop to the real file.
    let target = match shared.files.resolve(node.id).await {
        Ok(t) => t,
        Err(e) => return err_reply(ty, id, &format!("{e}")),
    };
    if target.kind != KIND_FILE {
        return err_reply(ty, id, "not a file");
    }
    let resource = file_resource(&target.area, Some(&target.path));
    if !shared
        .perms
        .allows(&active.subject, &resource, Caps::FILE_DOWNLOAD)
    {
        return err_reply(ty, id, "not permitted");
    }
    // Drop-boxed content is not downloadable without view/manage rights.
    let in_dropbox = shared.files.in_dropbox(&target).await.unwrap_or(false);
    if in_dropbox
        && !shared
            .perms
            .allows(&active.subject, &resource, Caps::DROPBOX_VIEW)
        && !shared.perms.allows(
            &active.subject,
            &file_resource(&target.area, None),
            Caps::FILE_MANAGE,
        )
    {
        return err_reply(ty, id, "not permitted");
    }
    let Some(blob_id) = target.blob_id else {
        return err_reply(ty, id, "no content");
    };
    let served = match shared.files.record_download(node.id).await {
        Ok(s) => s,
        Err(e) => return err_reply(ty, id, &format!("{e}")),
    };
    let blobs = shared.blobs.clone();
    let bytes = match tokio::task::spawn_blocking(move || blobs.get(&BlobId(blob_id))).await {
        Ok(Ok(b)) => b,
        _ => return err_reply(ty, id, "blob unavailable"),
    };
    let (tc, _) = node_type_size(&served);
    let ffo = build_ffo(&served.name, &served.comment, &tc, b"RBBT", &bytes);
    let data_size = bytes.len() as u32;
    let transfer_size = ffo.len() as u32;
    let refnum = shared.hotline.stage_download(ffo);
    Transaction::reply(
        ty,
        id,
        0,
        vec![
            Field::int(field::REF_NUM, refnum),
            Field::int(field::TRANSFER_SIZE, transfer_size),
            Field::int(field::FILE_SIZE, data_size),
            Field::int(field::WAITING_COUNT, 0),
        ],
    )
}

// ========================================================================
// Account admin (350-355) + kick/ban (110), mapped onto the shared
// account service and RBAC classes
// ========================================================================
//
// ## The role <-> AccessMask projection
//
// Hotline accounts carry a 64-bit privilege bitmap; RabbitHole accounts carry
// a role + class + per-account capability masks. The two are bridged by a
// documented projection (see [`access_mask_for`] / [`role_for_access`]):
//
// - **GetUser** projects the account's effective base capabilities (role
//   default | class mask | grants, minus revokes) into the classic bitmap:
//   admin capability -> create/delete/open/modify-users bits, kick -> the
//   disconnect bit, board caps -> the news bits, file caps -> the file bits,
//   and so on. Every account additionally gets *show in list*; real (non-
//   guest) accounts get *change own password* and *use any name*.
// - **NewUser/SetUser** map an incoming bitmap back to the **nearest role**
//   (and its same-named class): any user-admin bit -> admin; any moderation
//   bit (disconnect / news-delete / close-chat) -> moderator; any member bit
//   (post news / upload / download / private messages / open chat) -> member;
//   otherwise guest.
//
// **Documented lossy cases**: individual bit grants beyond a role's default
// set are dropped (they are not translated into per-account grant/revoke
// masks — a follow-up); the superuser role projects as a full bitmap but can
// never be *assigned* from a bitmap (SetUser never up- or downgrades a
// superuser); DeleteUser is a soft delete (the login stays reserved).

/// Fire-and-forget audit record, same conventions as the native admin family.
fn audit(shared: &Arc<Shared>, actor: &str, action: &str, detail: String) {
    let pool = shared.pool.clone();
    let actor = actor.to_string();
    let action = action.to_string();
    tokio::spawn(async move {
        let _ = AuditRepo(&pool).record(&actor, &action, &detail).await;
    });
}

/// Project a role + effective capability mask into the classic Hotline
/// 64-bit access bitmap. See the section docs above for the mapping table.
fn access_mask_for(role: Role, caps: u64) -> AccessMask {
    let has = |c: Caps| caps & c.0 == c.0;
    let mut m = AccessMask::NONE;
    // Everyone appears in the online user list.
    m.grant(Privilege::ShowInList);
    if has(Caps::WHO) {
        m.grant(Privilege::GetClientInfo);
    }
    if has(Caps::CHAT_READ) {
        m.grant(Privilege::ReadChat);
    }
    if has(Caps::CHAT_SEND) {
        m.grant(Privilege::SendChat);
    }
    if has(Caps::CHAT_CREATE_ROOM) {
        m.grant(Privilege::OpenChat);
    }
    if has(Caps::CHAT_MODERATE) {
        m.grant(Privilege::CloseChat);
    }
    if has(Caps::DM_SEND) {
        m.grant(Privilege::SendPrivateMessages);
    }
    if has(Caps::BOARD_READ) {
        m.grant(Privilege::NewsReadArticle);
    }
    if has(Caps::BOARD_POST) {
        m.grant(Privilege::NewsPostArticle);
    }
    if has(Caps::BOARD_MODERATE) {
        for p in [
            Privilege::NewsDeleteArticle,
            Privilege::NewsCreateCategory,
            Privilege::NewsDeleteCategory,
            Privilege::NewsCreateFolder,
            Privilege::NewsDeleteFolder,
        ] {
            m.grant(p);
        }
    }
    if has(Caps::FILE_DOWNLOAD) {
        m.grant(Privilege::DownloadFiles);
    }
    if has(Caps::FILE_UPLOAD) {
        m.grant(Privilege::UploadFiles);
    }
    if has(Caps::FILE_MANAGE) {
        for p in [
            Privilege::DeleteFiles,
            Privilege::RenameFiles,
            Privilege::MoveFiles,
            Privilege::CreateFolders,
            Privilege::DeleteFolders,
            Privilege::RenameFolders,
            Privilege::MoveFolders,
            Privilege::UploadAnywhere,
            Privilege::SetFileComment,
            Privilege::SetFolderComment,
            Privilege::MakeAliases,
        ] {
            m.grant(p);
        }
    }
    if has(Caps::DROPBOX_VIEW) {
        m.grant(Privilege::ViewDropBoxes);
    }
    if has(Caps::USER_KICK) {
        m.grant(Privilege::DisconnectUsers);
    }
    if has(Caps::CANNOT_BE_KICKED) {
        m.grant(Privilege::CannotBeDisconnected);
    }
    if has(Caps::ACCOUNT_ADMIN) {
        for p in [
            Privilege::CreateUsers,
            Privilege::DeleteUsers,
            Privilege::OpenUsers,
            Privilege::ModifyUsers,
        ] {
            m.grant(p);
        }
    }
    if has(Caps::BROADCAST) {
        m.grant(Privilege::Broadcast);
    }
    // Real accounts (not guests) manage their own password and pick names.
    if role >= Role::User {
        m.grant(Privilege::ChangeOwnPassword);
        m.grant(Privilege::AnyName);
    }
    m
}

/// Map an incoming Hotline access bitmap to the nearest RabbitHole role.
///
/// Lossy by design (see the section docs): bits between role tiers are
/// rounded to the tier they signal, and superuser is never reachable.
fn role_for_access(mask: &AccessMask) -> Role {
    if mask.has(Privilege::CreateUsers)
        || mask.has(Privilege::DeleteUsers)
        || mask.has(Privilege::ModifyUsers)
    {
        Role::Admin
    } else if mask.has(Privilege::DisconnectUsers)
        || mask.has(Privilege::NewsDeleteArticle)
        || mask.has(Privilege::CloseChat)
    {
        Role::Moderator
    } else if mask.has(Privilege::NewsPostArticle)
        || mask.has(Privilege::UploadFiles)
        || mask.has(Privilege::DownloadFiles)
        || mask.has(Privilege::SendPrivateMessages)
        || mask.has(Privilege::OpenChat)
    {
        Role::User
    } else {
        Role::Guest
    }
}

/// The effective base capabilities of a stored account row (role default |
/// class mask | grants, minus revokes) — the same layering a live session's
/// `Subject` uses, sourced from the live class cache.
fn account_base_caps(shared: &Shared, account: &Account) -> u64 {
    Subject {
        account_id: account.id,
        role: Role::from_ordinal(account.role),
        class_id: account.class_id,
        class_mask: shared.classes.mask(account.class_id),
        grant_mask: account.grant_mask,
        revoke_mask: account.revoke_mask,
    }
    .base_caps()
}

/// Does the caller hold `needed` on the `admin` resource? (Identical to the
/// checks the native admin family performs.)
fn admin_allowed(shared: &Shared, active: &Active, needed: Caps) -> bool {
    shared.perms.allows(&active.subject, "admin", needed)
}

/// Look up a *visible* account by login. Soft-deleted (disabled) accounts
/// read as absent on this surface.
async fn visible_account(shared: &Shared, login: &str) -> Result<Option<Account>, anyhow::Error> {
    Ok(AccountsRepo(&shared.pool)
        .by_login(login)
        .await?
        .filter(|a| !a.disabled))
}

/// NewUser (350): create an account. The access bitmap picks the nearest
/// role (and its same-named class); a missing bitmap defaults to member.
async fn new_user(shared: &Arc<Shared>, active: &Active, txn: &Transaction) -> Transaction {
    let ty = transaction::NEW_USER;
    let id = txn.header.id;
    if !admin_allowed(shared, active, Caps::ACCOUNT_ADMIN) {
        return err_reply(ty, id, "not permitted");
    }
    let login = field_text_deobf(txn, field::USER_LOGIN).trim().to_string();
    if login.is_empty() {
        return err_reply(ty, id, "no login given");
    }
    let password = field_text_deobf(txn, field::USER_PASSWORD);
    let role = field_bytes(txn, field::USER_ACCESS)
        .and_then(|b| AccessMask::decode(b).ok())
        .map(|m| role_for_access(&m))
        .unwrap_or(Role::User);
    match shared.auth.create_account(&login, &password, role).await {
        Ok(_) => {
            audit(
                shared,
                &active.login,
                "account-create",
                format!("{login} role={role:?} via=hotline"),
            );
            Transaction::reply(ty, id, 0, Vec::new())
        }
        Err(AuthError::LoginTaken) => err_reply(ty, id, "login already exists"),
        Err(e) => err_reply(ty, id, &format!("create failed: {e}")),
    }
}

/// DeleteUser (351): soft-delete an account — it is disabled (login refused,
/// absent from this surface) and its live sessions are kicked. The row (and
/// the login) is retained; hard removal is a documented follow-up.
async fn delete_user(shared: &Arc<Shared>, active: &Active, txn: &Transaction) -> Transaction {
    let ty = transaction::DELETE_USER;
    let id = txn.header.id;
    if !admin_allowed(shared, active, Caps::ACCOUNT_ADMIN) {
        return err_reply(ty, id, "not permitted");
    }
    let login = field_text_deobf(txn, field::USER_LOGIN).trim().to_string();
    let account = match visible_account(shared, &login).await {
        Ok(Some(a)) => a,
        Ok(None) => return err_reply(ty, id, "no such account"),
        Err(e) => return err_reply(ty, id, &format!("{e}")),
    };
    // Never let a Hotline admin soft-delete a superuser (or themselves out
    // from under their own session by role trickery): role ordering applies,
    // same as the kick path.
    if Role::from_ordinal(account.role) >= active.subject.role
        && active.subject.role != Role::Superuser
    {
        return err_reply(ty, id, "cannot delete that account");
    }
    match AccountsRepo(&shared.pool)
        .admin_set(&login, None, None, Some(true))
        .await
    {
        Ok(true) => {}
        Ok(false) => return err_reply(ty, id, "no such account"),
        Err(e) => return err_reply(ty, id, &format!("{e}")),
    }
    // Classic servers drop the deleted account's live sessions.
    for e in shared.presence.snapshot() {
        if e.account_id == account.id {
            shared.bus.publish(ServerEvent::Kick {
                session_id: e.session_id,
                reason: "account deleted".into(),
            });
        }
    }
    audit(
        shared,
        &active.login,
        "account-delete",
        format!("{login} via=hotline (soft delete: disabled)"),
    );
    Transaction::reply(ty, id, 0, Vec::new())
}

/// GetUser (352): an account's name, login, password placeholder, and the
/// projected access bitmap.
///
/// The password is **never** disclosed (only an Argon2id hash is stored):
/// the reply carries an empty USER_PASSWORD placeholder, and SetUser treats
/// an empty password as "unchanged".
async fn get_user(shared: &Arc<Shared>, active: &Active, txn: &Transaction) -> Transaction {
    let ty = transaction::GET_USER;
    let id = txn.header.id;
    if !admin_allowed(shared, active, Caps::ACCOUNT_ADMIN) {
        return err_reply(ty, id, "not permitted");
    }
    let login = field_text_deobf(txn, field::USER_LOGIN).trim().to_string();
    let account = match visible_account(shared, &login).await {
        Ok(Some(a)) => a,
        Ok(None) => return err_reply(ty, id, "no such account"),
        Err(e) => return err_reply(ty, id, &format!("{e}")),
    };
    let mask = access_mask_for(
        Role::from_ordinal(account.role),
        account_base_caps(shared, &account),
    );
    audit(
        shared,
        &active.login,
        "account-get",
        format!("{login} via=hotline"),
    );
    Transaction::reply(
        ty,
        id,
        0,
        vec![
            Field::text(field::USER_NAME, &account.screen_name),
            Field::credential(field::USER_LOGIN, &account.login),
            Field::new(field::USER_PASSWORD, Vec::new()),
            Field::new(field::USER_ACCESS, mask.to_bytes().to_vec()),
        ],
    )
}

/// SetUser (353): update an account. A non-empty password re-hashes it; the
/// access bitmap re-maps role + class **only when the projected role
/// changes** (so a round-tripped GetUser bitmap is a no-op), and never
/// touches a superuser's role. Login rename and screen-name edits are not
/// supported on this surface (documented lossy cases).
async fn set_user(shared: &Arc<Shared>, active: &Active, txn: &Transaction) -> Transaction {
    let ty = transaction::SET_USER;
    let id = txn.header.id;
    if !admin_allowed(shared, active, Caps::ACCOUNT_ADMIN) {
        return err_reply(ty, id, "not permitted");
    }
    let login = field_text_deobf(txn, field::USER_LOGIN).trim().to_string();
    let account = match visible_account(shared, &login).await {
        Ok(Some(a)) => a,
        Ok(None) => return err_reply(ty, id, "no such account"),
        Err(e) => return err_reply(ty, id, &format!("{e}")),
    };

    let mut detail = Vec::new();

    // Password: empty means unchanged (GetUser sends an empty placeholder).
    let password = field_text_deobf(txn, field::USER_PASSWORD);
    if !password.is_empty() {
        let Ok(phc) = hash_password(&password) else {
            return err_reply(ty, id, "password rejected");
        };
        if let Err(e) = AccountsRepo(&shared.pool)
            .update_phc(account.id, &phc)
            .await
        {
            return err_reply(ty, id, &format!("{e}"));
        }
        detail.push("password".to_string());
    }

    // Access bitmap -> nearest role + same-named class, when it changes.
    if let Some(mask) =
        field_bytes(txn, field::USER_ACCESS).and_then(|b| AccessMask::decode(b).ok())
    {
        let current = Role::from_ordinal(account.role);
        let wanted = role_for_access(&mask);
        if wanted != current && current != Role::Superuser {
            let class = shared.classes.id_by_name(wanted.class_name());
            match AccountsRepo(&shared.pool)
                .admin_set(&login, Some(wanted as u8), Some(class), None)
                .await
            {
                Ok(true) => detail.push(format!("role={wanted:?}")),
                Ok(false) => return err_reply(ty, id, "no such account"),
                Err(e) => return err_reply(ty, id, &format!("{e}")),
            }
            // Push the new bitmap (UserAccess, 354) to the account's live
            // Hotline sessions so their menus update immediately.
            if let Ok(Some(updated)) = AccountsRepo(&shared.pool).by_login(&login).await {
                let mask = access_mask_for(wanted, account_base_caps(shared, &updated));
                let push = Transaction::request(
                    transaction::USER_ACCESS,
                    0,
                    vec![Field::new(field::USER_ACCESS, mask.to_bytes().to_vec())],
                )
                .encode();
                for e in shared.presence.snapshot() {
                    if e.account_id == account.id {
                        shared.hotline.deliver(e.session_id as u32, push.clone());
                    }
                }
            }
        }
    }

    audit(
        shared,
        &active.login,
        "account-set",
        format!("{login} [{}] via=hotline", detail.join(", ")),
    );
    Transaction::reply(ty, id, 0, Vec::new())
}

/// DisconnectUser (110): kick (and optionally temp-ban) an online user, via
/// the same [`ServerEvent::Kick`] path and the same capability + role-order
/// checks as the native admin Kick. The ban option (a non-zero OPTIONS
/// field) additionally requires the ban capability and records the target's
/// login — and, for Hotline clients, their remote IP — in the in-memory
/// temporary ban list.
async fn disconnect_user(shared: &Arc<Shared>, active: &Active, txn: &Transaction) -> Transaction {
    let ty = transaction::DISCONNECT_USER;
    let id = txn.header.id;
    if !admin_allowed(shared, active, Caps::USER_KICK) {
        return err_reply(ty, id, "not permitted");
    }
    let target = field_int(txn, field::USER_ID).unwrap_or(0);
    let Some(entry) = shared
        .presence
        .snapshot()
        .into_iter()
        .find(|e| e.session_id as u32 == target)
    else {
        return err_reply(ty, id, "no such user");
    };
    // Respect CANNOT_BE_KICKED and role ordering (no kicking upwards) —
    // identical to the native Kick handler.
    if entry.role >= active.subject.role && active.subject.role != Role::Superuser {
        return err_reply(ty, id, "cannot disconnect that user");
    }
    let ban = field_int(txn, field::OPTIONS).unwrap_or(0) != 0;
    if ban {
        if !admin_allowed(shared, active, Caps::USER_BAN) {
            return err_reply(ty, id, "not permitted to ban");
        }
        // Ban the account login (real accounts) and the client IP (known for
        // Hotline sessions; native sessions ban by login only).
        if entry.account_id > 0 {
            if let Ok(Some(account)) = AccountsRepo(&shared.pool).by_id(entry.account_id).await {
                shared
                    .hotline
                    .ban(format!("login:{}", account.login.to_lowercase()));
            }
        }
        if let Some(ip) = shared.hotline.ip_of(target) {
            shared.hotline.ban(format!("ip:{ip}"));
        }
    }
    let reason = if ban { "banned" } else { "kicked" };
    shared.bus.publish(ServerEvent::Kick {
        session_id: entry.session_id,
        reason: reason.into(),
    });
    audit(
        shared,
        &active.login,
        "kick",
        format!(
            "session {} via=hotline{}",
            entry.session_id,
            if ban { " (banned)" } else { "" }
        ),
    );
    Transaction::reply(ty, id, 0, Vec::new())
}

/// UserBroadcast (355): admin message to every connected user, published as
/// [`ServerEvent::Notice`] — the same shared path the native Broadcast admin
/// op uses — so native, telnet, and Hotline clients all see it.
fn user_broadcast(shared: &Arc<Shared>, active: &Active, txn: &Transaction) -> Transaction {
    let ty = transaction::USER_BROADCAST;
    let id = txn.header.id;
    if !admin_allowed(shared, active, Caps::BROADCAST) {
        return err_reply(ty, id, "not permitted");
    }
    let text = field_text(txn, field::DATA).unwrap_or_default();
    if text.trim().is_empty() {
        return err_reply(ty, id, "empty broadcast");
    }
    shared.bus.publish(ServerEvent::Notice {
        text: text.clone(),
        from: active.screen_name.clone(),
    });
    audit(shared, &active.login, "broadcast", text);
    Transaction::reply(ty, id, 0, Vec::new())
}

/// Project a server event into a Hotline push for this client, if relevant.
///
/// A client seeing its own roster/chat delta is harmless in the classic
/// protocol, so no self-filtering is applied here.
fn project_event(shared: &Shared, event: &ServerEvent) -> Option<Vec<u8>> {
    match event {
        ServerEvent::Chat { room, from, text } => {
            if !room.eq_ignore_ascii_case(LOBBY) {
                return None;
            }
            // Classic chat lines are a single formatted string: "\r nick:  msg".
            let line = format!("\r{from}:  {text}");
            Some(
                Transaction::request(
                    transaction::CHAT_MSG,
                    0,
                    vec![Field::new(field::CHAT_TEXT, line.into_bytes())],
                )
                .encode(),
            )
        }
        ServerEvent::SessionOpened {
            session_id,
            screen_name,
        }
        | ServerEvent::SessionChanged {
            session_id,
            screen_name,
        } => Some(notify_change_user(
            shared,
            *session_id as u32,
            screen_name,
            0,
        )),
        ServerEvent::PresenceChanged {
            session_id,
            screen_name,
            state,
            ..
        } => {
            if *state == 3 {
                // Going invisible reads as a leave to everyone else.
                return Some(notify_delete_user(*session_id as u32));
            }
            Some(notify_change_user(
                shared,
                *session_id as u32,
                screen_name,
                flags_for_state(*state),
            ))
        }
        ServerEvent::SessionClosed { session_id, .. } => {
            Some(notify_delete_user(*session_id as u32))
        }
        // Admin broadcast (native Broadcast or Hotline UserBroadcast): the
        // classic server-message push without a USER_ID field, which clients
        // render as a server broadcast rather than a private message.
        ServerEvent::Notice { text, .. } => Some(
            Transaction::request(
                transaction::SERVER_MSG,
                0,
                vec![Field::new(field::DATA, text.clone().into_bytes())],
            )
            .encode(),
        ),
        _ => None,
    }
}

/// A NotifyChangeUser (301) push for `uid`, looking up the live icon.
fn notify_change_user(shared: &Shared, uid: u32, name: &str, flags: u32) -> Vec<u8> {
    let icon = shared.hotline.icon_for(uid);
    Transaction::request(
        transaction::NOTIFY_CHANGE_USER,
        0,
        vec![
            Field::int(field::USER_ID, uid),
            Field::int(field::USER_ICON_ID, u32::from(icon)),
            Field::int(field::USER_FLAGS, flags),
            Field::text(field::USER_NAME, name),
        ],
    )
    .encode()
}

/// A NotifyDeleteUser (302) push for `uid`.
fn notify_delete_user(uid: u32) -> Vec<u8> {
    Transaction::request(
        transaction::NOTIFY_DELETE_USER,
        0,
        vec![Field::int(field::USER_ID, uid)],
    )
    .encode()
}

/// A bare success reply (no fields) for `type_`/`id`.
fn empty_reply(type_: u16, id: u32) -> Vec<u8> {
    Transaction::reply(type_, id, 0, Vec::new()).encode()
}

/// The classic user flags implied by a presence state (only "away" is mapped).
fn flags_for_state(state: u8) -> u32 {
    // Presence: 0 online, 1 away, 2 idle, 3 invisible. Hotline flag bit 1
    // (0x02) is the away/refuse marker clients render as a dimmed user.
    if state == 1 || state == 2 {
        0x02
    } else {
        0
    }
}

/// Pack a user into the classic "user name with info" record.
fn pack_user(uid: u32, icon: u16, flags: u32, name: &str) -> Vec<u8> {
    let name_bytes = name.as_bytes();
    let name_len = name_bytes.len().min(u16::MAX as usize);
    let mut out = Vec::with_capacity(8 + name_len);
    out.extend_from_slice(&(uid as u16).to_be_bytes());
    out.extend_from_slice(&icon.to_be_bytes());
    out.extend_from_slice(&(flags as u16).to_be_bytes());
    out.extend_from_slice(&(name_len as u16).to_be_bytes());
    out.extend_from_slice(&name_bytes[..name_len]);
    out
}

/// The raw bytes of the first field with `id`, if present.
fn field_bytes(txn: &Transaction, id: u16) -> Option<&[u8]> {
    txn.fields
        .iter()
        .find(|f| f.id == id)
        .map(|f| f.data.as_slice())
}

/// A text field decoded lossily as UTF-8.
fn field_text(txn: &Transaction, id: u16) -> Option<String> {
    field_bytes(txn, id).map(|b| String::from_utf8_lossy(b).into_owned())
}

/// A size-dependent integer field.
fn field_int(txn: &Transaction, id: u16) -> Option<u32> {
    field_bytes(txn, id).and_then(|b| read_int(b).ok())
}

/// A login credential field, de-obfuscated (each byte is bitwise-complemented
/// on the wire) and decoded lossily as UTF-8.
fn field_text_deobf(txn: &Transaction, id: u16) -> String {
    match field_bytes(txn, id) {
        Some(b) => {
            let inv: Vec<u8> = b.iter().map(|byte| !byte).collect();
            String::from_utf8_lossy(&inv).into_owned()
        }
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The projected mask for a role's *default* capabilities.
    fn mask_for(role: Role) -> AccessMask {
        access_mask_for(role, role.default_caps().0)
    }

    #[test]
    fn role_projection_roundtrips_for_assignable_roles() {
        // Guest/User/Moderator/Admin project to a bitmap that maps back to
        // the same role — the invariant GetUser -> SetUser depends on.
        for role in [Role::Guest, Role::User, Role::Moderator, Role::Admin] {
            assert_eq!(role_for_access(&mask_for(role)), role, "{role:?}");
        }
        // Superuser is the documented lossy case: its bitmap is admin-shaped,
        // and no bitmap can assign superuser.
        assert_eq!(role_for_access(&mask_for(Role::Superuser)), Role::Admin);
    }

    #[test]
    fn admin_mask_has_user_admin_kick_and_broadcast_bits() {
        let m = mask_for(Role::Admin);
        for p in [
            Privilege::CreateUsers,
            Privilege::DeleteUsers,
            Privilege::OpenUsers,
            Privilege::ModifyUsers,
            Privilege::DisconnectUsers,
            Privilege::Broadcast,
            Privilege::DeleteFiles,
            Privilege::ViewDropBoxes,
        ] {
            assert!(m.has(p), "admin should hold {p:?}");
        }
    }

    #[test]
    fn member_mask_has_participation_bits_but_no_admin_bits() {
        let m = mask_for(Role::User);
        for p in [
            Privilege::ReadChat,
            Privilege::SendChat,
            Privilege::OpenChat,
            Privilege::NewsReadArticle,
            Privilege::NewsPostArticle,
            Privilege::DownloadFiles,
            Privilege::UploadFiles,
            Privilege::SendPrivateMessages,
            Privilege::ChangeOwnPassword,
            Privilege::AnyName,
            Privilege::ShowInList,
        ] {
            assert!(m.has(p), "member should hold {p:?}");
        }
        for p in [
            Privilege::CreateUsers,
            Privilege::DeleteUsers,
            Privilege::ModifyUsers,
            Privilege::DisconnectUsers,
            Privilege::Broadcast,
            Privilege::DeleteFiles,
        ] {
            assert!(!m.has(p), "member should not hold {p:?}");
        }
    }

    #[test]
    fn guest_mask_is_read_mostly_and_maps_back_to_guest() {
        let m = mask_for(Role::Guest);
        assert!(m.has(Privilege::ReadChat));
        assert!(m.has(Privilege::SendChat)); // the Hotline tradition
        assert!(m.has(Privilege::NewsReadArticle));
        assert!(m.has(Privilege::ShowInList));
        assert!(!m.has(Privilege::DownloadFiles));
        assert!(!m.has(Privilege::NewsPostArticle));
        assert!(!m.has(Privilege::ChangeOwnPassword));
        assert_eq!(role_for_access(&m), Role::Guest);
    }

    #[test]
    fn moderator_bits_round_to_moderator() {
        // Any single moderation signal rounds up to moderator...
        let mut m = mask_for(Role::User);
        m.grant(Privilege::DisconnectUsers);
        assert_eq!(role_for_access(&m), Role::Moderator);
        // ...but a user-admin bit outranks it.
        m.grant(Privilege::CreateUsers);
        assert_eq!(role_for_access(&m), Role::Admin);
    }

    #[test]
    fn kick_and_cannot_be_kicked_bits_project() {
        let moderator = mask_for(Role::Moderator);
        assert!(moderator.has(Privilege::DisconnectUsers));
        // CANNOT_BE_KICKED is not in any default role mask; a grant projects.
        let caps = Role::User.default_caps().0 | Caps::CANNOT_BE_KICKED.0;
        assert!(access_mask_for(Role::User, caps).has(Privilege::CannotBeDisconnected));
    }
}
