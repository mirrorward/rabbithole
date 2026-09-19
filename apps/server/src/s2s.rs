//! Pulls between burrows (FILE 33..38): a person sends files or folders from
//! one burrow they are on (the **source**) to another (the **destination**),
//! and the destination fetches them itself. The design and Kevin's answers
//! are in `docs/design/server-to-server-transfers.md`.
//!
//! - **At the source**, [`PullGrantRequest`](pf::PullGrantRequest) signs a
//!   [`PullGrant`](fp::PullGrant) for what the person may download there,
//!   naming the destination's server key as its only fetcher.
//! - **At the destination**, [`RemotePull`](pf::RemotePull) checks the grant
//!   against the source's key in this burrow's own peer registry, the person's
//!   right to upload (and to make folders, when the grant carries any), the
//!   largest file, their space and the deny list, then fetches each file over
//!   a bulk stream of the live federation session with the source
//!   ([`fetch_item`]) and files it under the person.
//! - **Back at the source**, [`serve_pull_stream`] answers such a stream only
//!   for the peer the grant names, authenticated by that session's handshake.
//!
//! Only approved federation peers take part, on both sides: the destination
//! dials nothing new and opens no listener for this.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rabbithole_blobs::BlobId;
use rabbithole_federation::pull::{self as fp, stream_status, PullStreamRequest, SignedPullGrant};
use rabbithole_identity::IdentityKey;
use rabbithole_net::quic::QuicTransport;
use rabbithole_net::tls::{CertFingerprint, ServerAuth};
use rabbithole_net::{
    read_framed, write_framed, BulkRecv, BulkSend, BulkStreams, Connection, Transport,
};
use rabbithole_proto::filelib::{pull_reason, pull_state};
use rabbithole_proto::hello::{key_auth_message, Hello, HelloAck, KeyProof, PullSessionOpen};
use rabbithole_proto::{filelib as pf, ErrorCode, Frame};
use rabbithole_proto::{CapabilitySet, FrameKind, RequestId};
use rabbithole_server_core::files::{KIND_FILE, KIND_FOLDER};
use rabbithole_server_core::ratelimit::{class as rl, Scope};
use rabbithole_server_core::{Caps, FileError, Role, ServerEvent, Subject};
use rabbithole_store_server::repo::AccountsRepo;
use rabbithole_store_server::repo6::FileNodeRow;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::session::SessionCtx;
use crate::Shared;

/// How long a grant stands for starting a pull. It is bound to one
/// destination and spent there on first use.
pub const GRANT_TTL_SECS: i64 = 3600;
/// How long after a grant lapses the source still serves a pull that started
/// in time: a big folder can take longer than the hour.
const SERVE_GRACE_SECS: i64 = 6 * 3600;
/// Most nodes one grant request may name (a folder counts once).
const MAX_REQUEST_NODES: usize = 100;
/// A stream that says nothing for this long is given up on.
const STREAM_IDLE: Duration = Duration::from_secs(30);
/// The slowest average a source may send at before a file is given up on.
const MIN_RATE: u64 = 16 * 1024;
/// Time between progress pushes.
const PROGRESS_EVERY: Duration = Duration::from_millis(500);
/// Bytes per read or write of a pulled file.
const CHUNK: usize = 256 * 1024;
/// Pulls this burrow runs at once, for everyone.
const MAX_RUNNING: usize = 16;
/// The smallest file worth fetching from the swarm: two of its units, so
/// two peers can share the work.
const SWARM_MIN_BYTES: u64 = 2 * rabbithole_swarm::UNIT_SIZE;
/// Longest MIME type a grant carries; a longer one travels as unknown.
const MAX_MIME: usize = 255;
/// Most attempts at a free name before a clash is given up on.
const MAX_NUMBERING: u32 = 100;

/// The federation sessions a pull can ride, by peer key: each live session
/// with the token it was offered under, newest last.
type Links = HashMap<[u8; 32], Vec<(u64, Arc<dyn BulkStreams>)>>;

/// What this burrow knows about pulls: the federation sessions a pull can
/// ride, and the pulls running here. Spent grants are in the store, so a
/// restart does not make them good again.
#[derive(Default)]
pub struct S2sState {
    links: Mutex<Links>,
    next_link: AtomicU64,
    pulls: Mutex<HashMap<u64, PullEntry>>,
    next_pull: AtomicU64,
    /// Pull sessions open here for burrows fetching directly, by the nonce
    /// of the grant each was opened with (one at a time per grant), with the
    /// account that grant names.
    sessions: Mutex<HashMap<[u8; 16], Option<i64>>>,
}

/// What a burrow derives, from its signing seed, the key it hides the
/// asking person's account in its grants' nonces with: nothing to store, and
/// the same after a restart.
fn nonce_key(shared: &Shared) -> [u8; 32] {
    blake3::derive_key(
        "rabbithole 2026-09-19 s2s grant nonce account v1",
        &shared.server_signing_seed,
    )
}

/// A grant's nonce that names the person who asked for it, readable only by
/// the burrow holding `key`: 7 random bytes, the account id (48 bits) under a
/// pad drawn from them, and a 3-byte tag that tells it from a nonce that
/// names no one. An id that does not fit gets a plain random nonce.
fn grant_nonce(key: &[u8; 32], account_id: i64) -> [u8; 16] {
    grant_nonce_from(key, account_id, nonce())
}

/// [`grant_nonce`], its randomness given.
fn grant_nonce_from(key: &[u8; 32], account_id: i64, random: [u8; 16]) -> [u8; 16] {
    let mut n = random;
    if !(0..1i64 << 48).contains(&account_id) {
        return n;
    }
    let id = (account_id as u64).to_le_bytes();
    let pad = nonce_pad(key, &n[..7]);
    for i in 0..6 {
        n[7 + i] = id[i] ^ pad[i];
    }
    let tag = nonce_tag(key, &n[..7], &id[..6]);
    n[13..16].copy_from_slice(&tag);
    n
}

/// The account a grant's nonce names, if this burrow made it so.
fn nonce_account(key: &[u8; 32], nonce: &[u8; 16]) -> Option<i64> {
    let pad = nonce_pad(key, &nonce[..7]);
    let mut id = [0u8; 8];
    for i in 0..6 {
        id[i] = nonce[7 + i] ^ pad[i];
    }
    (nonce_tag(key, &nonce[..7], &id[..6]) == nonce[13..16]).then(|| u64::from_le_bytes(id) as i64)
}

fn nonce_pad(key: &[u8; 32], random: &[u8]) -> [u8; 6] {
    let mut input = b"pad".to_vec();
    input.extend_from_slice(random);
    let hash = blake3::keyed_hash(key, &input);
    hash.as_bytes()[..6].try_into().expect("six bytes")
}

fn nonce_tag(key: &[u8; 32], random: &[u8], id: &[u8]) -> [u8; 3] {
    let mut input = b"tag".to_vec();
    input.extend_from_slice(random);
    input.extend_from_slice(id);
    let hash = blake3::keyed_hash(key, &input);
    hash.as_bytes()[..3].try_into().expect("three bytes")
}

struct PullEntry {
    account_id: i64,
    cancel: Arc<AtomicBool>,
}

impl S2sState {
    /// A federation session with `peer` is up and can carry bulk streams.
    /// Returns a token for [`S2sState::link_down`].
    pub fn link_up(&self, peer: [u8; 32], bulk: Arc<dyn BulkStreams>) -> u64 {
        let gen = self.next_link.fetch_add(1, Ordering::Relaxed) + 1;
        self.links.lock().entry(peer).or_default().push((gen, bulk));
        gen
    }

    /// That session ended. Any other live session with the peer is kept.
    pub fn link_down(&self, peer: [u8; 32], gen: u64) {
        let mut links = self.links.lock();
        if let Some(list) = links.get_mut(&peer) {
            list.retain(|(g, _)| *g != gen);
            if list.is_empty() {
                links.remove(&peer);
            }
        }
    }

    /// The newest live session with `peer`, if there is one.
    pub fn link(&self, peer: &[u8; 32]) -> Option<Arc<dyn BulkStreams>> {
        self.links
            .lock()
            .get(peer)
            .and_then(|list| list.last())
            .map(|(_, bulk)| bulk.clone())
    }

    /// Take a place for a new pull, within the burrow's and the account's
    /// limits, in one step so two requests cannot both squeeze in.
    fn reserve(&self, account_id: i64, per_account: u32) -> Option<(u64, Arc<AtomicBool>)> {
        let mut pulls = self.pulls.lock();
        let mine = pulls
            .values()
            .filter(|p| p.account_id == account_id)
            .count();
        if pulls.len() >= MAX_RUNNING || (per_account > 0 && mine >= per_account as usize) {
            return None;
        }
        let id = self.next_pull.fetch_add(1, Ordering::Relaxed) + 1;
        let cancel = Arc::new(AtomicBool::new(false));
        pulls.insert(
            id,
            PullEntry {
                account_id,
                cancel: cancel.clone(),
            },
        );
        Some((id, cancel))
    }

    fn release(&self, pull_id: u64) {
        self.pulls.lock().remove(&pull_id);
    }

    /// Pulls running here, for tests and the console.
    pub fn running(&self) -> usize {
        self.pulls.lock().len()
    }
}

/// Remove pull staging files a stopped burrow left behind: a pull does not
/// survive a restart.
pub fn sweep_staging(data_dir: &Path) {
    let dir = data_dir.join("transfers");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // A file's staging, and a swarm fetch's partial and its sidecars.
        if name.starts_with("pull-") && name.contains(".part") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn resource(area: &str, path: Option<&str>) -> String {
    match path {
        Some(p) if !p.is_empty() => format!("files/{area}/{p}"),
        _ => format!("files/{area}"),
    }
}

fn nonce() -> [u8; 16] {
    use rand::RngCore;
    let mut n = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut n);
    n
}

/// `name` numbered the way downloads number a clash: `name (2).ext`,
/// `name (3).ext`, … (the extension is what follows the last dot, if the dot
/// is not the first character). The stem is shortened when the result would
/// pass the library's 128-byte name limit.
pub fn numbered(name: &str, n: u32) -> String {
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    let tag = format!(" ({n})");
    let room = fp::MAX_SEGMENT.saturating_sub(tag.len() + ext.len());
    let mut cut = stem.len().min(room);
    while !stem.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{tag}{ext}", &stem[..cut])
}

/// The first of `name`, `name (2)`, `name (3)`… that `taken` does not hold.
pub fn unique_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(name) {
        return name.to_string();
    }
    (2..)
        .map(|n| numbered(name, n))
        .find(|candidate| !taken(candidate))
        .expect("an unbounded range finds a free name")
}

fn audit(shared: &Arc<Shared>, actor: &str, action: &str, detail: String) {
    crate::handlers15::audit(shared, actor, action, detail);
}

