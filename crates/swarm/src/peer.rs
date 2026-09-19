//! The peer wire (Wave 5.3b): direct peer-to-peer file serving with
//! per-chunk Bao verification.
//!
//! A sharing peer runs a [`PeerServer`] — a QUIC endpoint (the same
//! `rabbithole-net` stack the client already speaks, fingerprint-pinned
//! self-signed TLS) that serves byte ranges of files it seeds. Every
//! request carries a server-signed [`CapToken`](crate::cap::CapToken) (or,
//! for a burrow a file was sent to, an
//! [`S2sCapToken`](crate::cap::S2sCapToken)); every response is a Bao
//! stream, so the fetcher verifies each 16 KiB block against the file's
//! blake3 root *as it arrives* — an untrusted peer can waste a fetcher's
//! time, but never feed it a wrong byte.
//!
//! One bi-stream per request: the fetcher writes a framed [`PeerRequest`]
//! and closes its side; the peer answers with a framed
//! [`PeerResponseHeader`] followed by the raw Bao stream for the requested
//! (chunk-aligned) ranges. Requests are capped at [`PEER_REQUEST_MAX`]
//! bytes so both sides can buffer whole responses; multi-range fetches
//! loop ([`fetch_file`]).
//!
//! A peer need not hold a whole file. A fetcher keeps the proof (the Bao
//! parent hashes) of every range it verifies, so what it has fetched it can
//! serve on, proof and all, while the rest is still coming: a **partial
//! seed**. A stream that opens with an empty frame asks a question instead
//! ([`PeerAsk`]): which units the peer holds ([`HaveMap`]). A peer asked for
//! a range it does not hold answers [`STATUS_NOT_HELD`]. Whoever serves a
//! range, whole seed or partial, every block is checked against the root by
//! the server before it goes and by the fetcher before it lands.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use bao_tree::io::outboard::{PreOrderMemOutboard, PreOrderOutboard};
use bao_tree::io::sync::{decode_ranges, encode_ranges_validated, Outboard, OutboardMut};
use bao_tree::io::{round_up_to_chunks, round_up_to_chunks_groups};
use bao_tree::{blake3 as bao_blake3, BaoTree, BlockSize, ChunkRanges, TreeNode};
use rabbithole_net::quic::{QuicListener, QuicTransport};
use rabbithole_net::tls::{CertFingerprint, ServerAuth, TlsIdentity};
use rabbithole_net::{
    read_framed, write_framed, BulkRecv, BulkSend, Listener, NetError, Transport,
};
use range_collections::RangeSet2;
use serde::{Deserialize, Serialize};

#[cfg(test)]
use crate::cap::CapToken;

/// Bao block size on the peer wire: 16 KiB chunk groups (log2(16) = 4).
/// The root hash is the plain blake3 of the file regardless, so peer-wire
/// roots are the same ids the blob store and adverts use.
pub const PEER_BLOCK: BlockSize = BlockSize::from_chunk_log(4);
/// Bytes covered by one Bao block at [`PEER_BLOCK`].
pub const PEER_BLOCK_BYTES: u64 = 16 * 1024;
/// Most bytes one [`PeerRequest`] may ask for (whole files loop ranges).
pub const PEER_REQUEST_MAX: u64 = 4 * 1024 * 1024;
/// How long to wait for a peer connection before giving up on it. Dead
/// sources fail fast so the multi-source scheduler can route around them.
pub const PEER_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// How long to wait for a peer's response header or body before giving up. A
/// peer that connects and then stalls mid-stream must not park a worker forever
/// (the module promises "a stalled peer can't hold the tail hostage") — on
/// timeout the fetch errors and the scheduler retires the source. Generous
/// enough for a 4 MiB unit over a slow-but-live link.
pub const PEER_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Response status codes.
pub const STATUS_OK: u8 = 0;
pub const STATUS_DENIED: u8 = 1;
pub const STATUS_NOT_FOUND: u8 = 2;
pub const STATUS_BAD_REQUEST: u8 = 3;
/// The peer holds part of the file, but not that range (yet): try it
/// elsewhere, and this peer again later.
pub const STATUS_NOT_HELD: u8 = 4;

/// The unit a [`HaveMap`] counts in, and the scheduler fetches in.
pub const HAVE_UNIT: u64 = 1024 * 1024;
const _: () = assert!(HAVE_UNIT % PEER_BLOCK_BYTES == 0);

/// Largest [`HaveMap`] frame a fetcher reads: a bit per unit, for files up
/// to 8 TiB.
const HAVE_MAP_MAX: usize = 1 << 20;

/// A question on the peer wire other than a range, sent in the frame after
/// an empty one. A peer from before these reads the empty frame as a
/// malformed request and closes the stream: it holds whole files only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PeerAsk {
    /// Which units of `root` the peer holds, under the same capability a
    /// range request carries.
    Have { token: Vec<u8>, root: [u8; 32] },
}

/// Which units of a file a peer holds: bit `i` (least significant first in
/// each byte) is the `unit` bytes from `i * unit`. Sent after an OK header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HaveMap {
    pub unit: u64,
    pub bits: Vec<u8>,
}

impl HaveMap {
    /// Every unit of a `size`-byte file.
    pub fn whole(size: u64) -> Self {
        let units = size.div_ceil(HAVE_UNIT).max(1) as usize;
        let mut bits = vec![0xFF; units.div_ceil(8)];
        if units % 8 != 0 {
            *bits.last_mut().expect("at least one byte") = (1u8 << (units % 8)) - 1;
        }
        HaveMap {
            unit: HAVE_UNIT,
            bits,
        }
    }

    /// Whether every unit touching `[offset, offset + len)` is held.
    pub fn covers(&self, offset: u64, len: u64) -> bool {
        if self.unit == 0 || len == 0 {
            return false;
        }
        let first = offset / self.unit;
        let last = (offset + len - 1) / self.unit;
        (first..=last).all(|i| bit(&self.bits, i))
    }

