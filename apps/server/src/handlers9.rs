//! Wave 4.2 handlers: the bulk file-transfer engine (control-frame chunk
//! path, works over both QUIC and WebSocket).
//!
//! A transfer is negotiated on the control stream ([`pt::TransferOpen`] →
//! [`pt::TransferTicket`]); bytes then move as windowed ranged chunks
//! ([`pt::FileChunkRequest`]/[`pt::FileChunk`] for downloads,
//! [`pt::FileChunkPut`] for uploads). Everything is resumable — a download
//! resumes from the client's local offset, an upload from the server's
//! verified staged prefix — and every finished file is checked against its
//! blake3 root (== blob id). Folder transfers pipeline over a single
//! [`pt::FolderManifest`] round trip. On QUIC, [`serve_bulk_stream`] moves
//! the bytes onto a dedicated bi-stream (off the control channel) via the
//! [`rabbithole_net::BulkStreams`] seam; WebSocket uses the ranged-chunk
//! path over the control stream. Same tickets, same verification.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use rabbithole_blobs::BlobId;
use rabbithole_net::{read_framed, BulkRecv, BulkSend, Connection};
use rabbithole_proto::filelib as pf;
use rabbithole_proto::transfer as pt;
use rabbithole_proto::{ErrorCode, Frame};
use rabbithole_server_core::files::KIND_FILE;
use rabbithole_server_core::ratelimit::{class as rl, Scope};
use rabbithole_server_core::{Caps, ServerEvent};

use crate::session::SessionCtx;
use crate::Shared;

/// Max bytes a single chunk carries (well under the 1 MiB control-frame cap).
const CHUNK_MAX: usize = 256 * 1024;

/// A live transfer authorization + progress.
pub struct Ticket {
    pub account_id: i64,
    /// The session that opened this ticket (for teardown cleanup).
    pub session_id: u64,
    pub direction: u8,
    pub root: [u8; 32],
    pub size: u64,
    pub token: [u8; 16],
    // Download:
    pub blob_id: Option<[u8; 32]>,
    pub node_area: String,
    pub node_path: String,
    // Upload:
    pub area: String,
    pub parent: Option<String>,
    pub name: String,
    pub mime: String,
    pub comment: String,
    pub uploader: String,
    pub staging: Option<PathBuf>,
    /// Verified staged bytes (upload) — the resume high-water.
    pub have: u64,
}

/// Registry of live transfer tickets, keyed by transfer id.
pub struct TransferRegistry {
    inner: Mutex<HashMap<u64, Ticket>>,
    next: AtomicU64,
}

impl TransferRegistry {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
        }
    }

    fn next_id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    /// Number of live tickets owned by an account — its in-flight transfer
    /// count, used to enforce the per-account concurrency cap.
    pub fn count_for_account(&self, account_id: i64) -> usize {
        self.inner
            .lock()
            .values()
            .filter(|t| t.account_id == account_id)
            .count()
    }

    /// Drop a finished download ticket. Downloads have no explicit finish
    /// message, so the server retires the ticket when it serves the last
    /// chunk (or when the dedicated stream drains); session teardown is the
    /// backstop for abandoned ones.
    pub fn remove_download(&self, id: u64) {
        self.inner.lock().remove(&id);
    }

    /// Remove every ticket a session opened and return the upload staging
    /// files that need deleting. Called on session teardown so abandoned
    /// transfers leak neither a registry slot (which would count against the
    /// concurrency cap) nor a partial `.part` file.
    pub fn close_session(&self, session_id: u64) -> Vec<PathBuf> {
        let mut staging = Vec::new();
        self.inner.lock().retain(|_, t| {
            if t.session_id == session_id {
                if let Some(p) = t.staging.take() {
                    staging.push(p);
                }
                false
            } else {
                true
            }
        });
        staging
    }
}