/// Session requests for pulls, at either end.
pub async fn handle(
    conn: &mut Box<dyn Connection>,
    frame: &Frame,
    shared: &Arc<Shared>,
    ctx: &mut SessionCtx,
) -> anyhow::Result<bool> {
    macro_rules! fail {
        ($code:expr) => {{
            conn.send(Frame::error_reply(frame, $code)).await?;
            return Ok(true);
        }};
    }

    if let Some(Ok(req)) = frame.decode::<pf::PullGrantAsk>() {
        let plain = pf::PullGrantRequest::new(req.fetcher_key, req.nodes);
        match grant(shared, ctx, &plain, &req.reach_host).await {
            Ok(issued) => conn.send(Frame::reply_to(frame, &issued)?).await?,
            Err(code) => fail!(code),
        }
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pf::PullGrantRequest>() {
        match grant(shared, ctx, &req, "").await {
            Ok(issued) => conn.send(Frame::reply_to(frame, &issued)?).await?,
            Err(code) => fail!(code),
        }
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pf::RemotePull>() {
        match accept(shared, ctx, req).await {
            Ok(accepted) => conn.send(Frame::reply_to(frame, &accepted)?).await?,
            Err(code) => fail!(code),
        }
        return Ok(true);
    }

    if let Some(Ok(req)) = frame.decode::<pf::RemotePullCancel>() {
        let cancel = shared
            .s2s
            .pulls
            .lock()
            .get(&req.pull_id)
            .filter(|p| p.account_id == ctx.account_id)
            .map(|p| p.cancel.clone());
        match cancel {
            Some(flag) => {
                flag.store(true, Ordering::Relaxed);
                conn.send(Frame::ack(frame)).await?;
            }
            None => fail!(ErrorCode::NotFound),
        }
        return Ok(true);
    }

    Ok(false)
}

/// What becomes of one file in a grant request.
enum Take {
    /// It goes.
    Item(fp::PullItem),
    /// The person may not send it, or its name will not travel: counted.
    Skip,
    /// Quarantined: neither sent nor counted, so it reads as absent.
    Hidden,
}

/// Whether the person may send `file`, as a grant item.
async fn take(
    shared: &Arc<Shared>,
    ctx: &SessionCtx,
    file: &FileNodeRow,
    rel_path: String,
) -> Take {
    let Some(blob_id) = file.blob_id.filter(|_| file.kind == KIND_FILE) else {
        return Take::Skip;
    };
    // Quarantined content does not leave the burrow, whoever asks, and its
    // absence looks like any other absence.
    if shared.moderation.file_quarantined(Some(&blob_id)) {
        return Take::Hidden;
    }
    if !fp::rel_path_is_acceptable(&rel_path) {
        return Take::Skip;
    }
    let res = resource(&file.area, Some(&file.path));
    if !ctx.allows(shared, &res, Caps::FILE_DOWNLOAD) {
        return Take::Skip;
    }
    if shared.files.in_dropbox(file).await.unwrap_or(true)
        && !ctx.allows(shared, &res, Caps::DROPBOX_VIEW)
        && !ctx.allows(shared, &resource(&file.area, None), Caps::FILE_MANAGE)
    {
        return Take::Skip;
    }
    let mime = if file.mime.len() <= MAX_MIME {
        file.mime.clone()
    } else {
        "application/octet-stream".to_string()
    };
    Take::Item(fp::PullItem {
        node_id: file.id,
        rel_path,
        root: blob_id,
        size: file.size.max(0) as u64,
        mime,
    })
}

/// At the source: sign a grant for what the person may send.
async fn grant(
    shared: &Arc<Shared>,
    ctx: &SessionCtx,
    req: &pf::PullGrantRequest,
    reach_host: &str,
) -> Result<pf::PullGrantIssued, ErrorCode> {
    if !shared.config.read().s2s_grants_enabled {
        return Err(ErrorCode::Unsupported);
    }
    if ctx.is_guest {
        return Err(ErrorCode::Forbidden);
    }
    if req.nodes.is_empty() || req.nodes.len() > MAX_REQUEST_NODES {
        return Err(ErrorCode::BadRequest);
    }
    // A peer, or any burrow when the operator allows it: the fetcher proves
    // its key when it comes to fetch either way.
    if req.fetcher_key == shared.server_key
        || !(shared.peers.is_approved(&req.fetcher_key) || shared.config.read().s2s_grants_to_any)
    {
        return Err(ErrorCode::Unavailable);
    }
    if !shared.rate_allow(Scope::Account(ctx.account_id), rl::TRANSFER) {
        return Err(ErrorCode::RateLimited);
    }
    let mut items = Vec::new();
    let mut skipped = 0u32;
    // Each requested thing lands under its own top-level name; two of the
    // same name are told apart here, before the destination has to guess.
    let mut tops: HashSet<String> = HashSet::new();
    let mut add = |taken: Take, items: &mut Vec<fp::PullItem>| -> Result<(), ErrorCode> {
        match taken {
            Take::Item(item) => items.push(item),
            Take::Skip => skipped += 1,
            Take::Hidden => {}
        }
        if items.len() > fp::MAX_PULL_ITEMS {
            return Err(ErrorCode::BadRequest);
        }
        Ok(())
    };
    for id in &req.nodes {
        let node = match shared.files.node(*id).await {
            Ok(Some(node)) => node,
            Ok(None) => return Err(ErrorCode::NotFound),
            Err(_) => return Err(ErrorCode::Internal),
        };
        let top = unique_name(&node.name, |n| tops.contains(n));
        tops.insert(top.clone());
        if node.kind == KIND_FOLDER {
            let res = resource(&node.area, Some(&node.path));
            if !ctx.allows(shared, &res, Caps::FILE_LIST) {
                return Err(ErrorCode::Forbidden);
            }
            // A drop box's contents are hidden from those who may not view
            // it, and a pull must not be a way around that.
            let hidden = node.is_dropbox || shared.files.in_dropbox(&node).await.unwrap_or(true);
            if hidden
                && !ctx.allows(shared, &res, Caps::DROPBOX_VIEW)
                && !ctx.allows(shared, &resource(&node.area, None), Caps::FILE_MANAGE)
            {
                return Err(ErrorCode::Forbidden);
            }
            let entries = shared
                .files
                .manifest(&node.area, Some(&node.path))
                .await
                .map_err(|_| ErrorCode::Internal)?;
            for (file, rel) in entries {
                let taken = take(shared, ctx, &file, format!("{top}/{rel}")).await;
                add(taken, &mut items)?;
            }
        } else {
            let file = match shared.files.resolve(node.id).await {
                Ok(file) => file,
                Err(_) => return Err(ErrorCode::NotFound),
            };
            let taken = take(shared, ctx, &file, top).await;
            if req.nodes.len() == 1 && matches!(taken, Take::Hidden) {
                return Err(ErrorCode::NotFound);
            }
            add(taken, &mut items)?;
        }
    }
    if items.is_empty() {
        return Err(if skipped > 0 {
            ErrorCode::Forbidden
        } else {
            ErrorCode::NotFound
        });
    }
    // A peer fetches over the federation session and gets the first
    // format, which a peer on an older release reads too. Anyone else is
    // told where to connect, and which certificate to expect there.
    let (version, tls_fingerprint, endpoints) = if shared.peers.is_approved(&req.fetcher_key) {
        (fp::PULL_GRANT_V1, [0; 32], Vec::new())
    } else {
        let certificate = CertFingerprint::from_hex(&shared.fingerprint_hex)
            .map(|fp| fp.0)
            .ok_or(ErrorCode::Internal)?;
        (
            fp::PULL_GRANT_VERSION,
            certificate,
            reachable_at(shared, reach_host),
        )
    };
    let now = now_unix();
    // The nonce names who asked: the grant stops when their account does,
    // and offers this burrow's swarm only while they may use it.
    let grant_nonce = grant_nonce(&nonce_key(shared), ctx.account_id);
    let unsigned = fp::PullGrant {
        version,
        source_key: shared.server_key,
        fetcher_key: req.fetcher_key,
        issued_unix: now,
        expires_unix: now + GRANT_TTL_SECS,
        nonce: grant_nonce,
        items,
        tls_fingerprint,
        endpoints,
    };
    let files = unsigned.items.len() as u32;
    let bytes = unsigned.total_bytes();
    let expires = unsigned.expires_unix;
    let signed = unsigned
        .sign(&IdentityKey::from_seed(&shared.server_signing_seed))
        .map_err(|_| ErrorCode::Internal)?;
    let encoded = signed.to_bytes();
    // It rides every stream request back to this burrow; one that cannot is
    // refused now rather than failing every fetch later.
    if encoded.len() > fp::MAX_GRANT_BYTES {
        return Err(ErrorCode::BadRequest);
    }
    audit(
        shared,
        &ctx.login,
        "pull-grant",
        format!(
            "files={files} bytes={bytes} to={}",
            hex::encode(req.fetcher_key)
        ),
    );
    Ok(pf::PullGrantIssued::new(
        encoded, files, bytes, skipped, expires,
    ))
}

/// Where a burrow that is not a federation peer can reach this one's QUIC
/// port: the host the operator advertises, then the one the person's app
/// used. Each is a claim this burrow signs; the destination still refuses
/// private addresses unless its operator allows them.
fn reachable_at(shared: &Shared, reach_host: &str) -> Vec<String> {
    let port = shared.quic_bound.port();
    let advertised = shared.config.read().advertise_host.clone();
    let mut out: Vec<String> = Vec::new();
    for host in [advertised.trim(), reach_host.trim()] {
        if !host.is_empty() && fp::host_is_acceptable(host) {
            let e = fp::endpoint(host, port);
            if !out.contains(&e) && out.len() < fp::MAX_ENDPOINTS {
                out.push(e);
            }
        }
    }
    out
}

/// The peer's federation name, or its key's fingerprint when it has none.
fn peer_name(shared: &Shared, key: &[u8; 32]) -> String {
    shared
        .peers
        .get(key)
        .and_then(|p| p.origin.or_else(|| (!p.name.is_empty()).then_some(p.name)))
        .unwrap_or_else(|| rabbithole_identity::PublicKey(*key).fingerprint())
}

/// Whether `folder` in `area` is a drop box or lies anywhere inside one.
async fn inside_dropbox(shared: &Shared, area: &str, folder: Option<&str>) -> bool {
    let Some(path) = folder else { return false };
    let mut at = path.to_string();
    loop {
        match shared.files.node_by_path(area, &at).await {
            Ok(Some(node)) if node.is_dropbox => return true,
            Ok(Some(_)) => {}
            _ => return true,
        }
        match at.rsplit_once('/') {
            Some((parent, _)) => at = parent.to_string(),
            None => return false,
        }
    }
}

/// At the destination: check everything, then start fetching.
async fn accept(
    shared: &Arc<Shared>,
    ctx: &SessionCtx,
    req: pf::RemotePull,
) -> Result<pf::RemotePullAccepted, ErrorCode> {
    let (enabled, max_bytes, per_account) = {
        let config = shared.config.read();
        (
            config.s2s_pull_enabled,
            config.s2s_max_bytes,
            config.s2s_max_concurrent,
        )
    };
    if !enabled {
        return Err(ErrorCode::Unsupported);
    }
    if ctx.is_guest {
        return Err(ErrorCode::Forbidden);
    }
    if req.grant.len() > fp::MAX_GRANT_BYTES {
        return Err(ErrorCode::BadRequest);
    }
    let signed = SignedPullGrant::from_bytes(&req.grant).map_err(|_| ErrorCode::BadRequest)?;
    let source = signed.grant.source_key;
    // An approved peer with a session up now is fetched from over that
    // session. Anyone else only if the operator takes sends from any burrow:
    // then this burrow connects to it, at an address the grant carries.
    let peer_link = shared
        .peers
        .is_approved(&source)
        .then(|| shared.s2s.link(&source))
        .flatten();
    if peer_link.is_none() && !shared.config.read().s2s_pull_from_any {
        return Err(ErrorCode::Unavailable);
    }
    let now = now_unix();
    match signed.check(&source, &shared.server_key, now) {
        Ok(()) => {}
        Err(fp::PullGrantError::Expired) => return Err(ErrorCode::SessionExpired),
        Err(_) => return Err(ErrorCode::BadRequest),
    }
    let folder = req.folder.clone().filter(|f| !f.is_empty());
    let here = resource(&req.area, folder.as_deref());
    if !ctx.allows(shared, &here, Caps::FILE_UPLOAD) {
        return Err(ErrorCode::Forbidden);
    }
    match &folder {
        Some(path) => match shared.files.node_by_path(&req.area, path).await {
            Ok(Some(node)) if node.kind == KIND_FOLDER => {}
            _ => return Err(ErrorCode::NotFound),
        },
        None => {
            if shared.files.list(&req.area, None).await.is_err() {
                return Err(ErrorCode::NotFound);
            }
        }
    }
    // A folder is recreated, and making folders is a file manager's right.
    // Never inside a drop box: folders made there would not hide what they
    // hold.
    let makes_folders = signed.grant.items.iter().any(|i| i.rel_path.contains('/'));
    if makes_folders
        && (!ctx.allows(shared, &here, Caps::FILE_MANAGE)
            || inside_dropbox(shared, &req.area, folder.as_deref()).await)
    {
        return Err(ErrorCode::Forbidden);
    }
    let total = signed.grant.total_bytes();
    for item in &signed.grant.items {
        if crate::upload_gate::max_file_bytes(shared).is_some_and(|max| item.size > max) {
            return Err(ErrorCode::TooLarge);
        }
        if shared.moderation.is_denied(&item.root) {
            return Err(ErrorCode::Forbidden);
        }
    }
    match crate::upload_gate::check_quota(shared, ctx.account_id, total).await {
        Ok(()) => {}
        Err(crate::upload_gate::Refusal::Unavailable) => return Err(ErrorCode::Internal),
        Err(_) => return Err(ErrorCode::TooLarge),
    }
    if max_bytes > 0 && total > max_bytes {
        return Err(ErrorCode::TooLarge);
    }
    // Every check passed. Reaching a burrow that is not a peer means
    // connecting out on the person's behalf: that costs from their transfer
    // budget, and the pull's place is taken first, so the limits on pulls
    // bound connections in progress too.
    if peer_link.is_none() && !shared.rate_allow(Scope::Account(ctx.account_id), rl::TRANSFER) {
        return Err(ErrorCode::RateLimited);
    }
    let Some((pull_id, cancel)) = shared.s2s.reserve(ctx.account_id, per_account) else {
        return Err(ErrorCode::RateLimited);
    };
    let (link, hold, source_name) = match peer_link {
        Some(link) => (link, None, peer_name(shared, &source)),
        None => match dial_source(shared, &signed, &req.grant).await {
            // An approved peer keeps the name its operator approved, not
            // the one it gives itself.
            Ok(direct) if shared.peers.is_approved(&source) => {
                (direct.bulk, Some(direct.conn), peer_name(shared, &source))
            }
            Ok(direct) => (direct.bulk, Some(direct.conn), direct.name),
            Err(why) => {
                shared.s2s.release(pull_id);
                tracing::info!(%why, "pull: could not reach the source");
                return Err(ErrorCode::Unavailable);
            }
        },
    };
    let close_hold = |hold: Option<Box<dyn Connection>>| async move {
        if let Some(mut conn) = hold {
            conn.close().await;
        }
    };
    // Spent last, so a refusal above leaves the grant good for another try.
    match shared
        .files
        .spend_pull_grant(&signed.grant.nonce, signed.grant.expires_unix, now)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            shared.s2s.release(pull_id);
            close_hold(hold).await;
            return Err(ErrorCode::AlreadyExists);
        }
        Err(_) => {
            shared.s2s.release(pull_id);
            close_hold(hold).await;
            return Err(ErrorCode::Internal);
        }
    }
    let files = signed.grant.items.len() as u32;
    audit(
        shared,
        &ctx.login,
        "pull-accept",
        format!(
            "#{pull_id} from={source_name:?} key={} files={files} bytes={total} into={}/{}",
            rabbithole_identity::PublicKey(source).fingerprint(),
            req.area,
            folder.as_deref().unwrap_or("")
        ),
    );
    let job = Pull {
        shared: shared.clone(),
        id: pull_id,
        account_id: ctx.account_id,
        login: ctx.login.clone(),
        uploader: format!("{}@{}", ctx.screen_name, shared.origin_name()),
        source,
        source_name: source_name.clone(),
        grant_bytes: req.grant,
        grant: signed,
        area: req.area,
        folder,
        cancel,
        link,
        hold: Mutex::new(hold),
    };
    tokio::spawn(job.run());
    Ok(pf::RemotePullAccepted::new(
        pull_id,
        files,
        total,
        source_name,
    ))
}