    /// Forget unit `index` (the peer said it does not hold it after all).
    pub fn clear(&mut self, index: u64) {
        clear_bit(&mut self.bits, index);
    }
}

fn bit(bits: &[u8], i: u64) -> bool {
    bits.get((i / 8) as usize)
        .is_some_and(|b| b & (1 << (i % 8)) != 0)
}

fn set_bit(bits: &mut Vec<u8>, i: u64) {
    let byte = (i / 8) as usize;
    if bits.len() <= byte {
        bits.resize(byte + 1, 0);
    }
    bits[byte] |= 1 << (i % 8);
}

fn clear_bit(bits: &mut [u8], i: u64) {
    if let Some(b) = bits.get_mut((i / 8) as usize) {
        *b &= !(1 << (i % 8));
    }
}
/// One framed request on a fresh bi-stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerRequest {
    /// Server-signed capability (postcard `CapToken`) for `root`.
    pub token: Vec<u8>,
    pub root: [u8; 32],
    pub offset: u64,
    /// Requested byte count (1..=[`PEER_REQUEST_MAX`]); the peer clamps to
    /// the file's end.
    pub len: u64,
}

/// The framed reply header; a Bao stream follows when `status == OK`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PeerResponseHeader {
    pub status: u8,
    /// Total file size — the fetcher derives the Bao tree from it (a lie
    /// here cannot survive verification against the root).
    pub size: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    #[error("net: {0}")]
    Net(#[from] NetError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("peer refused: status {0}")]
    Refused(u8),
    #[error("verification failed: {0}")]
    Verify(String),
    #[error("seeded file does not hash to the declared root")]
    RootMismatch,
    #[error("request is malformed or too large")]
    BadRequest,
}

/// What a peer seeds: root → a whole file with its outboard (the Bao
/// parent-hash tree, ~64 bytes per 16 KiB, held in memory), or part of a
/// file still being fetched with the proofs kept so far (on disk, beside
/// it) and which units it holds.
#[derive(Default)]
pub struct SeedStore {
    inner: RwLock<HashMap<[u8; 32], Seed>>,
}

#[derive(Clone)]
enum Seed {
    Whole {
        path: PathBuf,
        size: u64,
        outboard: Arc<PreOrderMemOutboard>,
    },
    Partial {
        path: PathBuf,
        size: u64,
        proofs: PathBuf,
        held: Arc<RwLock<Vec<u8>>>,
    },
}

impl Seed {
    fn size(&self) -> u64 {
        match self {
            Seed::Whole { size, .. } | Seed::Partial { size, .. } => *size,
        }
    }
}

/// A fetch's hold on its partial seed: it marks units as they land, and
/// the store serves them.
#[derive(Clone)]
pub struct Sharing {
    held: Arc<RwLock<Vec<u8>>>,
}

impl Sharing {
    /// Unit `index` has landed, and its proof is kept: serve it.
    pub fn mark(&self, index: u64) {
        set_bit(&mut self.held.write().expect("not poisoned"), index);
    }
}

impl SeedStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hash `path` (streamed, never all in memory), check it is `root`, and
    /// start seeding it whole.
    pub fn add(&self, root: [u8; 32], path: &Path) -> Result<(), PeerError> {
        let file = std::fs::File::open(path)?;
        let size = file.metadata()?.len();
        let tree = BaoTree::new(size, PEER_BLOCK);
        let mut outboard = PreOrderMemOutboard {
            root: bao_blake3::Hash::from([0; 32]),
            tree,
            data: vec![0u8; tree.outboard_size() as usize],
        };
        let hashed = bao_tree::io::sync::outboard(
            std::io::BufReader::with_capacity(256 * 1024, file),
            tree,
            &mut outboard,
        )?;
        if *hashed.as_bytes() != root {
            return Err(PeerError::RootMismatch);
        }
        outboard.root = hashed;
        self.insert_whole(root, path, size, outboard);
        Ok(())
    }

    /// Seed a whole file whose proofs were kept while it was fetched (the
    /// fetch has checked the file against `root`): the proofs are read into
    /// memory and checked complete against the root, without hashing the
    /// file again.
    pub fn add_proved(
        &self,
        root: [u8; 32],
        size: u64,
        path: &Path,
        proofs: &Path,
    ) -> Result<(), PeerError> {
        let tree = BaoTree::new(size, PEER_BLOCK);
        let mut data = std::fs::read(proofs)?;
        data.resize(tree.outboard_size() as usize, 0);
        let outboard = PreOrderMemOutboard {
            root: bao_blake3::Hash::from(root),
            tree,
            data,
        };
        if !proofs_complete(&outboard, size)? {
            return Err(PeerError::RootMismatch);
        }
        self.insert_whole(root, path, size, outboard);
        Ok(())
    }

    fn insert_whole(&self, root: [u8; 32], path: &Path, size: u64, outboard: PreOrderMemOutboard) {
        self.inner.write().expect("not poisoned").insert(
            root,
            Seed::Whole {
                path: path.to_path_buf(),
                size,
                outboard: Arc::new(outboard),
            },
        );
    }

    /// Seed the part of `path` fetched so far: `held` units, whose proofs
    /// are in the pre-order outboard file `proofs`. The fetch marks more
    /// through the returned [`Sharing`]. `None` when the whole file is
    /// already seeded here.
    pub fn add_partial(
        &self,
        root: [u8; 32],
        size: u64,
        path: &Path,
        proofs: &Path,
        held: impl IntoIterator<Item = u64>,
    ) -> Option<Sharing> {
        let mut inner = self.inner.write().expect("not poisoned");
        if matches!(inner.get(&root), Some(Seed::Whole { .. })) {
            return None;
        }
        let mut bits = Vec::new();
        for index in held {
            set_bit(&mut bits, index);
        }
        let held = Arc::new(RwLock::new(bits));
        inner.insert(
            root,
            Seed::Partial {
                path: path.to_path_buf(),
                size,
                proofs: proofs.to_path_buf(),
                held: held.clone(),
            },
        );
        Some(Sharing { held })
    }

    /// Stop seeding a partial file (a fetch that failed or was dropped). A
    /// whole seed of the same root is left as it is.
    pub fn remove_partial(&self, root: &[u8; 32]) {
        let mut inner = self.inner.write().expect("not poisoned");
        if matches!(inner.get(root), Some(Seed::Partial { .. })) {
            inner.remove(root);
        }
    }

    pub fn remove(&self, root: &[u8; 32]) {
        self.inner.write().expect("not poisoned").remove(root);
    }

    fn get(&self, root: &[u8; 32]) -> Option<Seed> {
        self.inner.read().expect("not poisoned").get(root).cloned()
    }

    /// Whether this store seeds `root` whole.
    pub fn holds_whole(&self, root: &[u8; 32]) -> bool {
        matches!(self.get(root), Some(Seed::Whole { .. }))
    }

    /// Whether this store seeds `root` whole, from `path`.
    pub fn holds_whole_at(&self, root: &[u8; 32], at: &Path) -> bool {
        matches!(self.get(root), Some(Seed::Whole { path, .. }) if path == at)
    }

    /// Stop seeding a whole file that can no longer be served (its file is
    /// gone or changed), if it is still the seed `outboard` belongs to.
    fn drop_whole(&self, root: &[u8; 32], outboard: &Arc<PreOrderMemOutboard>) {
        let mut inner = self.inner.write().expect("not poisoned");
        if matches!(inner.get(root), Some(Seed::Whole { outboard: o, .. }) if Arc::ptr_eq(o, outboard))
        {
            inner.remove(root);
        }
    }

    /// Which units of `root` this store holds, if it seeds it at all.
    pub fn have(&self, root: &[u8; 32]) -> Option<HaveMap> {
        Some(match self.get(root)? {
            Seed::Whole { size, .. } => HaveMap::whole(size),
            Seed::Partial { held, .. } => HaveMap {
                unit: HAVE_UNIT,
                bits: held.read().expect("not poisoned").clone(),
            },
        })
    }
}

