//! The peer wire (Wave 5.3b): direct peer-to-peer file serving with
//! per-chunk Bao verification.
//!
//! A sharing peer runs a [`PeerServer`] — a QUIC endpoint (the same
//! `rabbithole-net` stack the client already speaks, fingerprint-pinned
//! self-signed TLS) that serves byte ranges of files it seeds. Every
//! request carries a server-signed [`CapToken`]; every response is a Bao
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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use bao_tree::io::outboard::{EmptyOutboard, PreOrderMemOutboard};
use bao_tree::io::sync::{decode_ranges, encode_ranges_validated};
use bao_tree::io::{round_up_to_chunks, round_up_to_chunks_groups};
use bao_tree::{blake3 as bao_blake3, BaoTree, BlockSize, ChunkRanges};
use rabbithole_net::quic::{QuicListener, QuicTransport};
use rabbithole_net::tls::{CertFingerprint, ServerAuth, TlsIdentity};
use rabbithole_net::{
    read_framed, write_framed, BulkRecv, BulkSend, Listener, NetError, Transport,
};
use range_collections::RangeSet2;
use serde::{Deserialize, Serialize};

use crate::cap::CapToken;

/// Bao block size on the peer wire: 16 KiB chunk groups (log2(16) = 4).
/// The root hash is the plain blake3 of the file regardless, so peer-wire
/// roots are the same ids the blob store and adverts use.
pub const PEER_BLOCK: BlockSize = BlockSize::from_chunk_log(4);
/// Bytes covered by one Bao block at [`PEER_BLOCK`].
pub const PEER_BLOCK_BYTES: u64 = 16 * 1024;
/// Most bytes one [`PeerRequest`] may ask for (whole files loop ranges).
pub const PEER_REQUEST_MAX: u64 = 4 * 1024 * 1024;

/// Response status codes.
pub const STATUS_OK: u8 = 0;
pub const STATUS_DENIED: u8 = 1;
pub const STATUS_NOT_FOUND: u8 = 2;
pub const STATUS_BAD_REQUEST: u8 = 3;

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

/// What a peer seeds: root → (path, size, precomputed outboard).
///
/// The outboard (the Bao parent-hash tree) is computed once at [`add`] time
/// by reading the file through; serving then reads only the requested
/// leaves. ~64 bytes of outboard per 16 KiB of file.
#[derive(Default)]
pub struct SeedStore {
    inner: RwLock<HashMap<[u8; 32], Seed>>,
}

#[derive(Clone)]
struct Seed {
    path: PathBuf,
    size: u64,
    outboard: Arc<PreOrderMemOutboard>,
}