/// One pull under way at the destination.
struct Pull {
    shared: Arc<Shared>,
    id: u64,
    account_id: i64,
    login: String,
    uploader: String,
    source: [u8; 32],
    source_name: String,
    grant_bytes: Vec<u8>,
    grant: SignedPullGrant,
    area: String,
    folder: Option<String>,
    cancel: Arc<AtomicBool>,
    /// The streams to fetch over: the federation session's, or this pull's
    /// own connection's.
    link: Arc<dyn BulkStreams>,
    /// This pull's own connection to a source that is not a peer, held open
    /// for as long as the pull runs.
    hold: Mutex<Option<Box<dyn Connection>>>,
}

/// What one send has spent on the source's swarm.
#[derive(Default)]
struct SwarmBudget {
    /// Every seeder this send has dialed.
    dialed: HashSet<String>,
    /// Seeders that gave nothing in a fetch that finished: not dialed again.
    useless: HashSet<String>,
    /// The swarm failed a file, or the source does not share it: the rest
    /// of this send comes from the source alone.
    off: bool,
}

/// Most seeders one send dials, whatever the source names.
const MAX_SEND_SEEDERS: usize = 32;
/// The largest file fetched from the swarm: past it, the swarm's resume
/// record (an entry per megabyte, rewritten as it goes) grows too big to
/// keep rewriting, and the source sends it.
const SWARM_MAX_BYTES: u64 = 64 << 30;
/// The shortest capability worth fetching with, in seconds.
const MIN_TOKEN_LIFE_SECS: i64 = 180;
/// How long before a capability's end a file still coming is asked for
/// again, in seconds: room for the seeders' clocks to run ahead.
const RENEW_MARGIN_SECS: i64 = 120;

impl SwarmBudget {
    /// The offered seeders this send will dial: addresses only (a seeder is
    /// never a name to look up), public unless the operator allows private
    /// ones, not one that gave nothing before, and no new one past
    /// [`MAX_SEND_SEEDERS`].
    fn usable(&mut self, offered: &[fp::SwarmSource], allow_private: bool) -> Usable {
        let mut out: Vec<rabbithole_swarm::SourcePeer> = Vec::new();
        let mut refused = 0;
        for source in offered.iter().take(fp::MAX_SWARM_SOURCES) {
            let Ok(addr) = source.endpoint.parse::<std::net::SocketAddr>() else {
                refused += 1;
                continue;
            };
            if addr.port() == 0 || !(allow_private || rabbithole_net::reach::is_public(addr.ip())) {
                refused += 1;
                continue;
            }
            let endpoint = addr.to_string();
            if self.useless.contains(&endpoint) || out.iter().any(|s| s.endpoint == endpoint) {
                continue;
            }
            if !self.dialed.contains(&endpoint) {
                if self.dialed.len() >= MAX_SEND_SEEDERS {
                    continue;
                }
                self.dialed.insert(endpoint.clone());
            }
            out.push(rabbithole_swarm::SourcePeer {
                endpoint,
                cert_fp: source.cert_fp,
            });
        }
        Usable {
            sources: out,
            refused,
        }
    }
}

/// The seeders of one offer a send will dial, and how many it would not
/// (a name, a port of 0, or an address it keeps off).
struct Usable {
    sources: Vec<rabbithole_swarm::SourcePeer>,
    refused: usize,
}

/// How fetching one file from the swarm went.
#[derive(Debug, PartialEq, Eq)]
enum Swarm {
    /// The whole file, verified.
    Fetched,
    /// The pull is no longer wanted here (pulls switched off, or the
    /// person's account closed): it stops, rather than going to the source.
    Stopped,
    /// Not tried: not wanted, too small, or nothing offered.
    Skipped,
    /// Tried, and the peers did not give the whole file.
    Failed,
}

/// Why fetching one file stopped.
#[derive(Debug, PartialEq, Eq)]
enum FetchError {
    /// The source no longer has it: skip it, carry on.
    Gone,
    /// The source refused: the grant no longer holds there.
    Refused,
    /// The session or the stream failed, or the source was too slow.
    Unreachable,
    /// It sent more or less than the grant said.
    WrongSize,
    /// Cancelled while it was coming.
    Cancelled,
    /// Staging on this side failed.
    Local,
}

/// Progress so far, for the pushes.
#[derive(Default, Clone, Copy)]
struct Tally {
    files: u32,
    bytes: u64,
    missing: u32,
    /// Files that came from the source's swarm peers.
    swarm: u32,
}

impl Pull {
    fn status(&self, state: u8, tally: Tally, reason: u8, landed: &str) -> pf::RemotePullStatus {
        pf::RemotePullStatus::new(
            self.id,
            state,
            tally.files,
            self.grant.grant.items.len() as u32,
            tally.bytes,
            self.grant.grant.total_bytes(),
            reason,
            tally.missing,
            self.source_name.clone(),
            self.area.clone(),
            landed.to_string(),
        )
    }

    fn push(&self, status: pf::RemotePullStatus) {
        self.shared.bus.publish(ServerEvent::PullStatus {
            to_account: self.account_id,
            status,
        });
    }