/// Whether an outboard holds the proof of every block of a `size`-byte
/// file, each checked against its root.
fn proofs_complete(outboard: impl Outboard, size: u64) -> std::io::Result<bool> {
    let all = ChunkRanges::from(bao_tree::ChunkNum(0)..BaoTree::new(size, PEER_BLOCK).chunks());
    let mut proven = ChunkRanges::empty();
    for range in bao_tree::io::sync::valid_outboard_ranges(outboard, &all) {
        proven |= ChunkRanges::from(range?);
    }
    Ok(proven.is_superset(&all))
}

/// The byte ranges of a `size`-byte file whose proofs `outboard` holds,
/// checked against its root: what a resumed fetch may serve again.
pub fn proven_ranges(outboard: impl Outboard, size: u64) -> std::io::Result<Vec<(u64, u64)>> {
    let all = ChunkRanges::from(bao_tree::ChunkNum(0)..BaoTree::new(size, PEER_BLOCK).chunks());
    let mut out: Vec<(u64, u64)> = Vec::new();
    for range in bao_tree::io::sync::valid_outboard_ranges(outboard, &all) {
        let range = range?;
        let (start, end) = (range.start.to_bytes(), range.end.to_bytes().min(size));
        match out.last_mut() {
            Some(last) if last.1 >= start => last.1 = last.1.max(end),
            _ => out.push((start, end)),
        }
    }
    Ok(out)
}

/// The pre-order outboard file a fetch keeps its proofs in, beside `dest`.
pub fn proofs_path(dest: &Path) -> PathBuf {
    let mut os = dest.as_os_str().to_owned();
    os.push(".obao");
    PathBuf::from(os)
}

/// Open (or start) the proofs file for a `size`-byte file with `root`,
/// sized to hold every proof.
pub fn open_proofs(
    path: &Path,
    root: [u8; 32],
    size: u64,
) -> std::io::Result<PreOrderOutboard<std::fs::File>> {
    let tree = BaoTree::new(size, PEER_BLOCK);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    if file.metadata()?.len() != tree.outboard_size() {
        file.set_len(tree.outboard_size())?;
    }
    Ok(PreOrderOutboard {
        root: bao_blake3::Hash::from(root),
        tree,
        data: file,
    })
}

/// A range's verified bytes and the proof that came with them: the parent
/// hash pairs on the path from the root to each of its blocks.
pub struct Proved {
    pub bytes: Vec<u8>,
    pub parents: Vec<(TreeNode, (bao_blake3::Hash, bao_blake3::Hash))>,
}

impl Proved {
    /// Keep the proof in `outboard` (nodes it does not store are skipped).
    pub fn keep(&self, outboard: &mut impl OutboardMut) -> std::io::Result<()> {
        for (node, pair) in &self.parents {
            outboard.save(*node, pair)?;
        }
        Ok(())
    }
}

/// An outboard that stores nothing and records every parent pair the
/// decoder verified, so a fetch can keep the proof of what it fetched.
struct Recorder {
    tree: BaoTree,
    root: bao_blake3::Hash,
    parents: Vec<(TreeNode, (bao_blake3::Hash, bao_blake3::Hash))>,
}

impl Outboard for Recorder {
    fn root(&self) -> bao_blake3::Hash {
        self.root
    }

    fn tree(&self) -> BaoTree {
        self.tree
    }

    fn load(
        &self,
        _node: TreeNode,
    ) -> std::io::Result<Option<(bao_blake3::Hash, bao_blake3::Hash)>> {
        Ok(None)
    }
}

impl OutboardMut for Recorder {
    fn save(
        &mut self,
        node: TreeNode,
        pair: &(bao_blake3::Hash, bao_blake3::Hash),
    ) -> std::io::Result<()> {
        self.parents.push((node, *pair));
        Ok(())
    }