impl SeedStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read `path`, verify it hashes to `root`, and start seeding it.
    pub fn add(&self, root: [u8; 32], path: &Path) -> Result<(), PeerError> {
        let bytes = std::fs::read(path)?;
        let outboard = PreOrderMemOutboard::create(&bytes, PEER_BLOCK);
        if *outboard.root.as_bytes() != root {
            return Err(PeerError::RootMismatch);
        }
        self.inner.write().expect("not poisoned").insert(
            root,
            Seed {
                path: path.to_path_buf(),
                size: bytes.len() as u64,
                outboard: Arc::new(outboard),
            },
        );
        Ok(())
    }

    pub fn remove(&self, root: &[u8; 32]) {
        self.inner.write().expect("not poisoned").remove(root);
    }

    fn get(&self, root: &[u8; 32]) -> Option<Seed> {
        self.inner.read().expect("not poisoned").get(root).cloned()
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
    let Ok(req) = postcard::from_bytes::<PeerRequest>(&bytes) else {
        return;
    };

    let respond = |status: u8, size: u64| {
        postcard::to_allocvec(&PeerResponseHeader { status, size }).expect("header serializes")
    };

    // Authorize: a valid, unexpired capability for this exact root.
    let authorized = CapToken::from_bytes(&req.token)
        .map(|t| t.verify(&server_key, &req.root, now_unix()).is_ok())
        .unwrap_or(false);
    if !authorized {
        let h = respond(STATUS_DENIED, 0);
        let _ = write_framed(&mut send, &h).await;
        let _ = send.shutdown().await;
        return;
    }
    let Some(seed) = seeds.get(&req.root) else {
        let h = respond(STATUS_NOT_FOUND, 0);
        let _ = write_framed(&mut send, &h).await;
        let _ = send.shutdown().await;
        return;
    };
    if req.len == 0 || req.len > PEER_REQUEST_MAX || req.offset >= seed.size {
        let h = respond(STATUS_BAD_REQUEST, seed.size);
        let _ = write_framed(&mut send, &h).await;
        let _ = send.shutdown().await;
        return;
    }
    let len = req.len.min(seed.size - req.offset);

    // Encode the chunk-aligned ranges (validated against the outboard: a
    // file that changed on disk fails here rather than serving bad bytes).
    let encoded = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, PeerError> {
        let file = std::fs::File::open(&seed.path)?;
        let ranges = block_ranges(req.offset, len);
        let mut out = Vec::new();
        encode_ranges_validated(&file, seed.outboard.as_ref(), &ranges, &mut out)
            .map_err(|e| PeerError::Verify(e.to_string()))?;
        Ok(out)
    })
    .await;

    match encoded {
        Ok(Ok(stream)) => {
            let h = respond(STATUS_OK, seed.size);
            if write_framed(&mut send, &h).await.is_err() {
                return;
            }
            let _ = send.write_all(&stream).await;
            let _ = send.shutdown().await;
        }
        _ => {
            // Encoding failed (stale seed, io error): drop without a body.
        }
    }
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    if len == 0 || len > PEER_REQUEST_MAX {
        return Err(PeerError::BadRequest);
    }
    let transport = QuicTransport::new(
        "peer".to_string(),
        ServerAuth::Pinned(CertFingerprint(cert_fp)),
    );
    let conn = transport.connect(endpoint).await?;
    let bulk = conn.bulk().ok_or(PeerError::BadRequest)?;
    let (mut send, mut recv) = bulk.open().await?;

    let req = PeerRequest {
        token: token.to_vec(),
        root,
        offset,
        len,
    };
    write_framed(&mut send, &postcard::to_allocvec(&req).expect("serializes")).await?;
    send.shutdown().await?;

    let header = read_framed(&mut recv, 64).await?;
    let header: PeerResponseHeader =
        postcard::from_bytes(&header).map_err(|e| PeerError::Verify(e.to_string()))?;
    if header.status != STATUS_OK {
        return Err(PeerError::Refused(header.status));
    }
    let len = len.min(header.size.saturating_sub(offset));
    if len == 0 {
        return Ok(Vec::new());
    }
    let mut stream = Vec::new();
    recv.read_to_end(&mut stream).await?;
    drop(conn);

    // Verify the Bao stream against the root; only verified leaves land.
    let size = header.size;
    tokio::task::spawn_blocking(move || {
        let tree = BaoTree::new(size, PEER_BLOCK);
        let ranges = block_ranges(offset, len);
        let outboard = EmptyOutboard {
            tree,
            root: bao_blake3::Hash::from(root),
        };
        let base = (offset / PEER_BLOCK_BYTES) * PEER_BLOCK_BYTES;
        let mut target = OffsetBuf {
            base,
            buf: Vec::new(),
        };
        decode_ranges(stream.as_slice(), &ranges, &mut target, outboard)
            .map_err(|e| PeerError::Verify(e.to_string()))?;
        let start = (offset - base) as usize;
        let end = start + len as usize;
        if target.buf.len() < end {
            return Err(PeerError::Verify("short verified stream".into()));
        }
        Ok(target.buf[start..end].to_vec())
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
        let (server, root, _seeds) = peer_with(&key, &body, dir.path()).await;
        let token = token_for(&key, root, now_unix() + 60);
        let endpoint = format!("127.0.0.1:{}", server.addr.port());

        // Corrupt the file on disk after the outboard was computed: the
        // peer's validated encode fails, so the fetcher gets an error —
        // never silently wrong bytes.
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
                PeerError::Verify(_) | PeerError::Io(_) | PeerError::Net(_)
            ),
            "fetch must fail on tampered data, got: {err}"
        );

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