    fn staging(&self, index: usize) -> PathBuf {
        let data_dir = self.shared.config.read().data_dir.clone();
        crate::resolve_dir(&data_dir, Path::new("transfers"))
            .join(format!("pull-{}-{index}.part", self.id))
    }

    /// Where a file fetched from the swarm is put together: apart from
    /// [`Pull::staging`], so nothing of a swarm attempt given up on can
    /// touch the fetch from the source that follows it.
    fn swarm_staging(&self, index: usize) -> PathBuf {
        let data_dir = self.shared.config.read().data_dir.clone();
        crate::resolve_dir(&data_dir, Path::new("transfers"))
            .join(format!("pull-{}-{index}.swarm.part", self.id))
    }

    /// Fetch one file from the source's swarm peers into `dest`, when the
    /// operator wants that, the file is big enough to gain from it, and the
    /// source offers any peers. Anything but [`Swarm::Fetched`] leaves
    /// nothing behind, and the source is asked for the file then.
    async fn fetch_from_swarm(
        &self,
        link: &dyn BulkStreams,
        index: usize,
        item: &fp::PullItem,
        dest: &Path,
        budget: &mut SwarmBudget,
        progress: &mut impl FnMut(u64),
    ) -> Swarm {
        if budget.off || !(SWARM_MIN_BYTES..=SWARM_MAX_BYTES).contains(&item.size) {
            return Swarm::Skipped;
        }
        let _ = tokio::fs::create_dir_all(dest.parent().unwrap_or(Path::new("."))).await;
        let deadline =
            tokio::time::Instant::now() + STREAM_IDLE + Duration::from_secs(item.size / MIN_RATE);
        let total_units = item.size.div_ceil(rabbithole_swarm::UNIT_SIZE);
        let mut units_done = 0u64;
        let mut asked = false;
        loop {
            // Asked again when a capability nears its end and the file is
            // still coming: the operator's settings and the person's account
            // are read afresh each time.
            let (wanted, allow_private) = {
                let config = self.shared.config.read();
                (config.s2s_swarm, config.s2s_private_addresses)
            };
            if !wanted {
                budget.off = true;
                return self.gave_up(dest, asked).await;
            }
            if asked && !self.still_wanted().await {
                remove_swarm_partial(dest).await;
                return Swarm::Stopped;
            }
            let offer = match self
                .until_cancelled(ask_sources(link, &self.grant_bytes, index as u32))
                .await
            {
                Some(Ok(offer)) => offer,
                // Gone or changed at the source: nothing to say about the
                // rest of the send.
                Some(Err(Some(stream_status::GONE))) => return self.gave_up(dest, asked).await,
                // The source does not share its swarm, refuses, predates the
                // question, or did not answer: not asked again this send.
                Some(Err(_)) => {
                    budget.off = true;
                    return self.gave_up(dest, asked).await;
                }
                None => return self.gave_up(dest, true).await,
            };
            let life = offer.expires_unix.saturating_sub(now_unix());
            if offer.sources.is_empty() && !asked {
                return Swarm::Skipped;
            }
            // A capability too short to fetch with is no offer at all.
            if life < MIN_TOKEN_LIFE_SECS {
                budget.off = true;
                return self.gave_up(dest, asked).await;
            }
            let usable = budget.usable(&offer.sources, allow_private);
            if usable.sources.is_empty() {
                // Offered addresses this burrow will not dial: not asked
                // again. Only ones already spent this send: the next file
                // may name others.
                budget.off = usable.refused > 0;
                return self.gave_up(dest, asked).await;
            }
            asked = true;
            // Measured here, from the capability's known length: seeders
            // check its end on their own clocks.
            let window =
                Duration::from_secs((life.min(SWARM_TOKEN_SECS) - RENEW_MARGIN_SECS).max(0) as u64);
            let token_ends = tokio::time::Instant::now() + window;
            // A window must bring at least what the source is held to in
            // the same time, or the swarm is not worth asking again.
            let least = (MIN_RATE * window.as_secs() / rabbithole_swarm::UNIT_SIZE).max(1);
            let (tx, mut units) = tokio::sync::mpsc::unbounded_channel();
            let mut fetch = Box::pin(rabbithole_swarm::fetch_swarm_resumable_with_progress(
                &usable.sources,
                &offer.token,
                item.root,
                item.size,
                dest,
                tx,
            ));
            let until = tokio::time::sleep_until(token_ends.min(deadline));
            tokio::pin!(until);
            let mut watch = tokio::time::interval(Duration::from_millis(250));
            let before = units_done;
            let mut producing: HashSet<String> = HashSet::new();
            let ended = loop {
                tokio::select! {
                    result = &mut fetch => break Some(result.is_ok()),
                    Some(unit) = units.recv() => {
                        units_done = unit.done_units;
                        producing.insert(unit.endpoint);
                        progress((unit.done_units * rabbithole_swarm::UNIT_SIZE).min(item.size));
                    }
                    // Once every unit is in, only the check of the whole file
                    // is left: it is not cut off by the capability's end.
                    _ = &mut until, if units_done < total_units => break None,
                    _ = watch.tick() => {
                        if self.cancel.load(Ordering::Relaxed) || !self.shared.config.read().s2s_swarm {
                            break Some(false);
                        }
                    }
                }
            };
            // Dropping the fetch stops its workers.
            drop(fetch);
            match ended {
                Some(true) => return Swarm::Fetched,
                Some(false) => {
                    budget.off = true;
                    return self.gave_up(dest, true).await;
                }
                // The capability is about to run out: ask again, but only
                // while the swarm is getting somewhere and time remains. A
                // seeder that gave nothing this while is not dialed again.
                None if units_done >= before + least && tokio::time::Instant::now() < deadline => {
                    for peer in &usable.sources {
                        if !producing.contains(&peer.endpoint) {
                            budget.useless.insert(peer.endpoint.clone());
                        }
                    }
                }
                None => {
                    budget.off = true;
                    return self.gave_up(dest, true).await;
                }
            }
        }
    }

    /// The end of a swarm attempt that did not give the file: what it left
    /// is removed. [`Swarm::Failed`] if peers were tried, else skipped.
    async fn gave_up(&self, dest: &Path, tried: bool) -> Swarm {
        remove_swarm_partial(dest).await;
        if tried {
            Swarm::Failed
        } else {
            Swarm::Skipped
        }
    }

    /// `fut`, unless the pull is cancelled first (`None`).
    async fn until_cancelled<T>(&self, fut: impl std::future::Future<Output = T>) -> Option<T> {
        tokio::pin!(fut);
        let mut watch = tokio::time::interval(Duration::from_millis(250));
        loop {
            tokio::select! {
                value = &mut fut => return Some(value),
                _ = watch.tick() => {
                    if self.cancel.load(Ordering::Relaxed) {
                        return None;
                    }
                }
            }
        }
    }

    async fn run(self) {
        let (tally, outcome, landed) = self.fetch_all().await;
        let (state, reason) = match outcome {
            None => (pull_state::DONE, pull_reason::NONE),
            Some(reason) => (pull_state::FAILED, reason),
        };
        self.push(self.status(state, tally, reason, &landed));
        audit(
            &self.shared,
            &self.login,
            "pull-done",
            format!(
                "#{} files={} bytes={} missing={} swarm={} reason={reason}",
                self.id, tally.files, tally.bytes, tally.missing, tally.swarm
            ),
        );
        self.shared.s2s.release(self.id);
        let held = self.hold.lock().take();
        if let Some(mut conn) = held {
            conn.close().await;
        }
    }

    /// Whether this burrow still takes the pull: pulls on, and the person's
    /// account still open.
    async fn still_wanted(&self) -> bool {
        if !self.shared.config.read().s2s_pull_enabled {
            return false;
        }
        matches!(
            AccountsRepo(&self.shared.pool).by_id(self.account_id).await,
            Ok(Some(account)) if !account.disabled
        )
    }

    /// Fetch and file every item. Returns the tally, `None` or the reason it
    /// stopped, and where the first thing landed.
    async fn fetch_all(&self) -> (Tally, Option<u8>, String) {
        let mut tally = Tally::default();
        let mut landed = String::new();
        self.push(self.status(pull_state::RUNNING, tally, pull_reason::NONE, ""));
        let link = self.link.clone();
        // Source top-level name → the folder this pull made for it here.
        let mut tops: HashMap<String, String> = HashMap::new();
        let mut last_push = Instant::now();
        let mut swarm = SwarmBudget::default();
        for (index, item) in self.grant.grant.items.iter().enumerate() {
            if self.cancel.load(Ordering::Relaxed) {
                return (tally, Some(pull_reason::CANCELLED), landed);
            }
            if !self.still_wanted().await {
                return (tally, Some(pull_reason::STOPPED), landed);
            }
            let (parent, name) = match item.rel_path.split_once('/') {
                // Inside a folder: under the folder this pull made for it.
                Some((top, rest)) => {
                    let made = match tops.get(top) {
                        Some(made) => made.clone(),
                        None => match self.make_top(top).await {
                            Ok(made) => {
                                tops.insert(top.to_string(), made.clone());
                                made
                            }
                            Err(_) => return (tally, Some(pull_reason::INTERNAL), landed),
                        },
                    };
                    let rel = format!("{made}/{rest}");
                    let (dir, name) = rel.rsplit_once('/').expect("a nested path has a parent");
                    if self.ensure_folders(dir).await.is_err() {
                        return (tally, Some(pull_reason::INTERNAL), landed);
                    }
                    (join(self.folder.as_deref(), Some(dir)), name.to_string())
                }
                None => (self.folder.clone(), item.rel_path.clone()),
            };
            let base = tally.bytes;
            let mut progress = |got: u64| {
                if last_push.elapsed() >= PROGRESS_EVERY {
                    last_push = Instant::now();
                    let mut t = tally;
                    t.bytes = base + got;
                    self.push(self.status(pull_state::RUNNING, t, pull_reason::NONE, &landed));
                }
            };
            // From the source's swarm peers when both burrows allow it; from
            // the source itself for anything they do not give. Once the
            // swarm has failed a file, the rest of this send comes from the
            // source alone, so peers that do not answer cost one try, not one
            // per file.
            let swarm_staging = self.swarm_staging(index);
            let from_swarm = self
                .fetch_from_swarm(
                    link.as_ref(),
                    index,
                    item,
                    &swarm_staging,
                    &mut swarm,
                    &mut progress,
                )
                .await;
            if from_swarm == Swarm::Stopped {
                return (tally, Some(pull_reason::STOPPED), landed);
            }
            let from_swarm = from_swarm == Swarm::Fetched;
            // Cancelled while the swarm had it: not on to the source.
            if !from_swarm && self.cancel.load(Ordering::Relaxed) {
                return (tally, Some(pull_reason::CANCELLED), landed);
            }
            let staging = if from_swarm {
                swarm_staging
            } else {
                self.staging(index)
            };
            // A generous deadline: the idle timeout, plus the file at the
            // slowest rate a source may keep.
            let deadline = STREAM_IDLE + Duration::from_secs(item.size / MIN_RATE);
            let fetched = if from_swarm {
                Ok(())
            } else {
                tokio::time::timeout(
                    deadline,
                    fetch_item(
                        link.as_ref(),
                        &self.grant_bytes,
                        index as u32,
                        item.size,
                        &staging,
                        &self.cancel,
                        &mut progress,
                    ),
                )
                .await
                .unwrap_or(Err(FetchError::Unreachable))
            };
            match fetched {
                Ok(()) => {}
                Err(FetchError::Gone) => {
                    let _ = tokio::fs::remove_file(&staging).await;
                    tally.missing += 1;
                    continue;
                }
                Err(e) => {
                    let _ = tokio::fs::remove_file(&staging).await;
                    let reason = match e {
                        FetchError::Refused => pull_reason::SOURCE_REFUSED,
                        FetchError::Unreachable => pull_reason::SOURCE_UNREACHABLE,
                        FetchError::WrongSize => pull_reason::VERIFY_FAILED,
                        FetchError::Cancelled => pull_reason::CANCELLED,
                        FetchError::Gone | FetchError::Local => pull_reason::INTERNAL,
                    };
                    return (tally, Some(reason), landed);
                }
            }
            let filed = self
                .file_one(item, &staging, parent.as_deref(), &name)
                .await;
            let _ = tokio::fs::remove_file(&staging).await;
            match filed {
                Ok(path) => {
                    if landed.is_empty() {
                        landed = top_of(&path, self.folder.as_deref());
                    }
                    tally.files += 1;
                    tally.bytes += item.size;
                    tally.swarm += u32::from(from_swarm);
                    last_push = Instant::now();
                    self.push(self.status(pull_state::RUNNING, tally, pull_reason::NONE, &landed));
                }
                Err(reason) => return (tally, Some(reason), landed),
            }
        }
        (tally, None, landed)
    }