    fn sync(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A running peer-wire endpoint.
pub struct PeerServer {
    pub addr: std::net::SocketAddr,
    pub fingerprint: CertFingerprint,
    task: tokio::task::JoinHandle<()>,
}

impl PeerServer {
    /// Bind a QUIC endpoint with a fresh self-signed identity and serve
    /// `seeds` to fetchers presenting capabilities signed by `server_key`.
    pub async fn start(
        bind: std::net::SocketAddr,
        server_key: [u8; 32],
        seeds: Arc<SeedStore>,
    ) -> Result<PeerServer, PeerError> {
        let tls = TlsIdentity::self_signed(&["peer".into()])?;
        let fingerprint = tls.fingerprint();
        let mut listener = QuicListener::bind(bind, &tls)?;
        let addr = listener.local_addr()?;
        let task = tokio::spawn(async move {
            while let Ok(conn) = listener.accept().await {
                let seeds = seeds.clone();
                tokio::spawn(async move {
                    // Peer-wire connections only carry bulk streams; keep
                    // the connection handle alive while we serve them.
                    let Some(bulk) = conn.bulk() else { return };
                    let _conn = conn;
                    while let Ok((send, recv)) = bulk.accept().await {
                        let seeds = seeds.clone();
                        tokio::spawn(serve_stream(send, recv, server_key, seeds));
                    }
                });
            }
        });
        Ok(PeerServer {
            addr,
            fingerprint,
            task,
        })
    }

    pub fn stop(&self) {
        self.task.abort();
    }
}

impl Drop for PeerServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The chunk ranges both sides use for a byte range: rounded up to whole
/// [`PEER_BLOCK`] chunk groups, matching the outboard's geometry (finer
/// ranges would make the stream descend below the stored parent nodes).
/// Encoder and verifier MUST compute ranges identically — this is that
/// single definition.
fn block_ranges(offset: u64, len: u64) -> ChunkRanges {
    let byte_ranges: RangeSet2<u64> = RangeSet2::from(offset..offset + len);
    round_up_to_chunks_groups(round_up_to_chunks(&byte_ranges), PEER_BLOCK)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Serve one request stream. Any error just drops the stream — the
/// control-plane invariant (the fetcher verifies everything against the
/// root) makes silent failure safe.
async fn serve_stream(
    mut send: BulkSend,
    mut recv: BulkRecv,
    server_key: [u8; 32],
    seeds: Arc<SeedStore>,
) {
    use tokio::io::AsyncWriteExt;

    let Ok(bytes) = read_framed(&mut recv, 8192).await else {
        return;
    };
    if bytes.is_empty() {
        answer_ask(send, recv, server_key, seeds).await;
        return;
    }
    let Ok(req) = postcard::from_bytes::<PeerRequest>(&bytes) else {
        return;
    };

    // Authorize: a valid, unexpired capability for this exact root, made
    // for a person or for another burrow a file was sent to.
    let authorized = crate::cap::token_allows(&req.token, &server_key, &req.root, now_unix());
    if !authorized {
        refuse(&mut send, STATUS_DENIED, 0).await;
        return;
    }
    let Some(seed) = seeds.get(&req.root) else {
        refuse(&mut send, STATUS_NOT_FOUND, 0).await;
        return;
    };
    let size = seed.size();
    if req.len == 0 || req.len > PEER_REQUEST_MAX || req.offset >= size {
        refuse(&mut send, STATUS_BAD_REQUEST, size).await;
        return;
    }
    let len = req.len.min(size - req.offset);
    let (offset, root) = (req.offset, req.root);

    // Encode the chunk-aligned ranges, validated against the outboard: a
    // block that does not match its proof (a file changed on disk, a gap in
    // a partial file) fails here, and nothing of it is sent. What fails is
    // no longer claimed: a whole seed whose file is gone or changed is
    // dropped, a partial seed's units stop being offered.
    let store = seeds.clone();
    let encoded = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, PeerError> {
        let ranges = block_ranges(offset, len);
        let mut out = Vec::new();
        match &seed {
            Seed::Whole { path, outboard, .. } => {
                let served = std::fs::File::open(path)
                    .map_err(Unserved::from)
                    .and_then(|file| {
                        encode_ranges_validated(&file, outboard.as_ref(), &ranges, &mut out)
                            .map_err(Unserved::from)
                    });
                match served {
                    Ok(()) => {}
                    // The file is gone or is no longer the one seeded.
                    Err(Unserved::Changed) => {
                        store.drop_whole(&root, outboard);
                        return Err(PeerError::Refused(STATUS_NOT_FOUND));
                    }
                    // A passing failure (too many open files, a volume
                    // that dozed off): not this time, and nothing more.
                    Err(Unserved::Passing) => return Err(PeerError::Refused(STATUS_NOT_HELD)),
                }
            }
            Seed::Partial {
                path, proofs, held, ..
            } => {
                let covered = {
                    let held = held.read().expect("not poisoned");
                    HaveMap {
                        unit: HAVE_UNIT,
                        bits: held.clone(),
                    }
                    .covers(offset, len)
                };
                if !covered {
                    return Err(PeerError::Refused(STATUS_NOT_HELD));
                }
                let served = (|| -> Result<(), Unserved> {
                    let file = std::fs::File::open(path)?;
                    let outboard = PreOrderOutboard {
                        root: bao_blake3::Hash::from(root),
                        tree: BaoTree::new(size, PEER_BLOCK),
                        data: std::fs::File::open(proofs)?,
                    };
                    encode_ranges_validated(&file, &outboard, &ranges, &mut out)?;
                    Ok(())
                })();
                if served == Err(Unserved::Changed) {
                    // What was marked held does not prove out, or its file
                    // is gone: stop offering it.
                    let mut held = held.write().expect("not poisoned");
                    for i in offset / HAVE_UNIT..=(offset + len - 1) / HAVE_UNIT {
                        clear_bit(&mut held, i);
                    }
                }
                if served.is_err() {
                    return Err(PeerError::Refused(STATUS_NOT_HELD));
                }
            }
        }
        Ok(out)
    })
    .await;

    match encoded {
        Ok(Ok(stream)) => {
            let h = header(STATUS_OK, size);
            if write_framed(&mut send, &h).await.is_err() {
                return;
            }
            let _ = send.write_all(&stream).await;
            let _ = send.shutdown().await;
        }
        Ok(Err(PeerError::Refused(status))) => refuse(&mut send, status, size).await,
        // Not provable from what is here: say so, so the fetcher asks
        // someone else.
        _ => refuse(&mut send, STATUS_NOT_HELD, size).await,
    }
}

/// Why a range could not be served: the data is gone or no longer matches
/// its proof (stop claiming it), or something passing got in the way (try
/// again later).
#[derive(Debug, PartialEq, Eq)]
enum Unserved {
    Changed,
    Passing,
}

impl From<std::io::Error> for Unserved {
    fn from(e: std::io::Error) -> Self {
        use std::io::ErrorKind::{NotFound, UnexpectedEof};
        if matches!(e.kind(), NotFound | UnexpectedEof) {
            Unserved::Changed
        } else {
            Unserved::Passing
        }
    }
}

impl From<bao_tree::io::EncodeError> for Unserved {
    fn from(e: bao_tree::io::EncodeError) -> Self {
        use bao_tree::io::EncodeError;
        match e {
            EncodeError::ParentHashMismatch(_)
            | EncodeError::LeafHashMismatch(_)
            | EncodeError::SizeMismatch => Unserved::Changed,
            EncodeError::Io(e) => Unserved::from(e),
            _ => Unserved::Passing,
        }
    }
}

fn header(status: u8, size: u64) -> Vec<u8> {
    postcard::to_allocvec(&PeerResponseHeader { status, size }).expect("header serializes")
}

async fn refuse(send: &mut BulkSend, status: u8, size: u64) {
    use tokio::io::AsyncWriteExt;
    let _ = write_framed(send, &header(status, size)).await;
    let _ = send.shutdown().await;
}

/// Answer a [`PeerAsk`], the frame after an empty one.
async fn answer_ask(
    mut send: BulkSend,
    mut recv: BulkRecv,
    server_key: [u8; 32],
    seeds: Arc<SeedStore>,
) {
    use tokio::io::AsyncWriteExt;

    let Ok(bytes) = read_framed(&mut recv, 8192).await else {
        return;
    };
    let Ok(PeerAsk::Have { token, root }) = postcard::from_bytes::<PeerAsk>(&bytes) else {
        return;
    };
    if !crate::cap::token_allows(&token, &server_key, &root, now_unix()) {
        refuse(&mut send, STATUS_DENIED, 0).await;
        return;
    }
    let (Some(size), Some(map)) = (seeds.get(&root).map(|s| s.size()), seeds.have(&root)) else {
        refuse(&mut send, STATUS_NOT_FOUND, 0).await;
        return;
    };
    if write_framed(&mut send, &header(STATUS_OK, size))
        .await
        .is_err()
    {
        return;
    }
    let map = postcard::to_allocvec(&map).expect("a have-map serializes");
    let _ = write_framed(&mut send, &map).await;
    let _ = send.shutdown().await;
}

/// A `WriteAt` sink for decoded leaves: absolute file offsets land into a
/// buffer based at `base` (the chunk-group floor of the requested offset).
struct OffsetBuf {
    base: u64,
    buf: Vec<u8>,
}

impl positioned_io::WriteAt for OffsetBuf {
    fn write_at(&mut self, pos: u64, data: &[u8]) -> std::io::Result<usize> {
        let Some(rel) = pos.checked_sub(self.base) else {
            return Err(std::io::Error::other("write below range base"));
        };
        let rel = rel as usize;
        if self.buf.len() < rel + data.len() {
            self.buf.resize(rel + data.len(), 0);
        }
        self.buf[rel..rel + data.len()].copy_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Fetch and verify one byte range from a peer. Returns exactly the
/// requested bytes (clamped to the file's end), each block verified
/// against `root` before it is accepted.
pub async fn fetch_range(
    endpoint: &str,
    cert_fp: [u8; 32],
    token: &[u8],
    root: [u8; 32],
    offset: u64,
    len: u64,
) -> Result<Vec<u8>, PeerError> {
    fetch_range_proved(endpoint, cert_fp, token, root, offset, len)
        .await
        .map(|proved| proved.bytes)
}

/// Dial a peer and open one request stream on it, every wait bounded: a
/// dead or unreachable peer fails fast instead of hanging on the QUIC idle
/// timeout (the scheduler retires it and moves on), and one that never
/// grants a stream fails like one that never answers.
async fn open_stream(
    endpoint: &str,
    cert_fp: [u8; 32],
) -> Result<(Box<dyn rabbithole_net::Connection>, BulkSend, BulkRecv), PeerError> {
    let transport = QuicTransport::new(
        "peer".to_string(),
        ServerAuth::Pinned(CertFingerprint(cert_fp)),
    );
    let conn = match tokio::time::timeout(PEER_CONNECT_TIMEOUT, transport.connect(endpoint)).await {
        Ok(r) => r?,
        Err(_) => return Err(PeerError::Refused(STATUS_BAD_REQUEST)),
    };
    let bulk = conn.bulk().ok_or(PeerError::BadRequest)?;
    let (send, recv) = match tokio::time::timeout(PEER_CONNECT_TIMEOUT, bulk.open()).await {
        Ok(r) => r?,
        Err(_) => return Err(PeerError::Refused(STATUS_BAD_REQUEST)),
    };
    Ok((conn, send, recv))
}

/// Ask a peer which units of `root` it holds. `Ok(None)` when it predates
/// the question (it closes the stream unanswered): such a peer seeds whole
/// files only.
pub async fn fetch_have(
    endpoint: &str,
    cert_fp: [u8; 32],
    token: &[u8],
    root: [u8; 32],
) -> Result<Option<HaveMap>, PeerError> {
    use tokio::io::AsyncWriteExt;

    let (conn, mut send, mut recv) = open_stream(endpoint, cert_fp).await?;
    let ask = postcard::to_allocvec(&PeerAsk::Have {
        token: token.to_vec(),
        root,
    })
    .expect("serializes");
    let sent = tokio::time::timeout(PEER_READ_TIMEOUT, async {
        write_framed(&mut send, &[]).await?;
        write_framed(&mut send, &ask).await?;
        send.shutdown().await.map_err(NetError::from)
    })
    .await;
    if !matches!(sent, Ok(Ok(()))) {
        return Ok(None);
    }
    let Ok(Ok(header)) = tokio::time::timeout(PEER_READ_TIMEOUT, read_framed(&mut recv, 64)).await
    else {
        return Ok(None);
    };
    let header: PeerResponseHeader =
        postcard::from_bytes(&header).map_err(|e| PeerError::Verify(e.to_string()))?;
    if header.status != STATUS_OK {
        return Err(PeerError::Refused(header.status));
    }
    let map =
        match tokio::time::timeout(PEER_READ_TIMEOUT, read_framed(&mut recv, HAVE_MAP_MAX)).await {
            Ok(r) => r?,
            Err(_) => return Err(PeerError::Refused(STATUS_BAD_REQUEST)),
        };
    drop(conn);
    let map: HaveMap = postcard::from_bytes(&map).map_err(|e| PeerError::Verify(e.to_string()))?;
    if map.unit == 0 || map.unit % PEER_BLOCK_BYTES != 0 {
        return Err(PeerError::Verify(
            "a have-map in units that are not whole blocks".into(),
        ));
    }
    Ok(Some(map))
}

/// [`fetch_range`], keeping the proof that came with the bytes.
pub async fn fetch_range_proved(
    endpoint: &str,
    cert_fp: [u8; 32],
    token: &[u8],
    root: [u8; 32],
    offset: u64,
    len: u64,
) -> Result<Proved, PeerError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    if len == 0 || len > PEER_REQUEST_MAX {
        return Err(PeerError::BadRequest);
    }
    let (conn, mut send, mut recv) = open_stream(endpoint, cert_fp).await?;

    let req = PeerRequest {
        token: token.to_vec(),
        root,
        offset,
        len,
    };
    let req = postcard::to_allocvec(&req).expect("serializes");
    match tokio::time::timeout(PEER_READ_TIMEOUT, write_framed(&mut send, &req)).await {
        Ok(r) => r?,
        Err(_) => return Err(PeerError::Refused(STATUS_BAD_REQUEST)),
    }
    match tokio::time::timeout(PEER_READ_TIMEOUT, send.shutdown()).await {
        Ok(r) => r?,
        Err(_) => return Err(PeerError::Refused(STATUS_BAD_REQUEST)),
    }

    let header = match tokio::time::timeout(PEER_READ_TIMEOUT, read_framed(&mut recv, 64)).await {
        Ok(r) => r?,
        Err(_) => return Err(PeerError::Refused(STATUS_BAD_REQUEST)),
    };
    let header: PeerResponseHeader =
        postcard::from_bytes(&header).map_err(|e| PeerError::Verify(e.to_string()))?;
    if header.status != STATUS_OK {
        return Err(PeerError::Refused(header.status));
    }
    // An honest peer asked for a range inside the file says the file goes
    // past it; one that says it ends before claims to have nothing, and is
    // not taken at its word (nothing it sent could be verified).
    if header.size <= offset {
        return Err(PeerError::Verify(
            "peer claims the file ends before the range".into(),
        ));
    }
    let len = len.min(header.size - offset);
    // Bound the body read: a peer that sent a valid header then stalls mid-stream
    // must fail (freeing the worker + the scheduler's join/progress-drain) rather
    // than park here until the QUIC idle timeout — or forever. And bound its
    // size: a Bao stream for `len` bytes is the covered 16 KiB blocks (one
    // more at each end for alignment) and their 64-byte parent hashes, so a
    // peer sending more than twice that is lying, and the rest is never read.
    let limit = 2 * len + 64 * 1024;
    let mut stream = Vec::new();
    match tokio::time::timeout(
        PEER_READ_TIMEOUT,
        (&mut recv).take(limit).read_to_end(&mut stream),
    )
    .await
    {
        Ok(r) => {
            r?;
        }
        Err(_) => return Err(PeerError::Refused(STATUS_BAD_REQUEST)),
    }
    drop(conn);

    // Verify the Bao stream against the root; only verified leaves land.
    let size = header.size;
    tokio::task::spawn_blocking(move || {
        let tree = BaoTree::new(size, PEER_BLOCK);
        let ranges = block_ranges(offset, len);
        let mut recorder = Recorder {
            tree,
            root: bao_blake3::Hash::from(root),
            parents: Vec::new(),
        };
        let base = (offset / PEER_BLOCK_BYTES) * PEER_BLOCK_BYTES;
        let mut target = OffsetBuf {
            base,
            buf: Vec::new(),
        };
        decode_ranges(stream.as_slice(), &ranges, &mut target, &mut recorder)
            .map_err(|e| PeerError::Verify(e.to_string()))?;
        let start = (offset - base) as usize;
        let end = start + len as usize;
        if target.buf.len() < end {
            return Err(PeerError::Verify("short verified stream".into()));
        }
        Ok(Proved {
            bytes: target.buf[start..end].to_vec(),
            parents: recorder.parents,
        })
    })
    .await
    .map_err(|e| PeerError::Verify(e.to_string()))?
}

/// Fetch a whole file from one peer, verified block-by-block, writing it
/// to `dest`. Returns the byte count.
pub async fn fetch_file(
    endpoint: &str,
    cert_fp: [u8; 32],
    token: &[u8],
    root: [u8; 32],
    size: u64,
    dest: &Path,
) -> Result<u64, PeerError> {
    use std::io::Write;
    let mut out = std::fs::File::create(dest)?;
    let mut offset = 0u64;
    while offset < size {
        let want = (size - offset).min(PEER_REQUEST_MAX);
        let bytes = fetch_range(endpoint, cert_fp, token, root, offset, want).await?;
        if bytes.is_empty() {
            return Err(PeerError::Verify("peer returned no bytes".into()));
        }
        out.write_all(&bytes)?;
        offset += bytes.len() as u64;
    }
    out.flush()?;
    Ok(offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabbithole_identity::IdentityKey;

    fn payload(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    fn root_of(bytes: &[u8]) -> [u8; 32] {
        *blake3::hash(bytes).as_bytes()
    }

    async fn peer_with(
        key: &IdentityKey,
        bytes: &[u8],
        dir: &Path,
    ) -> (PeerServer, [u8; 32], Arc<SeedStore>) {
        let path = dir.join("seed.bin");
        std::fs::write(&path, bytes).unwrap();
        let root = root_of(bytes);
        let seeds = Arc::new(SeedStore::new());
        seeds.add(root, &path).unwrap();
        let server = PeerServer::start(
            "127.0.0.1:0".parse().unwrap(),
            key.public().0,
            seeds.clone(),
        )
        .await
        .unwrap();
        (server, root, seeds)
    }

    fn token_for(key: &IdentityKey, root: [u8; 32], expires: i64) -> Vec<u8> {
        CapToken::issue(key, root, "tester", expires)
            .unwrap()
            .to_bytes()
    }

    #[test]
    fn a_have_map_counts_whole_units() {
        let map = HaveMap::whole(3 * HAVE_UNIT + 5);
        assert_eq!(map.bits, vec![0b1111]);
        assert!(map.covers(0, 4 * HAVE_UNIT));
        assert!(!map.covers(0, 4 * HAVE_UNIT + 1), "past the last unit");
        let mut map = map;
        map.clear(1);
        assert!(map.covers(0, HAVE_UNIT));
        assert!(!map.covers(HAVE_UNIT - 1, 2), "touches unit 1");
        assert!(map.covers(2 * HAVE_UNIT, HAVE_UNIT + 5));
        assert_eq!(HaveMap::whole(8 * HAVE_UNIT).bits, vec![0xFF]);
        assert_eq!(HaveMap::whole(0).bits, vec![1]);
    }

    /// A seed of part of a file: `body` on disk with every proof, sharing
    /// only the units in `held`.
    fn partial(dir: &Path, body: &[u8], held: &[u64]) -> (Arc<SeedStore>, [u8; 32], PathBuf) {
        let path = dir.join("part.bin");
        std::fs::write(&path, body).unwrap();
        let root = root_of(body);
        let outboard = PreOrderMemOutboard::create(body, PEER_BLOCK);
        let proofs = proofs_path(&path);
        std::fs::write(&proofs, &outboard.data).unwrap();
        let seeds = Arc::new(SeedStore::new());
        seeds
            .add_partial(
                root,
                body.len() as u64,
                &path,
                &proofs,
                held.iter().copied(),
            )
            .unwrap();
        (seeds, root, path)
    }

    #[tokio::test]
    async fn a_partial_seed_serves_what_it_holds_and_proves_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let key = IdentityKey::from_seed(&[5; 32]);
        let body = payload((4 * HAVE_UNIT + 100) as usize);
        let (seeds, root, path) = partial(dir.path(), &body, &[0, 2]);
        let server = PeerServer::start(
            "127.0.0.1:0".parse().unwrap(),
            key.public().0,
            seeds.clone(),
        )
        .await
        .unwrap();
        let endpoint = format!("127.0.0.1:{}", server.addr.port());
        let fp = server.fingerprint.0;
        let token = token_for(&key, root, now_unix() + 60);
        let unit = |i: u64| {
            let start = (i * HAVE_UNIT) as usize;
            body[start..(start + HAVE_UNIT as usize).min(body.len())].to_vec()
        };

        // It says what it holds.
        let map = fetch_have(&endpoint, fp, &token, root)
            .await
            .unwrap()
            .unwrap();
        assert!(map.covers(0, HAVE_UNIT) && map.covers(2 * HAVE_UNIT, HAVE_UNIT));
        assert!(!map.covers(HAVE_UNIT, 1) && !map.covers(4 * HAVE_UNIT, 1));

        // It serves that, verified, with its proof.
        let got = fetch_range_proved(&endpoint, fp, &token, root, 2 * HAVE_UNIT, HAVE_UNIT)
            .await
            .unwrap();
        assert_eq!(got.bytes, unit(2));
        assert!(!got.parents.is_empty(), "the proof came with it");
        // And says it does not hold the rest, rather than failing.
        let err = fetch_range(&endpoint, fp, &token, root, HAVE_UNIT, HAVE_UNIT)
            .await
            .unwrap_err();
        assert!(matches!(err, PeerError::Refused(STATUS_NOT_HELD)), "{err}");

        // A unit that no longer matches its proof (the file changed on
        // disk) is not served, and not offered again.
        let mut changed = body.clone();
        changed[(2 * HAVE_UNIT + 7) as usize] ^= 0xFF;
        std::fs::write(&path, &changed).unwrap();
        let err = fetch_range(&endpoint, fp, &token, root, 2 * HAVE_UNIT, HAVE_UNIT)
            .await
            .unwrap_err();
        assert!(matches!(err, PeerError::Refused(STATUS_NOT_HELD)), "{err}");
        let map = fetch_have(&endpoint, fp, &token, root)
            .await
            .unwrap()
            .unwrap();
        assert!(!map.covers(2 * HAVE_UNIT, 1) && map.covers(0, HAVE_UNIT));
        assert_eq!(
            fetch_range(&endpoint, fp, &token, root, 0, HAVE_UNIT)
                .await
                .unwrap(),
            unit(0)
        );

        // Without a capability for the file, it says nothing of it.
        let other = token_for(&key, [9; 32], now_unix() + 60);
        let err = fetch_have(&endpoint, fp, &other, root).await.unwrap_err();
        assert!(matches!(err, PeerError::Refused(STATUS_DENIED)), "{err}");

        // A whole seed of the same file replaces the part, and removing the
        // part afterwards leaves the whole one.
        seeds.add(root, &dir.path().join("part.bin")).unwrap_err();
        let whole = dir.path().join("whole.bin");
        std::fs::write(&whole, &body).unwrap();
        seeds.add(root, &whole).unwrap();
        seeds.remove_partial(&root);
        assert_eq!(seeds.have(&root), Some(HaveMap::whole(body.len() as u64)));
        assert!(seeds
            .add_partial(root, body.len() as u64, &whole, &proofs_path(&whole), [0])
            .is_none());
    }

    #[tokio::test]
    async fn fetch_full_file_verified() {
        let dir = tempfile::tempdir().unwrap();
        let key = IdentityKey::from_seed(&[1; 32]);
        // ~100 KiB: several 16 KiB Bao blocks plus a partial tail.
        let body = payload(100 * 1024 + 137);
        let (server, root, _seeds) = peer_with(&key, &body, dir.path()).await;
        let token = token_for(&key, root, now_unix() + 60);

        let endpoint = format!("127.0.0.1:{}", server.addr.port());
        let dest = dir.path().join("out.bin");
        let n = fetch_file(
            &endpoint,
            server.fingerprint.0,
            &token,
            root,
            body.len() as u64,
            &dest,
        )
        .await
        .unwrap();
        assert_eq!(n, body.len() as u64);
        assert_eq!(std::fs::read(&dest).unwrap(), body);
    }

    #[tokio::test]
    async fn fetch_unaligned_middle_range() {
        let dir = tempfile::tempdir().unwrap();
        let key = IdentityKey::from_seed(&[2; 32]);
        let body = payload(80 * 1024);
        let (server, root, _seeds) = peer_with(&key, &body, dir.path()).await;
        let token = token_for(&key, root, now_unix() + 60);
        let endpoint = format!("127.0.0.1:{}", server.addr.port());

        // Straddles block boundaries at neither-aligned offsets.
        let (off, len) = (10_000u64, 30_123u64);
        let bytes = fetch_range(&endpoint, server.fingerprint.0, &token, root, off, len)
            .await
            .unwrap();
        assert_eq!(bytes, body[off as usize..(off + len) as usize]);
    }

    #[tokio::test]
    async fn expired_or_mismatched_tokens_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let key = IdentityKey::from_seed(&[3; 32]);
        let body = payload(4096);
        let (server, root, _seeds) = peer_with(&key, &body, dir.path()).await;
        let endpoint = format!("127.0.0.1:{}", server.addr.port());

        // Expired.
        let stale = token_for(&key, root, now_unix() - 1);
        let err = fetch_range(&endpoint, server.fingerprint.0, &stale, root, 0, 100)
            .await
            .unwrap_err();
        assert!(matches!(err, PeerError::Refused(STATUS_DENIED)), "{err}");

        // Signed by the wrong server.
        let mallory = IdentityKey::from_seed(&[9; 32]);
        let forged = token_for(&mallory, root, now_unix() + 60);
        let err = fetch_range(&endpoint, server.fingerprint.0, &forged, root, 0, 100)
            .await
            .unwrap_err();
        assert!(matches!(err, PeerError::Refused(STATUS_DENIED)));

        // Token for a different root than requested.
        let other = token_for(&key, [0xAA; 32], now_unix() + 60);
        let err = fetch_range(&endpoint, server.fingerprint.0, &other, root, 0, 100)
            .await
            .unwrap_err();
        assert!(matches!(err, PeerError::Refused(STATUS_DENIED)));
    }

    #[tokio::test]
    async fn tampered_seed_cannot_serve_wrong_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let key = IdentityKey::from_seed(&[4; 32]);
        let body = payload(64 * 1024);
        let (server, root, seeds) = peer_with(&key, &body, dir.path()).await;
        let token = token_for(&key, root, now_unix() + 60);
        let endpoint = format!("127.0.0.1:{}", server.addr.port());

        // Corrupt the file on disk after the outboard was computed: the
        // peer's validated encode fails, so it drops the seed and says it
        // does not have the file, and the fetcher gets an error — never
        // silently wrong bytes.
        let path = dir.path().join("seed.bin");
        let mut corrupted = body.clone();
        corrupted[20_000] ^= 0xFF;
        std::fs::write(&path, &corrupted).unwrap();

        let err = fetch_range(
            &endpoint,
            server.fingerprint.0,
            &token,
            root,
            16_384,
            16_384,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(
                err,
                PeerError::Refused(STATUS_NOT_HELD | STATUS_NOT_FOUND)
                    | PeerError::Verify(_)
                    | PeerError::Io(_)
                    | PeerError::Net(_)
            ),
            "fetch must fail on tampered data, got: {err}"
        );
        // And the seed is dropped: it no longer claims the file at all.
        assert!(seeds.have(&root).is_none());

        // SeedStore.add refuses a file that doesn't match its root.
        let seeds = SeedStore::new();
        assert!(matches!(
            seeds.add(root, &path),
            Err(PeerError::RootMismatch)
        ));
    }

    #[tokio::test]
    async fn seeding_peer_answers_not_found_for_unknown_roots() {
        let dir = tempfile::tempdir().unwrap();
        let key = IdentityKey::from_seed(&[5; 32]);
        let body = payload(4096);
        let (server, _root, _seeds) = peer_with(&key, &body, dir.path()).await;
        let endpoint = format!("127.0.0.1:{}", server.addr.port());

        let unknown = [0x77; 32];
        let token = token_for(&key, unknown, now_unix() + 60);
        let err = fetch_range(&endpoint, server.fingerprint.0, &token, unknown, 0, 100)
            .await
            .unwrap_err();
        assert!(matches!(err, PeerError::Refused(STATUS_NOT_FOUND)));
    }
}
