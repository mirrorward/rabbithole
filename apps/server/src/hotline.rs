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
//! # Scope (this slice)
//!
//! A deliberately *core* subset: **login, presence, public chat, and IM**.
//! Files (HTXF transfers), flat news, private chat rooms, and admin/account
//! transactions are explicitly deferred to later slices — those transaction
//! types are tolerated (an empty success reply) rather than served, so a client
//! that probes them keeps working. The listener is opt-in via config
//! (`hotline_enabled`) and off by default.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use parking_lot::Mutex;
use rabbithole_legacy_hotline::constants::{field, transaction};
use rabbithole_legacy_hotline::{read_int, Field, Handshake, HandshakeReply, Reassembler};
use rabbithole_legacy_hotline::{Transaction, TransactionHeader};
use rabbithole_server_core::chat::LOBBY;
use rabbithole_server_core::{Caps, PresenceEntry, Role, ServerEvent, Subject};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::TcpListener;
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

/// The per-server Hotline hub: the set of currently-connected Hotline clients,
/// keyed by their wire user id, used to route private instant messages and to
/// answer icon lookups for the user list. A field of [`Shared`] alongside
/// `radio`.
#[derive(Default)]
pub struct Hub {
    inner: Mutex<HashMap<u32, ClientHandle>>,
}

/// A connected Hotline client's routing handle.
struct ClientHandle {
    /// Pre-encoded transactions to write to this client (IM delivery).
    tx: mpsc::UnboundedSender<Vec<u8>>,
    /// The client's current icon id (for the user list).
    icon: u16,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    fn register(&self, id: u32, tx: mpsc::UnboundedSender<Vec<u8>>, icon: u16) {
        self.inner.lock().insert(id, ClientHandle { tx, icon });
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
}

/// Bind + serve the Hotline surface. Returns the bound address (useful when the
/// config asked for port 0) and the accept-loop task handle. Mirrors the
/// telnet/finger/nntp/radio spawn helpers.
pub async fn spawn_hotline(
    shared: Arc<Shared>,
    addr: SocketAddr,
) -> Result<(SocketAddr, JoinHandle<()>)> {
    let listener = TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    let handle = tokio::spawn(async move {
        loop {
            let Ok((sock, _peer)) = listener.accept().await else {
                break;
            };
            let shared = shared.clone();
            tokio::spawn(async move {
                if let Err(e) = serve(sock, shared).await {
                    tracing::debug!("hotline session error: {e}");
                }
            });
        }
    });
    Ok((local, handle))
}

/// The mutable per-connection state once a client is logged in.
struct Active {
    /// Shared server session id (presence/chat membership key).
    session_id: u64,
    /// Hotline wire user id (the low 32 bits of `session_id`).
    user_id: u32,
    /// Permission subject captured at login.
    subject: Subject,
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
            return Ok(());
        }
    };

    let screen_name = want_name.unwrap_or_else(|| authed.persona.screen_name.clone());
    let session_id = shared.next_session_id();
    let user_id = session_id as u32;

    // 4. Login reply: success carries a server version + name.
    let server_name = shared.config.read().name;
    wr.write_all(
        &Transaction::reply(
            transaction::LOGIN,
            login_txn.header.id,
            0,
            vec![
                Field::int(field::VERSION, SERVER_VERSION),
                Field::text(field::SERVER_NAME, &server_name),
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
    shared.hotline.register(user_id, out_tx, icon);
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

    // 7. Leave the shared world (publishes the roster delete).
    shared.hotline.unregister(user_id);
    shared.chat.session_closed(session_id);
    shared.presence.leave(session_id);
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

        // Deferred transaction families (files/news/private-chat/admin): reply
        // with a bare success so a probing client keeps working.
        _ => {
            wr.write_all(&empty_reply(txn.header.type_, txn.header.id))
                .await?;
        }
    }
    Ok(())
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