    /// Make the folder a pulled top-level folder lands in: its own name, or
    /// numbered when the destination already has one. Always a folder this
    /// pull made, so nothing merges into what was already there.
    async fn make_top(&self, top: &str) -> Result<String, FileError> {
        for n in 1..=MAX_NUMBERING {
            let name = if n == 1 {
                top.to_string()
            } else {
                numbered(top, n)
            };
            match self
                .shared
                .files
                .mkdir(&self.area, self.folder.as_deref(), &name, false)
                .await
            {
                Ok(_) => return Ok(name),
                Err(FileError::Exists) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(FileError::Exists)
    }

    /// Make each folder of `rel` (under the destination folder) that is not
    /// there yet. Its first segment is a folder this pull made.
    async fn ensure_folders(&self, rel: &str) -> Result<(), FileError> {
        let mut at = self.folder.clone();
        for segment in rel.split('/') {
            let path = join(at.as_deref(), Some(segment)).unwrap_or_default();
            match self.shared.files.node_by_path(&self.area, &path).await? {
                Some(node) if node.kind == KIND_FOLDER => {}
                Some(_) => return Err(FileError::NotAFolder),
                None => match self
                    .shared
                    .files
                    .mkdir(&self.area, at.as_deref(), segment, false)
                    .await
                {
                    Ok(_) | Err(FileError::Exists) => {}
                    Err(e) => return Err(e),
                },
            }
            at = Some(path);
        }
        Ok(())
    }

    /// Check a fetched file against every limit, then verify and file it
    /// under the person. Nothing reaches the store until the checks pass.
    /// Returns its path, or the reason the pull must stop.
    async fn file_one(
        &self,
        item: &fp::PullItem,
        staging: &Path,
        parent: Option<&str>,
        name: &str,
    ) -> Result<String, u8> {
        let _commit = crate::upload_gate::commit_lock(&self.shared).await;
        match crate::upload_gate::check(&self.shared, self.account_id, item.size).await {
            Ok(()) => {}
            Err(crate::upload_gate::Refusal::TooBig { .. }) => return Err(pull_reason::TOO_LARGE),
            Err(crate::upload_gate::Refusal::OverQuota { .. }) => {
                return Err(pull_reason::OVER_QUOTA)
            }
            Err(crate::upload_gate::Refusal::Unavailable) => return Err(pull_reason::INTERNAL),
        }
        if self.shared.moderation.is_denied(&item.root) {
            return Err(pull_reason::DENIED_CONTENT);
        }
        let blobs = self.shared.blobs.clone();
        let staged = staging.to_path_buf();
        let root = item.root;
        let verified =
            tokio::task::spawn_blocking(move || blobs.put_verified(&staged, &BlobId(root)))
                .await
                .map_err(|_| pull_reason::INTERNAL)?;
        if verified.is_err() {
            return Err(pull_reason::VERIFY_FAILED);
        }
        // A top-level name another upload took since: number on.
        let mut attempt = 1u32;
        let node = loop {
            let candidate = if attempt == 1 {
                name.to_string()
            } else {
                numbered(name, attempt)
            };
            match self
                .shared
                .files
                .add_file(
                    &self.area,
                    parent,
                    &candidate,
                    &item.root,
                    item.size as i64,
                    &item.mime,
                    "",
                    "",
                    &self.uploader,
                    self.account_id,
                )
                .await
            {
                Ok(node) => break node,
                Err(FileError::Exists) if attempt < MAX_NUMBERING => attempt += 1,
                Err(_) => return Err(pull_reason::INTERNAL),
            }
        };
        let _ = self
            .shared
            .files
            .set_provenance(node.id, &self.source_name, &self.source)
            .await;
        self.shared.bus.publish(ServerEvent::FileAdded {
            area: self.area.clone(),
            id: node.id,
        });
        Ok(node.path)
    }
}

/// Ask the source, on a stream of its own, for the swarm peers holding one
/// granted file. `Err(Some(status))` when it answers anything but OK (a
/// source that predates the question answers the empty first frame as a bad
/// request); `Err(None)` when the stream fails or goes quiet.
async fn ask_sources(
    link: &dyn BulkStreams,
    grant: &[u8],
    item: u32,
) -> Result<fp::PullSources, Option<u8>> {
    let ask = postcard::to_allocvec(&fp::PullStreamAsk::Sources {
        grant: grant.to_vec(),
        item,
    })
    .map_err(|_| None)?;
    let quiet = |_| None;
    let (mut send, mut recv) = tokio::time::timeout(STREAM_IDLE, link.open())
        .await
        .map_err(quiet)?
        .map_err(|_| None)?;
    tokio::time::timeout(STREAM_IDLE, async {
        write_framed(&mut send, &[]).await?;
        write_framed(&mut send, &ask).await
    })
    .await
    .map_err(quiet)?
    .map_err(|_| None)?;
    let _ = send.shutdown().await;
    let mut status = [0u8; 1];
    tokio::time::timeout(STREAM_IDLE, recv.read_exact(&mut status))
        .await
        .map_err(quiet)?
        .map_err(|_| None)?;
    if status[0] != stream_status::OK {
        return Err(Some(status[0]));
    }
    let bytes = tokio::time::timeout(STREAM_IDLE, read_framed(&mut recv, fp::MAX_SOURCES_ANSWER))
        .await
        .map_err(quiet)?
        .map_err(|_| None)?;
    postcard::from_bytes(&bytes).map_err(|_| None)
}

/// Remove what a swarm fetch given up on left: the partial file and its
/// progress sidecars.
async fn remove_swarm_partial(dest: &Path) {
    let _ = tokio::fs::remove_file(dest).await;
    let mut sidecar = dest.as_os_str().to_owned();
    sidecar.push(".rhstate");
    let _ = tokio::fs::remove_file(&sidecar).await;
    sidecar.push(".tmp");
    let _ = tokio::fs::remove_file(&sidecar).await;
}

/// The top-level thing a filed path landed as, relative to the area: the
/// destination folder plus the path's first segment below it.
fn top_of(path: &str, folder: Option<&str>) -> String {
    let below = match folder.filter(|f| !f.is_empty()) {
        Some(f) => path.strip_prefix(&format!("{f}/")).unwrap_or(path),
        None => path,
    };
    let first = below.split('/').next().unwrap_or(below);
    join(folder, Some(first)).unwrap_or_default()
}

fn join(base: Option<&str>, rel: Option<&str>) -> Option<String> {
    match (
        base.filter(|b| !b.is_empty()),
        rel.filter(|r| !r.is_empty()),
    ) {
        (Some(b), Some(r)) => Some(format!("{b}/{r}")),
        (Some(b), None) => Some(b.to_string()),
        (None, Some(r)) => Some(r.to_string()),
        (None, None) => None,
    }
}

/// Fetch one granted file from the source over a fresh bulk stream of the
/// federation session, into `staging`. `progress` hears the bytes so far.
async fn fetch_item(
    link: &dyn BulkStreams,
    grant: &[u8],
    item: u32,
    size: u64,
    staging: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> Result<(), FetchError> {
    let (mut send, mut recv) = tokio::time::timeout(STREAM_IDLE, link.open())
        .await
        .map_err(|_| FetchError::Unreachable)?
        .map_err(|_| FetchError::Unreachable)?;
    let request = postcard::to_allocvec(&PullStreamRequest {
        grant: grant.to_vec(),
        item,
        offset: 0,
    })
    .map_err(|_| FetchError::Local)?;
    tokio::time::timeout(STREAM_IDLE, write_framed(&mut send, &request))
        .await
        .map_err(|_| FetchError::Unreachable)?
        .map_err(|_| FetchError::Unreachable)?;
    let _ = send.shutdown().await;
    let mut status = [0u8; 1];
    match tokio::time::timeout(STREAM_IDLE, recv.read_exact(&mut status)).await {
        Ok(Ok(_)) => {}
        _ => return Err(FetchError::Unreachable),
    }
    match status[0] {
        stream_status::OK => {}
        stream_status::GONE => return Err(FetchError::Gone),
        stream_status::DENIED | stream_status::OFF => return Err(FetchError::Refused),
        _ => return Err(FetchError::Unreachable),
    }
    if let Some(dir) = staging.parent() {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|_| FetchError::Local)?;
    }
    let mut file = tokio::fs::File::create(staging)
        .await
        .map_err(|_| FetchError::Local)?;
    let mut got = 0u64;
    let mut buf = vec![0u8; CHUNK];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(FetchError::Cancelled);
        }
        let n = match tokio::time::timeout(STREAM_IDLE, recv.read(&mut buf)).await {
            Ok(Ok(n)) => n,
            _ => return Err(FetchError::Unreachable),
        };
        if n == 0 {
            break;
        }
        got += n as u64;
        if got > size {
            return Err(FetchError::WrongSize);
        }
        file.write_all(&buf[..n])
            .await
            .map_err(|_| FetchError::Local)?;
        progress(got);
    }
    if got != size {
        return Err(if got == 0 {
            FetchError::Unreachable
        } else {
            FetchError::WrongSize
        });
    }
    file.flush().await.map_err(|_| FetchError::Local)?;
    Ok(())
}