impl Default for TransferRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Serve one dedicated bulk stream (QUIC only). The client opens a fresh
/// bi-stream, writes a length-prefixed [`pt::BulkPreamble`] binding it to a
/// ticket, then bytes flow off the control channel: the server streams the
/// requested range (download) or consumes the remainder into staging
/// (upload), acking with one byte so the client knows staging is durable
/// before it sends `UploadFinish`. Errors just drop the stream — the client
/// falls back or retries; the control-plane invariants (ticket, whole-file
/// verification at finish) are unchanged.
pub async fn serve_bulk_stream(
    shared: Arc<Shared>,
    account_id: i64,
    mut send: BulkSend,
    mut recv: BulkRecv,
) {
    use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

    let Ok(bytes) = read_framed(&mut recv, 4096).await else {
        return;
    };
    let Ok(pre) = postcard::from_bytes::<pt::BulkPreamble>(&bytes) else {
        return;
    };
    // Authorize against the ticket (token is unguessable; account must match).
    let info = {
        let map = shared.transfers.inner.lock();
        match map.get(&pre.transfer_id) {
            Some(t)
                if t.account_id == account_id
                    && t.token == pre.token
                    && t.direction == pre.direction =>
            {
                Some((t.direction, t.blob_id, t.size, t.staging.clone()))
            }
            _ => None,
        }
    };
    let Some((direction, blob_id, size, staging)) = info else {
        return;
    };

    match direction {
        pt::DIR_DOWNLOAD => {
            let Some(blob_id) = blob_id else { return };
            let rate = shared.config.read().transfer_rate_bytes_per_sec;
            let mut offset = pre.offset;
            while offset < size {
                let want = ((size - offset).min(CHUNK_MAX as u64)) as usize;
                let blobs = shared.blobs.clone();
                let at = offset;
                let chunk = match tokio::task::spawn_blocking(move || {
                    blobs.read_range(&BlobId(blob_id), at, want)
                })
                .await
                {
                    Ok(Ok(c)) => c,
                    _ => break,
                };
                if chunk.is_empty() {
                    break;
                }
                let n = chunk.len();
                if send.write_all(&chunk).await.is_err() {
                    break;
                }
                offset += n as u64;
                throttle_after(rate, n).await;
            }
            let _ = send.shutdown().await;
            // Download over (or the peer went away): retire the ticket so it
            // stops counting against the account's concurrency cap.
            shared.transfers.remove_download(pre.transfer_id);
        }
        pt::DIR_UPLOAD => {
            let Some(staging) = staging else { return };
            let Ok(mut f) = tokio::fs::OpenOptions::new()
                .write(true)
                .open(&staging)
                .await
            else {
                return;
            };
            if f.seek(std::io::SeekFrom::Start(pre.offset)).await.is_err() {
                return;
            }
            let mut offset = pre.offset;
            let mut buf = vec![0u8; CHUNK_MAX];
            loop {
                match recv.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if f.write_all(&buf[..n]).await.is_err() {
                            return;
                        }
                        offset += n as u64;
                    }
                    Err(_) => return,
                }
            }
            let _ = f.flush().await;
            {
                let mut map = shared.transfers.inner.lock();
                if let Some(t) = map.get_mut(&pre.transfer_id) {
                    t.have = t.have.max(offset);
                }
            }
            // One ack byte: staging is fully written and durable.
            let _ = send.write_all(&[1u8]).await;
            let _ = send.shutdown().await;
        }
        _ => {}
    }
}

/// Bandwidth cap: sleep long enough after emitting `bytes` to hold the
/// average send rate at or under `rate` bytes/sec. `rate == 0` disables it.
/// A per-chunk sleep slightly under-utilizes the link (it ignores wire time),
/// which is the safe direction for a cap.
async fn throttle_after(rate: u64, bytes: usize) {
    if rate > 0 && bytes > 0 {
        let secs = bytes as f64 / rate as f64;
        tokio::time::sleep(std::time::Duration::from_secs_f64(secs)).await;
    }
}

/// An unguessable token binding a transfer to its authorization: keyed by the
/// server's secret signing seed so clients can't forge one for another id.
fn mint_token(seed: &[u8; 32], transfer_id: u64) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(seed);
    hasher.update(b"transfer-token");
    hasher.update(&transfer_id.to_le_bytes());
    let mut token = [0u8; 16];
    token.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    token
}

fn resource(area: &str, path: Option<&str>) -> String {
    match path {
        Some(p) if !p.is_empty() => format!("files/{area}/{p}"),
        _ => format!("files/{area}"),
    }
}

fn staging_path(shared: &Shared, id: u64) -> PathBuf {
    shared
        .config
        .read()
        .data_dir
        .join("transfers")
        .join(format!("{id}.part"))
}

