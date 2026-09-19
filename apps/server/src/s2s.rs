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
use rabbithole_net::{read_framed, write_framed, BulkRecv, BulkSend, BulkStreams, Connection};
use rabbithole_proto::filelib::{pull_reason, pull_state};
use rabbithole_proto::{filelib as pf, ErrorCode, Frame};
use rabbithole_server_core::files::{KIND_FILE, KIND_FOLDER};
use rabbithole_server_core::ratelimit::{class as rl, Scope};
use rabbithole_server_core::{Caps, FileError, ServerEvent};
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
        if name.starts_with("pull-") && name.ends_with(".part") {
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

    if let Some(Ok(req)) = frame.decode::<pf::PullGrantRequest>() {
        match grant(shared, ctx, &req).await {
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
    if req.fetcher_key == shared.server_key || !shared.peers.is_approved(&req.fetcher_key) {
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
    let now = now_unix();
    let unsigned = fp::PullGrant {
        version: fp::PULL_GRANT_VERSION,
        source_key: shared.server_key,
        fetcher_key: req.fetcher_key,
        issued_unix: now,
        expires_unix: now + GRANT_TTL_SECS,
        nonce: nonce(),
        items,
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
    // The source must be a peer this burrow approved, with a session up now.
    // Its key comes from the registry, never from the grant's own say-so.
    if !shared.peers.is_approved(&source) || shared.s2s.link(&source).is_none() {
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
    let Some((pull_id, cancel)) = shared.s2s.reserve(ctx.account_id, per_account) else {
        return Err(ErrorCode::RateLimited);
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
            return Err(ErrorCode::AlreadyExists);
        }
        Err(_) => {
            shared.s2s.release(pull_id);
            return Err(ErrorCode::Internal);
        }
    }
    let source_name = peer_name(shared, &source);
    let files = signed.grant.items.len() as u32;
    audit(
        shared,
        &ctx.login,
        "pull-accept",
        format!(
            "#{pull_id} from={source_name} files={files} bytes={total} into={}/{}",
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
                "#{} files={} bytes={} missing={} reason={reason}",
                self.id, tally.files, tally.bytes, tally.missing
            ),
        );
        self.shared.s2s.release(self.id);
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
        let Some(link) = self.shared.s2s.link(&self.source) else {
            return (tally, Some(pull_reason::SOURCE_UNREACHABLE), landed);
        };
        // Source top-level name → the folder this pull made for it here.
        let mut tops: HashMap<String, String> = HashMap::new();
        let mut last_push = Instant::now();
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
            let staging = self.staging(index);
            let base = tally.bytes;
            // A generous deadline: the idle timeout, plus the file at the
            // slowest rate a source may keep.
            let deadline = STREAM_IDLE + Duration::from_secs(item.size / MIN_RATE);
            let fetched = tokio::time::timeout(
                deadline,
                fetch_item(
                    link.as_ref(),
                    &self.grant_bytes,
                    index as u32,
                    item.size,
                    &staging,
                    &self.cancel,
                    |got| {
                        if last_push.elapsed() >= PROGRESS_EVERY {
                            last_push = Instant::now();
                            let mut t = tally;
                            t.bytes = base + got;
                            self.push(self.status(
                                pull_state::RUNNING,
                                t,
                                pull_reason::NONE,
                                &landed,
                            ));
                        }
                    },
                ),
            )
            .await
            .unwrap_or(Err(FetchError::Unreachable));
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

/// At the source: answer one pull stream a federation peer opened. The
/// grant must be this burrow's own, name this peer as its fetcher, and still
/// stand (with grace for a pull under way); the file must still be the one
/// granted.
pub async fn serve_pull_stream(
    shared: Arc<Shared>,
    peer_key: [u8; 32],
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
        let req: PullStreamRequest =
            postcard::from_bytes(&bytes).map_err(|_| stream_status::BAD)?;
        if !shared.config.read().s2s_grants_enabled {
            return Err(stream_status::OFF);
        }
        let grant = SignedPullGrant::from_bytes(&req.grant).map_err(|_| stream_status::BAD)?;
        let clock = serve_clock(now_unix(), grant.grant.expires_unix);
        grant
            .check(&shared.server_key, &peer_key, clock)
            .map_err(|_| stream_status::DENIED)?;
        if !shared.peers.is_approved(&peer_key) {
            return Err(stream_status::DENIED);
        }
        let item = grant
            .grant
            .items
            .get(req.item as usize)
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
        if req.offset > item.size {
            return Err(stream_status::BAD);
        }
        Ok((item.root, item.size, req.offset))
    }
    .await;
    let (root, size, mut offset) = match answer {
        Ok(v) => v,
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
    while offset < size {
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

#[cfg(test)]
mod tests {
    use super::*;

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