/// Write to a stream the far end may have stopped reading: give up after the
/// idle timeout rather than hold the task and its buffer.
async fn write_timed(send: &mut BulkSend, bytes: &[u8]) -> bool {
    matches!(
        tokio::time::timeout(STREAM_IDLE, send.write_all(bytes)).await,
        Ok(Ok(()))
    )
}

/// The moment a grant is judged at, when serving: a pull that started while
/// the grant stood may finish within the grace that follows it.
fn serve_clock(now: i64, expires: i64) -> i64 {
    if now >= expires && now < expires.saturating_add(SERVE_GRACE_SECS) {
        expires - 1
    } else {
        now
    }
}

/// At the source: whether a grant this burrow signed lets `peer_key` have
/// item `item` now, over a federation session (`direct` is `None`) or a pull
/// session opened with the grant whose nonce `direct` holds; and the item,
/// if the file is still the one granted. Refusals are stream statuses.
async fn granted_item(
    shared: &Shared,
    peer_key: &[u8; 32],
    direct: Option<[u8; 16]>,
    grant: &[u8],
    item: u32,
) -> Result<(fp::PullItem, [u8; 16]), u8> {
    if !shared.config.read().s2s_grants_enabled {
        return Err(stream_status::OFF);
    }
    let grant = SignedPullGrant::from_bytes(grant).map_err(|_| stream_status::BAD)?;
    let clock = serve_clock(now_unix(), grant.grant.expires_unix);
    grant
        .check(&shared.server_key, peer_key, clock)
        .map_err(|_| stream_status::DENIED)?;
    // A peer over its federation session; anyone else only over a pull
    // session that proved its key, and only while the operator sends to
    // any burrow.
    let allowed = shared.peers.is_approved(peer_key)
        || (direct.is_some() && shared.config.read().s2s_grants_to_any);
    // A pull session serves the grant it was opened with, and no other.
    if !allowed || direct.is_some_and(|nonce| nonce != grant.grant.nonce) {
        return Err(stream_status::DENIED);
    }
    // A grant stops with the account of the person who asked for it.
    if !grant_stands(shared, &grant.grant.nonce).await {
        return Err(stream_status::DENIED);
    }
    let nonce = grant.grant.nonce;
    let item = grant
        .grant
        .items
        .get(item as usize)
        .cloned()
        .ok_or(stream_status::BAD)?;
    let node = shared
        .files
        .node(item.node_id)
        .await
        .ok()
        .flatten()
        .ok_or(stream_status::GONE)?;
    let unchanged = node.kind == KIND_FILE
        && node.blob_id == Some(item.root)
        && node.size.max(0) as u64 == item.size;
    if !unchanged || shared.moderation.file_quarantined(Some(&item.root)) {
        return Err(stream_status::GONE);
    }
    Ok((item, nonce))
}

/// At the source: answer one pull stream a federation peer opened. The
/// grant must be this burrow's own, name this peer as its fetcher, and still
/// stand (with grace for a pull under way); the file must still be the one
/// granted. A stream that opens with an empty frame asks something else
/// ([`fp::PullStreamAsk`]) in the next.
pub async fn serve_pull_stream(
    shared: Arc<Shared>,
    peer_key: [u8; 32],
    direct: Option<[u8; 16]>,
    mut send: BulkSend,
    mut recv: BulkRecv,
) {
    let answer = async {
        let bytes =
            tokio::time::timeout(STREAM_IDLE, read_framed(&mut recv, fp::MAX_STREAM_REQUEST))
                .await
                .ok()
                .and_then(Result::ok)
                .ok_or(stream_status::BAD)?;
        if bytes.is_empty() {
            return Err(ANSWERED_ELSEWHERE);
        }
        let req: PullStreamRequest =
            postcard::from_bytes(&bytes).map_err(|_| stream_status::BAD)?;
        let (item, nonce) = granted_item(&shared, &peer_key, direct, &req.grant, req.item).await?;
        if req.offset > item.size {
            return Err(stream_status::BAD);
        }
        Ok((item.root, item.size, req.offset, nonce))
    }
    .await;
    let (root, size, mut offset, nonce) = match answer {
        Ok(v) => v,
        Err(ANSWERED_ELSEWHERE) => {
            answer_ask(&shared, &peer_key, direct, send, recv).await;
            return;
        }
        Err(status) => {
            let _ = write_timed(&mut send, &[status]).await;
            let _ = send.shutdown().await;
            return;
        }
    };
    if !write_timed(&mut send, &[stream_status::OK]).await {
        return;
    }
    // Under a bandwidth cap, write in pieces that go out at least twice a
    // second, so the destination never sits silent past its idle timeout.
    let rate = shared.config.read().transfer_rate_bytes_per_sec;
    let piece = if rate > 0 {
        ((rate / 2) as usize).clamp(1024, CHUNK)
    } else {
        CHUNK
    };
    let mut checked = Instant::now();
    while offset < size {
        // A long file is not sent on past its person's account being
        // disabled, nor past grants being switched off.
        if checked.elapsed() >= STANDING_EVERY {
            checked = Instant::now();
            if !shared.config.read().s2s_grants_enabled || !grant_stands(&shared, &nonce).await {
                break;
            }
        }
        let want = ((size - offset).min(piece as u64)) as usize;
        let blobs = shared.blobs.clone();
        let at = offset;
        let chunk =
            match tokio::task::spawn_blocking(move || blobs.read_range(&BlobId(root), at, want))
                .await
            {
                Ok(Ok(c)) if !c.is_empty() => c,
                _ => break,
            };
        let n = chunk.len();
        if !write_timed(&mut send, &chunk).await {
            return;
        }
        offset += n as u64;
        crate::handlers9::throttle_after(rate, n).await;
    }
    let _ = send.shutdown().await;
}

/// Not a stream status: the stream opened with an empty frame, and its
/// question is answered by [`answer_ask`].
const ANSWERED_ELSEWHERE: u8 = u8::MAX;

/// How long a capability for the swarm peers holding a sent file stands:
/// as long as a person's ticket. A file still coming when it ends is asked
/// for again, so an account disabled or a permission taken away holds for at
/// most this long at the seeders.
const SWARM_TOKEN_SECS: i64 = 600;

/// At the source: answer a [`fp::PullStreamAsk`], the frame after an empty
/// one.
async fn answer_ask(
    shared: &Shared,
    peer_key: &[u8; 32],
    direct: Option<[u8; 16]>,
    mut send: BulkSend,
    mut recv: BulkRecv,
) {
    let answer = async {
        let bytes =
            tokio::time::timeout(STREAM_IDLE, read_framed(&mut recv, fp::MAX_STREAM_REQUEST))
                .await
                .ok()
                .and_then(Result::ok)
                .ok_or(stream_status::BAD)?;
        match postcard::from_bytes::<fp::PullStreamAsk>(&bytes) {
            Ok(fp::PullStreamAsk::Sources { grant, item }) => {
                let (item, nonce) = granted_item(shared, peer_key, direct, &grant, item).await?;
                if !may_use_swarm(shared, &nonce).await {
                    return Err(stream_status::OFF);
                }
                let serve_until = SignedPullGrant::from_bytes(&grant)
                    .map(|g| g.grant.expires_unix.saturating_add(SERVE_GRACE_SECS))
                    .map_err(|_| stream_status::BAD)?;
                swarm_offer(shared, peer_key, &item, serve_until)
            }
            Err(_) => Err(stream_status::BAD),
        }
    }
    .await;
    match answer.and_then(|offer| postcard::to_allocvec(&offer).map_err(|_| stream_status::BAD)) {
        Ok(bytes) => {
            if write_timed(&mut send, &[stream_status::OK]).await {
                let _ = tokio::time::timeout(STREAM_IDLE, write_framed(&mut send, &bytes)).await;
            }
        }
        Err(status) => {
            let _ = write_timed(&mut send, &[status]).await;
        }
    }
    let _ = send.shutdown().await;
}

/// Whether a grant this burrow signed still stands for its person: the
/// account its nonce names is there and not disabled. A grant from before
/// 0.228 names no one, and stands.
async fn grant_stands(shared: &Shared, nonce: &[u8; 16]) -> bool {
    match nonce_account(&nonce_key(shared), nonce) {
        None => true,
        Some(account_id) => matches!(
            AccountsRepo(&shared.pool).by_id(account_id).await,
            Ok(Some(account)) if !account.disabled
        ),
    }
}

/// Whether the person who asked for a grant may, now, find and fetch from
/// this burrow's swarm, as `FindSources` and `SourceTicket` require. False
/// for a grant whose nonce names no one (made before 0.228).
async fn may_use_swarm(shared: &Shared, nonce: &[u8; 16]) -> bool {
    let Some(account_id) = nonce_account(&nonce_key(shared), nonce) else {
        return false;
    };
    let Ok(Some(account)) = AccountsRepo(&shared.pool).by_id(account_id).await else {
        return false;
    };
    if account.disabled {
        return false;
    }
    let subject = Subject {
        account_id: account.id,
        role: Role::from_ordinal(account.role),
        class_id: account.class_id,
        class_mask: shared.classes.mask(account.class_id),
        grant_mask: account.grant_mask,
        revoke_mask: account.revoke_mask,
    };
    shared.perms.allows(&subject, "swarm", Caps::FILE_LIST)
        && shared.perms.allows(&subject, "swarm", Caps::FILE_DOWNLOAD)
}

/// The swarm peers here holding a granted file, for the burrow it was sent
/// to, with a capability naming that burrow, good for ten minutes and never past
/// `serve_until` (when this burrow stops serving the grant itself). Only
/// while the operator shares this burrow's swarm; never a session that is
/// invisible or gone, and never one holding a different size.
fn swarm_offer(
    shared: &Shared,
    peer_key: &[u8; 32],
    item: &fp::PullItem,
    serve_until: i64,
) -> Result<fp::PullSources, u8> {
    if !shared.config.read().s2s_swarm_sources {
        return Err(stream_status::OFF);
    }
    let mut sources: Vec<fp::SwarmSource> = Vec::new();
    for advert in shared.swarm.find(&item.root) {
        if sources.len() >= fp::MAX_SWARM_SOURCES {
            break;
        }
        let visible = shared
            .presence
            .get(advert.session_id)
            .is_some_and(|entry| !entry.is_invisible());
        if !visible || advert.size != item.size {
            continue;
        }
        let Some(contact) = shared.swarm.contact(advert.session_id) else {
            continue;
        };
        let Some(endpoint) = socket_endpoint(&contact.endpoint) else {
            continue;
        };
        if !sources.iter().any(|s| s.endpoint == endpoint) {
            sources.push(fp::SwarmSource {
                endpoint,
                cert_fp: contact.cert_fp,
            });
        }
    }
    let expires_unix = (now_unix() + SWARM_TOKEN_SECS).min(serve_until);
    let token = rabbithole_swarm::S2sCapToken::issue(
        &IdentityKey::from_seed(&shared.server_signing_seed),
        item.root,
        *peer_key,
        expires_unix,
    )
    .map_err(|_| stream_status::BAD)?;
    Ok(fp::PullSources {
        token: token.to_bytes(),
        expires_unix,
        sources,
    })
}