pub async fn handle(
    conn: &mut Box<dyn Connection>,
    frame: &Frame,
    shared: &Arc<Shared>,
    ctx: &mut SessionCtx,
) -> anyhow::Result<bool> {
    macro_rules! reply {
        ($msg:expr) => {
            conn.send(Frame::reply_to(frame, $msg)?).await?
        };
    }
    macro_rules! fail {
        ($code:expr) => {{
            conn.send(Frame::error_reply(frame, $code)).await?;
            return Ok(true);
        }};
    }

    // ---- Open a transfer -------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<pt::TransferOpen>() {
        // Per-account concurrency cap (0 = unlimited). Enforced before the
        // ticket is minted so a flood is refused rather than admitted.
        // Checked before the rate budget so a client politely polling for a
        // free slot does not drain its transfer-open tokens.
        let cap = shared.config.read().max_concurrent_transfers;
        if cap > 0 && shared.transfers.count_for_account(ctx.account_id) >= cap as usize {
            fail!(ErrorCode::RateLimited);
        }
        // Per-account transfer-open rate budget (Wave 13), consumed before
        // any work is done for the request.
        if !shared.rate_allow(Scope::Account(ctx.account_id), rl::TRANSFER) {
            fail!(ErrorCode::RateLimited);
        }
        match req.direction {
            pt::DIR_DOWNLOAD => {
                let Some(node_id) = req.node_id else {
                    fail!(ErrorCode::BadRequest)
                };
                let Some(node) = shared.files.node(node_id).await.ok().flatten() else {
                    fail!(ErrorCode::NotFound)
                };
                let target = match shared.files.resolve(node.id).await {
                    Ok(t) => t,
                    Err(_) => fail!(ErrorCode::NotFound),
                };
                if target.kind != KIND_FILE {
                    fail!(ErrorCode::BadRequest);
                }
                let res = resource(&target.area, Some(&target.path));
                if !ctx.allows(shared, &res, Caps::FILE_DOWNLOAD) {
                    fail!(ErrorCode::Forbidden);
                }
                if shared.files.in_dropbox(&target).await.unwrap_or(false)
                    && !ctx.allows(shared, &res, Caps::DROPBOX_VIEW)
                    && !ctx.allows(shared, &resource(&target.area, None), Caps::FILE_MANAGE)
                {
                    fail!(ErrorCode::Forbidden);
                }
                let Some(blob_id) = target.blob_id else {
                    fail!(ErrorCode::NotFound)
                };
                // Count the download once, at authorization.
                let _ = shared.files.record_download(node.id).await;
                let id = shared.transfers.next_id();
                let token = mint_token(&shared.server_signing_seed, id);
                let size = target.size.max(0) as u64;
                shared.transfers.inner.lock().insert(
                    id,
                    Ticket {
                        account_id: ctx.account_id,
                        session_id: ctx.session_id,
                        direction: pt::DIR_DOWNLOAD,
                        root: blob_id,
                        size,
                        token,
                        blob_id: Some(blob_id),
                        node_area: target.area.clone(),
                        node_path: target.path.clone(),
                        area: String::new(),
                        parent: None,
                        name: String::new(),
                        mime: String::new(),
                        comment: String::new(),
                        uploader: String::new(),
                        staging: None,
                        have: size,
                    },
                );
                reply!(&pt::TransferTicket::new(id, blob_id, size, token));
                return Ok(true);
            }
            pt::DIR_UPLOAD => {
                if ctx.is_guest {
                    fail!(ErrorCode::Forbidden);
                }
                let res = resource(&req.area, req.parent.as_deref());
                if !ctx.allows(shared, &res, Caps::FILE_UPLOAD) {
                    fail!(ErrorCode::Forbidden);
                }
                if req.name.trim().is_empty() || req.name.contains('/') {
                    fail!(ErrorCode::BadRequest);
                }
                // Per-account storage quota (0 = unlimited).
                let quota = shared.config.read().upload_quota_bytes;
                if quota > 0 {
                    let used = shared
                        .files
                        .uploaded_bytes(ctx.account_id)
                        .await
                        .unwrap_or(0)
                        .max(0) as u64;
                    if used.saturating_add(req.size) > quota {
                        fail!(ErrorCode::TooLarge);
                    }
                }
                let id = shared.transfers.next_id();
                let token = mint_token(&shared.server_signing_seed, id);
                let staging = staging_path(shared, id);
                if let Some(dir) = staging.parent() {
                    let _ = tokio::fs::create_dir_all(dir).await;
                }
                let _ = tokio::fs::File::create(&staging).await;
                let uploader = format!("{}@{}", ctx.screen_name, shared.origin_name());
                shared.transfers.inner.lock().insert(
                    id,
                    Ticket {
                        account_id: ctx.account_id,
                        session_id: ctx.session_id,
                        direction: pt::DIR_UPLOAD,
                        root: req.root,
                        size: req.size,
                        token,
                        blob_id: None,
                        node_area: String::new(),
                        node_path: String::new(),
                        area: req.area.clone(),
                        parent: req.parent.clone(),
                        name: req.name.clone(),
                        mime: req.mime.clone(),
                        comment: req.comment.clone(),
                        uploader,
                        staging: Some(staging),
                        have: 0,
                    },
                );
                reply!(&pt::TransferTicket::new(id, req.root, req.size, token).with_server_have(0));
                return Ok(true);
            }
            _ => fail!(ErrorCode::BadRequest),
        }
    }

    // ---- Resume ----------------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<pt::TransferResume>() {
        // Compute under the lock, drop the guard, THEN await (the guard is
        // !Send and must not cross an await point).
        let outcome: Result<([u8; 32], u64, u64), ErrorCode> = {
            let map = shared.transfers.inner.lock();
            match map.get(&req.transfer_id) {
                None => Err(ErrorCode::NotFound),
                Some(t) if t.account_id != ctx.account_id || t.token != req.token => {
                    Err(ErrorCode::Forbidden)
                }
                Some(t) => Ok((t.root, t.size, t.have)),
            }
        };
        let (root, size, have) = match outcome {
            Ok(v) => v,
            Err(code) => fail!(code),
        };
        reply!(
            &pt::TransferTicket::new(req.transfer_id, root, size, req.token).with_server_have(have)
        );
        return Ok(true);
    }

    // ---- Download a chunk ------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<pt::FileChunkRequest>() {
        let outcome: Result<([u8; 32], u64), ErrorCode> = {
            let map = shared.transfers.inner.lock();
            match map.get(&req.transfer_id) {
                None => Err(ErrorCode::NotFound),
                Some(t) if t.account_id != ctx.account_id || t.direction != pt::DIR_DOWNLOAD => {
                    Err(ErrorCode::Forbidden)
                }
                Some(t) => match t.blob_id {
                    Some(b) => Ok((b, t.size)),
                    None => Err(ErrorCode::NotFound),
                },
            }
        };
        let (blob_id, size) = match outcome {
            Ok(v) => v,
            Err(code) => fail!(code),
        };
        let len = (req.len as usize).min(CHUNK_MAX);
        let blobs = shared.blobs.clone();
        let offset = req.offset;
        let bytes = match tokio::task::spawn_blocking(move || {
            blobs.read_range(&BlobId(blob_id), offset, len)
        })
        .await?
        {
            Ok(b) => b,
            Err(_) => fail!(ErrorCode::NotFound),
        };
        let n = bytes.len();
        let last = req.offset + n as u64 >= size;
        reply!(&pt::FileChunk::new(
            req.transfer_id,
            req.offset,
            last,
            bytes
        ));
        let rate = shared.config.read().transfer_rate_bytes_per_sec;
        throttle_after(rate, n).await;
        if last {
            // Whole file served: retire the (single-use) download ticket.
            shared.transfers.remove_download(req.transfer_id);
        }
        return Ok(true);
    }

    // ---- Upload a chunk --------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<pt::FileChunkPut>() {
        let outcome: Result<PathBuf, ErrorCode> = {
            let map = shared.transfers.inner.lock();
            match map.get(&req.transfer_id) {
                None => Err(ErrorCode::NotFound),
                Some(t) if t.account_id != ctx.account_id || t.direction != pt::DIR_UPLOAD => {
                    Err(ErrorCode::Forbidden)
                }
                Some(t) => match &t.staging {
                    Some(p) => Ok(p.clone()),
                    None => Err(ErrorCode::Internal),
                },
            }
        };
        let staging = match outcome {
            Ok(v) => v,
            Err(code) => fail!(code),
        };
        // Position-addressed write: seek to the chunk's declared offset rather
        // than blind-append. This makes chunk delivery idempotent and order-
        // independent — a re-sent or reordered chunk lands in the right place
        // instead of corrupting the stage. The finished file is verified whole
        // against the declared blake3 root at UploadFinish, which is the real
        // integrity gate (a lost chunk leaves a hole and fails that check).
        use tokio::io::{AsyncSeekExt, AsyncWriteExt};
        let mut f = match tokio::fs::OpenOptions::new()
            .write(true)
            .open(&staging)
            .await
        {
            Ok(f) => f,
            Err(_) => fail!(ErrorCode::Internal),
        };
        // Flush before acking: `tokio::fs` only *queues* the write, so without
        // this the sync `put_verified` read at UploadFinish can race an
        // unflushed chunk and see a short/holey stage → spurious hash-mismatch
        // (BadRequest). The dedicated-stream path already flushes; the chunk
        // path must too.
        if f.seek(std::io::SeekFrom::Start(req.offset)).await.is_err()
            || f.write_all(&req.bytes).await.is_err()
            || f.flush().await.is_err()
        {
            fail!(ErrorCode::Internal);
        }
        {
            let mut map = shared.transfers.inner.lock();
            if let Some(t) = map.get_mut(&req.transfer_id) {
                t.have = t.have.max(req.offset + req.bytes.len() as u64);
            }
        }
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    // ---- Finish an upload ------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<pt::UploadFinish>() {
        let outcome: Result<Ticket, ErrorCode> = {
            let mut map = shared.transfers.inner.lock();
            match map.get(&req.transfer_id) {
                None => Err(ErrorCode::NotFound),
                Some(t) if t.account_id != ctx.account_id || t.direction != pt::DIR_UPLOAD => {
                    Err(ErrorCode::Forbidden)
                }
                Some(_) => Ok(map.remove(&req.transfer_id).expect("just checked")),
            }
        };
        let ticket = match outcome {
            Ok(t) => t,
            Err(code) => fail!(code),
        };
        let staging = ticket.staging.clone().unwrap_or_default();
        let blobs = shared.blobs.clone();
        let root = ticket.root;
        let staged = staging.clone();
        let committed =
            tokio::task::spawn_blocking(move || blobs.put_verified(&staged, &BlobId(root))).await?;
        if committed.is_err() {
            let _ = tokio::fs::remove_file(&staging).await;
            fail!(ErrorCode::BadRequest); // hash mismatch or io error
        }
        let node = match shared
            .files
            .add_file(
                &ticket.area,
                ticket.parent.as_deref(),
                &ticket.name,
                &ticket.root,
                ticket.size as i64,
                &ticket.mime,
                "",
                &ticket.comment,
                &ticket.uploader,
                ctx.account_id,
            )
            .await
        {
            Ok(n) => n,
            Err(_) => fail!(ErrorCode::AlreadyExists),
        };
        shared.bus.publish(ServerEvent::FileAdded {
            area: ticket.area.clone(),
            id: node.id,
        });
        reply!(&pf::NodeReply::new(crate::handlers8::view(&node)));
        return Ok(true);
    }

    // ---- Abort -----------------------------------------------------------
    if let Some(Ok(req)) = frame.decode::<pt::TransferAbort>() {
        let staging = shared
            .transfers
            .inner
            .lock()
            .remove(&req.transfer_id)
            .and_then(|t| t.staging);
        if let Some(p) = staging {
            let _ = tokio::fs::remove_file(p).await;
        }
        conn.send(Frame::ack(frame)).await?;
        return Ok(true);
    }

    // ---- Folder manifest (pipelined transfers) ---------------------------
    if let Some(Ok(req)) = frame.decode::<pt::FolderManifestRequest>() {
        if !ctx.allows(
            shared,
            &resource(&req.area, req.path.as_deref()),
            Caps::FILE_LIST,
        ) {
            fail!(ErrorCode::Forbidden);
        }
        let files = match shared.files.manifest(&req.area, req.path.as_deref()).await {
            Ok(f) => f,
            Err(_) => fail!(ErrorCode::NotFound),
        };
        let entries = files
            .iter()
            .filter_map(|(n, rel)| {
                n.blob_id.map(|b| {
                    pt::ManifestEntry::new(n.id, rel.clone(), b, n.size.max(0) as u64)
                        .with_mime(n.mime.clone())
                })
            })
            .collect();
        reply!(&pt::FolderManifest::new(entries));
        return Ok(true);
    }

    Ok(false)
}