/// A seeder's `ip:port` as a socket address writes it: an IPv6 address in
/// brackets. Seeders' contact cards join the address and port with a bare
/// colon.
fn socket_endpoint(endpoint: &str) -> Option<String> {
    if let Ok(addr) = endpoint.parse::<std::net::SocketAddr>() {
        return Some(addr.to_string());
    }
    let (ip, port) = endpoint.rsplit_once(':')?;
    let ip: std::net::IpAddr = ip.parse().ok()?;
    let port: u16 = port.parse().ok()?;
    Some(std::net::SocketAddr::new(ip, port).to_string())
}

// ---------------------------------------------------------------------------
// Sources that are not federation peers: the destination connects to the
// source's QUIC client port, proves the key the grant names, and opens a
// pull session.
// ---------------------------------------------------------------------------

/// How long connecting to one of a source's addresses, or opening a pull
/// session there, may take.
const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
/// How long reaching a source may take across all its addresses: the
/// person's request waits on it.
const DIAL_BUDGET: Duration = Duration::from_secs(20);
/// The longest name a source that is not a peer may go by here.
const MAX_SOURCE_NAME: usize = 64;
/// Pull sessions this burrow serves at once, for every burrow fetching
/// directly.
const MAX_PULL_SESSIONS: usize = 32;
/// Pull sessions open at once for grants one person asked for.
const MAX_PULL_SESSIONS_PER_ACCOUNT: usize = 4;
/// How often a stream or pull session under way checks that its grant still
/// stands: its person's account, and the operator's settings.
const STANDING_EVERY: Duration = Duration::from_secs(15);
/// How long past a grant's expiry a pull session may still open: room for
/// two burrows' clocks to disagree, not the grace a pull under way gets.
const OPEN_SKEW_SECS: i64 = 300;
/// How long a reply on a pull session's control stream may take to go.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);
/// Pull streams a pull session serves at once.
const SESSION_STREAMS: usize = 8;

/// A pull's own connection to a source that is not a peer.
struct Direct {
    conn: Box<dyn Connection>,
    bulk: Arc<dyn BulkStreams>,
    /// What the source calls itself, for the person and the record.
    name: String,
}

/// Connect to the source a grant names, at the addresses it carries, pinned
/// to its certificate, and open a pull session proving this burrow's key.
async fn dial_source(
    shared: &Arc<Shared>,
    signed: &SignedPullGrant,
    grant: &[u8],
) -> Result<Direct, String> {
    match tokio::time::timeout(DIAL_BUDGET, dial_endpoints(shared, signed, grant)).await {
        Ok(result) => result,
        Err(_) => Err("timed out".into()),
    }
}

/// What a source that is not a peer calls itself, fit to show: printable,
/// one line, not too long, and its key's fingerprint when that leaves
/// nothing.
fn source_name(claimed: &str, key: &[u8; 32]) -> String {
    let name: String = claimed
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| !c.is_control() && !is_invisible(*c))
        .take(MAX_SOURCE_NAME)
        .collect();
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        rabbithole_identity::PublicKey(*key).fingerprint()
    } else {
        name
    }
}

/// Characters that draw nothing, or turn the text around them.
fn is_invisible(c: char) -> bool {
    matches!(c,
        '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}'
        | '\u{feff}' | '\u{00ad}')
}

async fn dial_endpoints(
    shared: &Arc<Shared>,
    signed: &SignedPullGrant,
    grant: &[u8],
) -> Result<Direct, String> {
    let g = &signed.grant;
    if g.endpoints.is_empty() {
        return Err("the grant carries no address".into());
    }
    let allow_private = shared.config.read().s2s_private_addresses;
    let fingerprint = CertFingerprint(g.tls_fingerprint);
    let mut last = String::new();
    for endpoint in &g.endpoints {
        let addr = match rabbithole_net::reach::resolve_public(endpoint, allow_private).await {
            Ok(addr) => addr,
            Err(e) => {
                last = e.to_string();
                continue;
            }
        };
        let transport = QuicTransport::new("localhost", ServerAuth::Pinned(fingerprint));
        let conn =
            match tokio::time::timeout(DIAL_TIMEOUT, transport.connect(&addr.to_string())).await {
                Ok(Ok(conn)) => conn,
                Ok(Err(e)) => {
                    last = format!("{endpoint}: {e}");
                    continue;
                }
                Err(_) => {
                    last = format!("{endpoint}: timed out");
                    continue;
                }
            };
        let mut conn = conn;
        match tokio::time::timeout(DIAL_TIMEOUT, open_session(&mut conn, shared, g, grant)).await {
            Ok(Ok(name)) => {
                let Some(bulk) = conn.bulk() else {
                    conn.close().await;
                    last = format!("{endpoint}: no streams");
                    continue;
                };
                return Ok(Direct {
                    conn,
                    bulk: Arc::from(bulk),
                    name: source_name(&name, &g.source_key),
                });
            }
            Ok(Err(e)) => last = format!("{endpoint}: {e}"),
            Err(_) => last = format!("{endpoint}: timed out"),
        }
        conn.close().await;
    }
    Err(last)
}

/// The reply to request `id`, skipping pushes.
async fn reply(conn: &mut Box<dyn Connection>, id: RequestId) -> Result<Frame, String> {
    loop {
        let frame = conn
            .recv()
            .await
            .map_err(|e| e.to_string())?
            .ok_or("the source closed the connection")?;
        if frame.kind == FrameKind::Reply && frame.id == id {
            return match frame.error {
                Some(code) => Err(format!("the source refused: {code:?}")),
                None => Ok(frame),
            };
        }
    }
}

/// Hello offering this burrow's key, the proof of it bound to the source's
/// certificate, then the grant. Returns the source's name.
async fn open_session(
    conn: &mut Box<dyn Connection>,
    shared: &Shared,
    g: &fp::PullGrant,
    grant: &[u8],
) -> Result<String, String> {
    let e = |e: rabbithole_proto::ProtoError| e.to_string();
    let hello = Hello::new(
        "burrow",
        env!("CARGO_PKG_VERSION"),
        CapabilitySet::default(),
    )
    .with_pubkey(Some(shared.server_key));
    conn.send(Frame::request(RequestId(1), &hello).map_err(e)?)
        .await
        .map_err(|e| e.to_string())?;
    let ack: HelloAck = reply(conn, RequestId(1))
        .await?
        .decode::<HelloAck>()
        .ok_or("no handshake")?
        .map_err(e)?;
    // The pin already authenticated the certificate; the key it claims must
    // be the one that signed the grant.
    if ack.server_key != g.source_key {
        return Err("the source is not the burrow that signed the grant".into());
    }
    let nonce = ack
        .challenge
        .ok_or("the source did not challenge this burrow's key")?;
    let key = IdentityKey::from_seed(&shared.server_signing_seed);
    let proof = key.sign(&key_auth_message(&g.tls_fingerprint, &nonce));
    conn.send(Frame::request(RequestId(2), &KeyProof::new(proof.0.to_vec())).map_err(e)?)
        .await
        .map_err(|e| e.to_string())?;
    reply(conn, RequestId(2)).await?;
    conn.send(Frame::request(RequestId(3), &PullSessionOpen::new(grant.to_vec())).map_err(e)?)
        .await
        .map_err(|e| e.to_string())?;
    reply(conn, RequestId(3)).await?;
    Ok(ack.server_name)
}

/// At the source, before any sign-in: whether a connection may become a pull
/// session. `key` is the key it proved over QUIC, bound to this burrow's
/// certificate; `None` if it proved none.
pub async fn open_pull_session(
    shared: &Arc<Shared>,
    key: Option<[u8; 32]>,
    bound: bool,
    grant: &[u8],
) -> Result<PullSession, ErrorCode> {
    let key = key.filter(|_| bound).ok_or(ErrorCode::Unauthenticated)?;
    let (grants, to_any) = {
        let config = shared.config.read();
        (config.s2s_grants_enabled, config.s2s_grants_to_any)
    };
    if !grants || !(to_any || shared.peers.is_approved(&key)) {
        return Err(ErrorCode::Unsupported);
    }
    if grant.len() > fp::MAX_GRANT_BYTES {
        return Err(ErrorCode::BadRequest);
    }
    let signed = SignedPullGrant::from_bytes(grant).map_err(|_| ErrorCode::BadRequest)?;
    let expires_unix = signed.grant.expires_unix;
    signed
        .check(
            &shared.server_key,
            &key,
            open_clock(now_unix(), expires_unix),
        )
        .map_err(|_| ErrorCode::Forbidden)?;
    let nonce = signed.grant.nonce;
    if !grant_stands(shared, &nonce).await {
        return Err(ErrorCode::Forbidden);
    }
    let account = nonce_account(&nonce_key(shared), &nonce);
    {
        let mut sessions = shared.s2s.sessions.lock();
        if sessions.contains_key(&nonce) {
            return Err(ErrorCode::AlreadyExists);
        }
        let theirs = sessions
            .values()
            .filter(|a| a.is_some() && **a == account)
            .count();
        if sessions.len() >= MAX_PULL_SESSIONS || theirs >= MAX_PULL_SESSIONS_PER_ACCOUNT {
            return Err(ErrorCode::RateLimited);
        }
        sessions.insert(nonce, account);
    }
    Ok(PullSession {
        shared: shared.clone(),
        key,
        nonce,
        expires_unix,
    })
}

/// The clock a pull session is opened by: a grant opens one only while it
/// stands, give or take the two burrows' clocks.
fn open_clock(now: i64, expires: i64) -> i64 {
    if now >= expires && now < expires.saturating_add(OPEN_SKEW_SECS) {
        expires - 1
    } else {
        now
    }
}

/// A pull session's place at the source, given up when it is dropped: then
/// the grant may open another (a destination that lost its connection
/// retries).
pub struct PullSession {
    shared: Arc<Shared>,
    key: [u8; 32],
    nonce: [u8; 16],
    expires_unix: i64,
}

impl Drop for PullSession {
    fn drop(&mut self) {
        self.shared.s2s.sessions.lock().remove(&self.nonce);
    }
}

/// At the source: a connection that opened a pull session serves pull
/// streams for the key it proved, and nothing else, until it closes, the
/// grant's grace runs out, or the burrow shuts down.
pub async fn run_pull_session(
    mut conn: Box<dyn Connection>,
    shared: Arc<Shared>,
    session: PullSession,
) -> anyhow::Result<()> {
    let Some(bulk) = conn.bulk() else {
        conn.close().await;
        return Ok(());
    };
    let (key, nonce, expires_unix) = (session.key, session.nonce, session.expires_unix);
    let limit = Arc::new(tokio::sync::Semaphore::new(SESSION_STREAMS));
    let server = {
        let shared = shared.clone();
        tokio::spawn(async move {
            while let Ok((mut send, recv)) = bulk.accept().await {
                let Ok(permit) = limit.clone().try_acquire_owned() else {
                    // Too many at once: this one is refused, the rest go on.
                    let _ = write_timed(&mut send, &[stream_status::BAD]).await;
                    continue;
                };
                let shared = shared.clone();
                tokio::spawn(async move {
                    serve_pull_stream(shared, key, Some(nonce), send, recv).await;
                    drop(permit);
                });
            }
        })
    };
    let lasts = (expires_unix.saturating_add(SERVE_GRACE_SECS) - now_unix()).max(0) as u64;
    let until = tokio::time::sleep(Duration::from_secs(lasts));
    tokio::pin!(until);
    let mut bus = shared.bus.subscribe();
    let mut standing = tokio::time::interval(STANDING_EVERY);
    standing.tick().await;
    loop {
        tokio::select! {
            _ = &mut until => break,
            _ = standing.tick() => {
                // The session ends with its grant's person's account, or
                // when the operator stops sending to this burrow.
                let still = {
                    let config = shared.config.read();
                    config.s2s_grants_enabled
                        && (config.s2s_grants_to_any || shared.peers.is_approved(&key))
                };
                if !still || !grant_stands(&shared, &nonce).await {
                    break;
                }
            }
            ev = bus.recv() => {
                use tokio::sync::broadcast::error::RecvError;
                if matches!(ev, Ok(ServerEvent::Shutdown) | Err(RecvError::Closed)) {
                    break;
                }
            }
            frame = conn.recv() => {
                let Ok(Some(frame)) = frame else { break };
                if frame.kind != FrameKind::Request {
                    continue;
                }
                let answer = if frame.decode::<rabbithole_proto::session::Ping>().is_some() {
                    Frame::reply_to(&frame, &rabbithole_proto::session::Pong)
                        .unwrap_or_else(|_| Frame::error_reply(&frame, ErrorCode::Internal))
                } else {
                    Frame::error_reply(&frame, ErrorCode::Unsupported)
                };
                // A fetcher that stops reading its replies is let go, so the
                // grace and a shutdown are still watched.
                if !matches!(tokio::time::timeout(REPLY_TIMEOUT, conn.send(answer)).await, Ok(Ok(()))) {
                    break;
                }
            }
        }
    }
    server.abort();
    conn.close().await;
    drop(session);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_send_dials_only_seeder_addresses_it_may_and_only_so_many() {
        let seeder = |endpoint: &str| fp::SwarmSource {
            endpoint: endpoint.into(),
            cert_fp: [1; 32],
        };
        let mut budget = SwarmBudget::default();
        let offered = [
            seeder("203.0.113.9:4000"), // documentation range: not public
            seeder("8.8.8.8:4000"),
            seeder("8.8.8.8:4000"),        // the same one twice
            seeder("seeder.example:4000"), // a name, never looked up
            seeder("1.1.1.1:0"),
            seeder("[2606:4700::1111]:4000"),
            seeder("127.0.0.1:4000"),
        ];
        let first = budget.usable(&offered, false);
        let usable: Vec<String> = first.sources.into_iter().map(|s| s.endpoint).collect();
        assert_eq!(usable, ["8.8.8.8:4000", "[2606:4700::1111]:4000"]);
        // A documentation address, a name, a port of 0 and a loopback one.
        assert_eq!(first.refused, 4);
        // Private addresses only when the operator allows them.
        let local = budget.usable(&offered, true);
        assert!(local.sources.iter().any(|s| s.endpoint == "127.0.0.1:4000"));
        // One that gave nothing is not dialed again this send.
        budget.useless.insert("8.8.8.8:4000".into());
        assert!(budget
            .usable(&offered, false)
            .sources
            .iter()
            .all(|s| s.endpoint != "8.8.8.8:4000"));
        // No more than so many different seeders, whatever the source names.
        let many: Vec<fp::SwarmSource> = (1..=200)
            .map(|n| seeder(&format!("8.8.{}.{}:4000", n / 250, n % 250 + 1)))
            .collect();
        let mut budget = SwarmBudget::default();
        let mut total = 0;
        for chunk in many.chunks(fp::MAX_SWARM_SOURCES) {
            total += budget.usable(chunk, false).sources.len();
        }
        assert_eq!(total, MAX_SEND_SEEDERS);
    }

    #[test]
    fn a_grants_nonce_names_its_person_to_the_burrow_that_signed_it_alone() {
        let key = [3u8; 32];
        // Fixed randomness, so the test says the same thing every run.
        let random = |i: u32| -> [u8; 16] {
            blake3::hash(&i.to_le_bytes()).as_bytes()[..16]
                .try_into()
                .unwrap()
        };
        for (i, account) in [0i64, 1, 42, (1 << 48) - 1].into_iter().enumerate() {
            let n = grant_nonce_from(&key, account, random(i as u32));
            assert_eq!(nonce_account(&key, &n), Some(account));
            // Another burrow's key reads nothing from it.
            assert_eq!(nonce_account(&[4u8; 32], &n), None);
        }
        // The randomness is the grant's own: two grants for one person
        // differ.
        assert_ne!(
            grant_nonce_from(&key, 7, random(1)),
            grant_nonce_from(&key, 7, random(2))
        );
        // An id that does not fit is not written in at all.
        let plain = grant_nonce_from(&key, 1 << 48, random(9));
        assert_eq!(nonce_account(&key, &plain), None);
        // Nonces made before 0.228 were only random, and name no one.
        let named = (100..1100)
            .filter(|&i| nonce_account(&key, &random(i)).is_some())
            .count();
        assert_eq!(named, 0, "a random nonce names no one");
    }

    #[test]
    fn a_source_hands_out_seeder_addresses_as_socket_addresses() {
        assert_eq!(
            socket_endpoint("192.0.2.1:4000").as_deref(),
            Some("192.0.2.1:4000")
        );
        assert_eq!(
            socket_endpoint("2001:db8::1:4000").as_deref(),
            Some("[2001:db8::1]:4000")
        );
        assert_eq!(socket_endpoint("[::1]:9").as_deref(), Some("[::1]:9"));
        assert_eq!(socket_endpoint("host.example:4000"), None);
        assert_eq!(socket_endpoint("1.2.3.4"), None);
    }

    #[test]
    fn a_pull_session_opens_only_while_its_grant_stands() {
        // Before expiry: the real time.
        assert_eq!(open_clock(100, 200), 100);
        // Just past it, within the skew: as if at the last second.
        assert_eq!(open_clock(200, 200), 199);
        assert_eq!(open_clock(200 + OPEN_SKEW_SECS - 1, 200), 199);
        // Not the six hours a pull under way gets.
        assert_eq!(open_clock(200 + OPEN_SKEW_SECS, 200), 200 + OPEN_SKEW_SECS);
    }

    const _: () = assert!(OPEN_SKEW_SECS < SERVE_GRACE_SECS);

    #[test]
    fn a_source_that_is_not_a_peer_goes_by_a_name_fit_to_show() {
        let key = [9u8; 32];
        assert_eq!(source_name("Lonely  Source", &key), "Lonely Source");
        assert_eq!(source_name("Evil\u{202e}txt.exe\n", &key), "Eviltxt.exe");
        assert_eq!(source_name("a\tb", &key), "a b");
        assert_eq!(source_name(&"x".repeat(200), &key).len(), MAX_SOURCE_NAME);
        let fingerprint = rabbithole_identity::PublicKey(key).fingerprint();
        assert_eq!(source_name(" \u{200b}\u{0007} ", &key), fingerprint);
    }

    #[test]
    fn a_clash_is_numbered_the_way_downloads_number_one() {
        assert_eq!(numbered("mix.mp3", 2), "mix (2).mp3");
        assert_eq!(numbered("LICENSE", 3), "LICENSE (3)");
        assert_eq!(numbered(".profile", 2), ".profile (2)");
        assert_eq!(numbered("a.tar.gz", 2), "a.tar (2).gz");
        let taken = ["tapes", "tapes (2)"];
        assert_eq!(unique_name("tapes", |n| taken.contains(&n)), "tapes (3)");
        assert_eq!(unique_name("fresh", |n| taken.contains(&n)), "fresh");
    }

    #[test]
    fn a_numbered_name_stays_within_the_librarys_limit() {
        // 61 two-byte letters and ".flac": 127 bytes, a name the library takes.
        let long = format!("{}.flac", "\u{e9}".repeat(61));
        assert_eq!(long.len(), 127);
        let n = numbered(&long, 12);
        assert!(n.len() <= fp::MAX_SEGMENT, "{} bytes", n.len());
        assert!(n.ends_with(" (12).flac"));
        assert!(fp::segment_is_acceptable(&n));
        let bare = "x".repeat(128);
        assert_eq!(numbered(&bare, 2).len(), 128);
    }

    #[test]
    fn a_pull_under_way_outlives_its_grant_by_the_grace_and_no_more() {
        assert_eq!(serve_clock(100, 200), 100, "before expiry: the real time");
        assert_eq!(serve_clock(200, 200), 199, "at expiry: still standing");
        assert_eq!(serve_clock(200 + SERVE_GRACE_SECS - 1, 200), 199);
        assert_eq!(
            serve_clock(200 + SERVE_GRACE_SECS, 200),
            200 + SERVE_GRACE_SECS,
            "past the grace: lapsed"
        );
    }

    #[test]
    fn places_are_reserved_within_both_limits_at_once() {
        let s = S2sState::default();
        let (a, _) = s.reserve(1, 2).unwrap();
        s.reserve(1, 2).unwrap();
        assert!(s.reserve(1, 2).is_none(), "the account's limit");
        assert!(s.reserve(2, 2).is_some(), "another account has its own");
        s.release(a);
        assert!(s.reserve(1, 2).is_some(), "a place freed is a place taken");
        let many = S2sState::default();
        for account in 0..MAX_RUNNING as i64 {
            many.reserve(account, 0).unwrap();
        }
        assert!(many.reserve(99, 0).is_none(), "the burrow's limit");
    }

    #[test]
    fn a_landed_path_names_its_top_level_thing() {
        assert_eq!(top_of("tapes (2)/b/c.txt", None), "tapes (2)");
        assert_eq!(top_of("inbox/tapes/b.txt", Some("inbox")), "inbox/tapes");
        assert_eq!(top_of("notes (2).txt", None), "notes (2).txt");
        assert_eq!(join(None, None), None);
        assert_eq!(join(Some(""), Some("a")).as_deref(), Some("a"));
        assert_eq!(join(Some("in"), Some("a/b")).as_deref(), Some("in/a/b"));
    }
}
