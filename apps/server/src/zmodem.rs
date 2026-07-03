//! ZMODEM file transfers over the telnet BBS (Wave 6): the tokio driving
//! slice over the sans-IO `rabbithole-legacy-zmodem` codec.
//!
//! The files sub-shell ([`crate::telnet`]) gains two verbs:
//!
//! - **`zget <name>`** — the server runs the codec's [`Sender`]: `rz\r` +
//!   `ZRQINIT`, handshake, `ZFILE` (name/size/mtime), data subpackets from
//!   the library blob, `ZEOF`, `ZFIN`. `ZRPOS` repositioning is honored at
//!   every stage (initial offset, mid-stream rewind, post-`ZEOF`
//!   correction), so receivers can crash-recover a partial download. The
//!   download is counted on completion — the same counter the HTTP handoff
//!   bumps.
//! - **`zput`** — the server runs the codec's [`Receiver`]: accept `ZFILE`,
//!   sanitize the offered name (basename only, no control bytes, the
//!   [`FileService`](rabbithole_server_core::FileService) length cap, no
//!   clobbering), stream subpackets into in-memory staging under the
//!   declared-size/quota caps, and on `ZEOF` finalize with the native
//!   upload discipline — blake3, the moderation hash-deny list, quota
//!   re-checked on actual bytes, content-addressed blob commit,
//!   `add_file` + [`ServerEvent::FileAdded`]. Batches work: each `ZFILE`
//!   in the session is vetted and finalized independently.
//!
//! ## 8-bit cleanliness
//!
//! ZMODEM needs a transparent byte channel; telnet is not one. Both
//! directions ride the [`TelnetStream`] binary seam: outbound frames go
//! through `write_binary` (IAC doubled, no newline translation, no CP437
//! translation), inbound bytes come from `read_binary` (IAC undoubled,
//! negotiation absorbed). The codec's ZDLE layer never *emits* a raw
//! `0xFF` (it escapes `0xFF` as `ZDLE ZRUB1`), but real senders may leave
//! `0xFF` unescaped — legal ZMODEM — and those bytes survive the telnet
//! hop precisely because of the IAC doubling at this seam.
//!
//! ## Resume
//!
//! - **Downloads**: the codec's `Sender` honors any `ZRPOS`, so a receiver
//!   that answers `ZFILE` with `ZRPOS(n)` gets only the tail.
//! - **Uploads**: interrupted `zput` staging is parked in [`Partials`]
//!   (in-memory, TTL'd — the Hotline HTXF partial-upload discipline),
//!   keyed by `(account, area, folder, name)`. When the same account
//!   re-offers the same destination, the server arms
//!   [`Receiver::set_resume_offset`] and answers the `ZFILE` with
//!   `ZRPOS(staged)`; the client seeks and sends only the tail. Persisting
//!   partials across restarts is a follow-up, matching HTXF.
//!
//! ## Teardown discipline
//!
//! Every exit path returns the session to a usable line-mode prompt: on
//! our aborts (timeout, protocol error, refusal) the classic cancel
//! sequence (eight CANs + eight backspaces) is sent, and either way any
//! in-flight transfer residue is drained (bounded quiet-wait) so stray
//! frames never replay into `read_line` as garbage commands.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rabbithole_blobs::BlobId;
use rabbithole_legacy_telnet::TelnetStream;
use rabbithole_legacy_zmodem::subpacket::MAX_PAYLOAD;
use rabbithole_legacy_zmodem::{
    decode_header, decode_subpacket, encode_subpacket, DecodedHeader, DecodedSubpacket, FileInfo,
    FrameEnd, FrameType, HeaderError, HeaderFormat, Receiver, RecvAction, RecvEvent, RecvState,
    SendAction, SendEvent, Sender, SessionError, SubpacketError, ZDLE, ZPAD,
};
use rabbithole_server_core::{AuthedUser, ServerEvent};
use rabbithole_store_server::repo::AuditRepo;
use rabbithole_store_server::repo6::FileNodeRow;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::Shared;

/// Idle budget for one read or write during a transfer; a peer that goes
/// quiet longer than this gets the session aborted (and, for uploads, its
/// staging parked for resume).
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Hard ceiling on one `zput` upload's staged bytes (ZMODEM offsets are
/// 32-bit anyway; larger files belong on the native transfer path). Matches
/// the Hotline HTXF in-memory staging bound.
const MAX_ZPUT_BYTES: u64 = 64 * 1024 * 1024;

/// How long interrupted-upload staging is kept for resume (HTXF parity).
const PARTIAL_TTL: Duration = Duration::from_secs(30 * 60);

/// Consecutive CANs from the peer that abort the session (the spec's five).
const CANCEL_CANS: u32 = 5;

/// The classic abort sequence: CANs to stop the peer's engine, backspaces
/// to tidy its terminal.
const ABORT_SEQ: [u8; 16] = [
    0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08,
];

/// One quiet interval ends the post-transfer drain...
const DRAIN_QUIET: Duration = Duration::from_millis(250);
/// ...and the drain never runs longer than this in total.
const DRAIN_MAX: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------------------
// Interrupted-upload staging (resume across reconnects)
// ---------------------------------------------------------------------------

/// Parked partial uploads, keyed by `(account, area, folder, name)` — the
/// zmodem twin of the Hotline hub's HTXF partial store. In-memory with a
/// TTL; persistence across restarts is a documented follow-up.
pub struct Partials {
    inner: Mutex<HashMap<String, Partial>>,
}

struct Partial {
    data: Vec<u8>,
    expires: Instant,
}

impl Default for Partials {
    fn default() -> Self {
        Self::new()
    }
}

impl Partials {
    /// An empty store.
    pub fn new() -> Partials {
        Partials {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Take (and remove) parked bytes for a destination; empty when none
    /// (or only an expired entry) is live.
    fn take(&self, key: &str) -> Vec<u8> {
        match self.inner.lock().remove(key) {
            Some(p) if p.expires > Instant::now() => p.data,
            _ => Vec::new(),
        }
    }

    /// Park an interrupted upload's bytes for resume (fresh TTL).
    fn save(&self, key: String, data: Vec<u8>) {
        self.inner.lock().insert(
            key,
            Partial {
                data,
                expires: Instant::now() + PARTIAL_TTL,
            },
        );
    }
}

/// The staging key: per uploader account and destination, so a resume can
/// only continue *your own* interrupted upload of that file.
fn partial_key(account_id: i64, area: &str, folder: Option<&str>, name: &str) -> String {
    format!(
        "{account_id}\u{1f}{area}\u{1f}{}\u{1f}{name}",
        folder.unwrap_or("")
    )
}

// ---------------------------------------------------------------------------
// Transfer-level errors
// ---------------------------------------------------------------------------

/// Why a transfer stopped before `Finished`.
enum Zx {
    /// The transport failed or hit EOF (the caller is likely gone).
    Io(io::Error),
    /// The peer went quiet past [`IDLE_TIMEOUT`].
    Timeout,
    /// The peer struck CANs (or declined the file with ZSKIP/ZABORT/ZFERR).
    Cancelled,
    /// The byte stream or event order was wrong; recovery is out of scope.
    Protocol(String),
    /// Policy said no (bad name, collision, size, quota).
    Refused(String),
}

impl From<SessionError> for Zx {
    fn from(e: SessionError) -> Zx {
        Zx::Protocol(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// The wire: framing over the TelnetStream binary seam
// ---------------------------------------------------------------------------

/// Byte-level plumbing for one transfer: buffered inbound frames, timed
/// reads/writes, garbage skipping, and cancel counting.
struct Wire<'a, S> {
    t: &'a mut TelnetStream<S>,
    buf: Vec<u8>,
    /// Consecutive CANs seen while scanning for a header (survives refills).
    cans: u32,
}

impl<'a, S: AsyncRead + AsyncWrite + Unpin> Wire<'a, S> {
    fn new(t: &'a mut TelnetStream<S>) -> Wire<'a, S> {
        Wire {
            t,
            buf: Vec::new(),
            cans: 0,
        }
    }

    /// Transmit wire bytes through the IAC-doubling seam, with a timeout so
    /// a stalled peer cannot wedge the shell.
    async fn send(&mut self, bytes: &[u8]) -> Result<(), Zx> {
        match tokio::time::timeout(IDLE_TIMEOUT, self.t.write_binary(bytes)).await {
            Err(_) => Err(Zx::Timeout),
            Ok(r) => r.map_err(Zx::Io),
        }
    }

    /// Pull the next inbound chunk into the frame buffer.
    async fn refill(&mut self) -> Result<(), Zx> {
        match tokio::time::timeout(IDLE_TIMEOUT, self.t.read_binary()).await {
            Err(_) => Err(Zx::Timeout),
            Ok(Err(e)) => Err(Zx::Io(e)),
            Ok(Ok(None)) => Err(Zx::Io(io::ErrorKind::UnexpectedEof.into())),
            Ok(Ok(Some(chunk))) => {
                self.buf.extend_from_slice(&chunk);
                Ok(())
            }
        }
    }

    /// Discard bytes ahead of the next `ZPAD`, counting consecutive CANs
    /// (five abort the session — CAN and ZDLE are the same byte, but a
    /// header always leads with `*`, so CANs seen here are never framing).
    fn skip_to_zpad(&mut self) -> Result<(), Zx> {
        let mut i = 0;
        while i < self.buf.len() && self.buf[i] != ZPAD {
            if self.buf[i] == ZDLE {
                self.cans += 1;
                if self.cans >= CANCEL_CANS {
                    return Err(Zx::Cancelled);
                }
            } else {
                self.cans = 0;
            }
            i += 1;
        }
        if i < self.buf.len() {
            self.cans = 0; // real traffic follows
        }
        self.buf.drain(..i);
        Ok(())
    }

    /// The next well-formed header, skipping line noise (the dangling LF of
    /// the command line that started the transfer, hex-header trailers,
    /// garbled bytes) and honoring cancels.
    async fn next_header(&mut self) -> Result<DecodedHeader, Zx> {
        loop {
            self.skip_to_zpad()?;
            if self.buf.is_empty() {
                self.refill().await?;
                continue;
            }
            match decode_header(&self.buf) {
                Ok(decoded) => {
                    self.buf.drain(..decoded.consumed);
                    return Ok(decoded);
                }
                Err(HeaderError::Incomplete) => self.refill().await?,
                Err(HeaderError::Cancelled) => return Err(Zx::Cancelled),
                // Garbled: shed the leading pad and rescan (resync).
                Err(_) => {
                    self.buf.remove(0);
                }
            }
        }
    }

    /// The next data subpacket of the current frame (`wide` = 32-bit CRC,
    /// per the header format that opened the frame).
    async fn next_subpacket(&mut self, wide: bool) -> Result<DecodedSubpacket, Zx> {
        loop {
            if self.buf.is_empty() {
                self.refill().await?;
            }
            match decode_subpacket(&self.buf, wide) {
                Ok(sub) => {
                    self.buf.drain(..sub.consumed);
                    return Ok(sub);
                }
                Err(SubpacketError::Incomplete) => self.refill().await?,
                Err(SubpacketError::Cancelled) => return Err(Zx::Cancelled),
                Err(e) => return Err(Zx::Protocol(format!("bad subpacket: {e}"))),
            }
        }
    }

    /// Fire the classic cancel sequence (best-effort; the peer may be gone).
    async fn abort(&mut self) {
        let _ = tokio::time::timeout(DRAIN_QUIET, self.t.write_binary(&ABORT_SEQ)).await;
    }

    /// Swallow in-flight transfer residue — the peer's trailing `"OO"`
    /// after a completed session, CAN/backspace storms after an abort —
    /// until the line goes quiet (bounded), so line mode resumes on a
    /// clean stream. Seeing `"OO"` ends the drain immediately: a compliant
    /// peer sends nothing after over-and-out.
    async fn drain_residue(&mut self) {
        let mut tail = [0u8; 2];
        let start = Instant::now();
        while start.elapsed() < DRAIN_MAX {
            match tokio::time::timeout(DRAIN_QUIET, self.t.read_binary()).await {
                Ok(Ok(Some(chunk))) => {
                    for &b in &chunk {
                        tail = [tail[1], b];
                    }
                    if &tail == b"OO" {
                        break;
                    }
                }
                _ => break, // quiet, EOF, or transport error
            }
        }
    }
}

// ---------------------------------------------------------------------------
// zget: server sends a library file
// ---------------------------------------------------------------------------

/// Send `target` (an authorized, resolved library file) to the caller via
/// ZMODEM. RBAC/moderation/rate checks are the caller's (shared with `get`);
/// this drives the protocol, counts the download on completion, and always
/// returns the shell to a usable prompt. Only transport failures err.
pub async fn send_file<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    target: &FileNodeRow,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let Some(blob_id) = target.blob_id else {
        return t.write_str("\nThat file has no content to send.\n").await;
    };
    let blobs = shared.blobs.clone();
    let bytes = match tokio::task::spawn_blocking(move || blobs.get(&BlobId(blob_id))).await {
        Ok(Ok(b)) => b,
        _ => {
            return t
                .write_str("\nThat file is unavailable right now; try again later.\n")
                .await;
        }
    };
    if bytes.len() as u64 > u64::from(u32::MAX) {
        return t
            .write_str("\nThat file is too large for ZMODEM; use the web interface.\n")
            .await;
    }
    t.write_str(&format!(
        "\nSending {} ({} bytes) via ZMODEM. Start your receive now; \
         five Ctrl-X cancel.\n",
        target.name,
        bytes.len()
    ))
    .await?;

    let info = FileInfo {
        length: Some(bytes.len() as u64),
        mtime: Some((target.created_at / 1000).max(0) as u64),
        mode: Some(0o100644),
        ..FileInfo::new(target.name.clone())
    };
    let mut wire = Wire::new(t);
    let outcome = drive_send(&mut wire, Sender::new(info), &bytes).await;
    let detail = format!("{}/{} bytes={}", target.area, target.path, bytes.len());
    match outcome {
        Ok(()) => {
            // The receiver's own trailing "OO" (this codec's receiver sends
            // one) must not replay into line mode as a command.
            wire.drain_residue().await;
            if let Err(e) = shared.files.record_download(target.id).await {
                tracing::warn!("zmodem download counter failed: {e}");
            }
            audit(
                shared,
                &authed.account.login,
                "zmodem-send",
                format!("{detail} outcome=complete"),
            );
            t.write_str("\nZMODEM send complete.\n").await
        }
        Err(zx) => finish_failed(t, shared, authed, "zmodem-send", &detail, zx, None).await,
    }
}

/// Drive the codec [`Sender`] to completion over the wire.
async fn drive_send<S>(wire: &mut Wire<'_, S>, mut tx: Sender, bytes: &[u8]) -> Result<(), Zx>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // The classic opener: wakes auto-download in SyncTERM/qodem/rz.
    wire.send(b"rz\r").await?;
    let mut pending: VecDeque<SendAction> = tx.start()?.into();
    loop {
        while let Some(action) = pending.pop_front() {
            match action {
                SendAction::SendHeader { header, format } => {
                    wire.send(&header.encode(format)).await?;
                }
                SendAction::SendFileInfo(info) => {
                    let payload = info
                        .encode()
                        .map_err(|e| Zx::Protocol(format!("file info: {e}")))?;
                    let sub = encode_subpacket(&payload, FrameEnd::Zcrcw, tx.peer_can_fc32())
                        .map_err(|e| Zx::Protocol(format!("file info subpacket: {e}")))?;
                    wire.send(&sub).await?;
                }
                SendAction::StreamData { from } => {
                    stream_data(wire, tx.peer_can_fc32(), bytes, from).await?;
                    let exhausted = SendEvent::DataExhausted {
                        offset: bytes.len() as u32,
                    };
                    pending.extend(tx.advance(exhausted)?);
                }
                SendAction::SendOverAndOut => wire.send(b"OO").await?,
                SendAction::Finished => return Ok(()),
            }
        }
        let decoded = wire.next_header().await?;
        if matches!(
            decoded.header.frame_type,
            FrameType::Zskip | FrameType::Zabort | FrameType::Zferr | FrameType::Zcan
        ) {
            return Err(Zx::Cancelled);
        }
        pending.extend(tx.advance(SendEvent::Header(decoded.header))?);
    }
}

/// Stream `bytes[from..]` as ZDATA subpackets: ZCRCG runs, ZCRCE on the
/// last (an empty file still gets one empty ZCRCE so the frame closes).
async fn stream_data<S>(
    wire: &mut Wire<'_, S>,
    wide: bool,
    bytes: &[u8],
    from: u32,
) -> Result<(), Zx>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let start = (from as usize).min(bytes.len());
    let rest = &bytes[start..];
    let sub_err = |e| Zx::Protocol(format!("data subpacket: {e}"));
    if rest.is_empty() {
        let sub = encode_subpacket(&[], FrameEnd::Zcrce, wide).map_err(sub_err)?;
        return wire.send(&sub).await;
    }
    let mut chunks = rest.chunks(MAX_PAYLOAD).peekable();
    while let Some(chunk) = chunks.next() {
        let end = if chunks.peek().is_some() {
            FrameEnd::Zcrcg
        } else {
            FrameEnd::Zcrce
        };
        let sub = encode_subpacket(chunk, end, wide).map_err(sub_err)?;
        wire.send(&sub).await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// zput: server receives an upload into the current folder
// ---------------------------------------------------------------------------

/// One vetted, in-flight upload.
struct InFlight {
    /// Sanitized destination file name.
    name: String,
    /// Staged bytes so far (seeded from [`Partials`] on resume).
    data: Vec<u8>,
    /// Per-file byte ceiling (declared size capped by [`MAX_ZPUT_BYTES`]).
    cap: u64,
    /// Staging key for parking on interruption.
    key: String,
}

/// Receive one ZMODEM batch into `area`/`folder`. RBAC (`FILE_UPLOAD` on
/// the destination — drop boxes included, the classic use), the guest gate,
/// and the transfer rate budget are the caller's; per-file vetting
/// (name/collision/size/quota) and the finalize gates (blake3 → hash-deny →
/// quota-on-actual → blob → `add_file` + `FileAdded`) run here. Only
/// transport failures err.
pub async fn receive_files<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    area: &str,
    folder: Option<&str>,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    t.write_str(
        "\nReady for ZMODEM. Begin your send now; five Ctrl-X cancel. \
         Interrupted uploads resume from where they stopped.\n",
    )
    .await?;
    let mut wire = Wire::new(t);
    let mut rx = Receiver::new();
    // CRC width of the current data frame, from its opening header format.
    let mut wide = false;
    let mut current: Option<InFlight> = None;
    let mut results: Vec<String> = Vec::new();

    let outcome = loop {
        // Route by receiver state: file-info and data arrive as subpackets,
        // everything else as headers.
        let event = match rx.state() {
            RecvState::AwaitingFileInfo | RecvState::ReceivingData { .. } => {
                match wire.next_subpacket(wide).await {
                    Ok(sub) => RecvEvent::Data {
                        payload: sub.payload,
                        end: sub.end,
                    },
                    Err(zx) => break Err(zx),
                }
            }
            _ => match wire.next_header().await {
                Ok(decoded) => {
                    if matches!(
                        decoded.header.frame_type,
                        FrameType::Zfile | FrameType::Zdata
                    ) {
                        wide = decoded.format == HeaderFormat::Bin32;
                    }
                    if matches!(
                        decoded.header.frame_type,
                        FrameType::Zabort | FrameType::Zferr | FrameType::Zcan
                    ) {
                        break Err(Zx::Cancelled);
                    }
                    RecvEvent::Header(decoded.header)
                }
                Err(zx) => break Err(zx),
            },
        };
        // A ZFILE offer: vet it (and arm resume) before the codec answers.
        if rx.state() == RecvState::AwaitingFileInfo {
            if let RecvEvent::Data { payload, .. } = &event {
                let info = match FileInfo::decode(payload) {
                    Ok(i) => i,
                    Err(e) => break Err(Zx::Protocol(format!("bad ZFILE info: {e}"))),
                };
                match vet_offer(shared, authed, area, folder, &info).await {
                    Ok(inflight) => {
                        rx.set_resume_offset(inflight.data.len() as u32);
                        current = Some(inflight);
                    }
                    Err(reason) => break Err(Zx::Refused(reason)),
                }
            }
        }
        let actions = match rx.advance(event) {
            Ok(a) => a,
            Err(e) => break Err(e.into()),
        };
        let mut finished = false;
        let mut failed = None;
        for action in actions {
            match action {
                RecvAction::SendHeader { header, format } => {
                    if let Err(zx) = wire.send(&header.encode(format)).await {
                        failed = Some(zx);
                        break;
                    }
                }
                RecvAction::OpenFile(_) => {} // staged via `current`
                RecvAction::WriteData { offset, data } => {
                    let Some(cur) = current.as_mut() else {
                        failed = Some(Zx::Protocol("data with no open file".into()));
                        break;
                    };
                    if offset as usize != cur.data.len() {
                        failed = Some(Zx::Protocol("non-contiguous data".into()));
                        break;
                    }
                    if (cur.data.len() + data.len()) as u64 > cur.cap {
                        failed = Some(Zx::Refused("more data than declared".into()));
                        break;
                    }
                    cur.data.extend_from_slice(&data);
                }
                RecvAction::CloseFile => {
                    let Some(done) = current.take() else {
                        failed = Some(Zx::Protocol("close with no open file".into()));
                        break;
                    };
                    results.push(finalize_upload(shared, authed, area, folder, done).await);
                }
                RecvAction::SendOverAndOut => {
                    if let Err(zx) = wire.send(b"OO").await {
                        failed = Some(zx);
                        break;
                    }
                }
                RecvAction::Finished => finished = true,
            }
        }
        if let Some(zx) = failed {
            break Err(zx);
        }
        if finished {
            break Ok(());
        }
    };

    match outcome {
        Ok(()) => {
            // A compliant sender answers our ZFIN with its own "OO"; eat it
            // so it never replays into line mode as a command.
            wire.drain_residue().await;
            let mut out = String::from("\nZMODEM receive complete.\n");
            for line in &results {
                out.push_str(&format!("  {line}\n"));
            }
            if results.is_empty() {
                out.push_str("  (no files were offered)\n");
            }
            t.write_str(&out).await
        }
        Err(zx) => {
            // Park what arrived so a reconnect can resume from the offset.
            let parked = match current.take() {
                Some(cur) if !cur.data.is_empty() => {
                    let at = cur.data.len();
                    shared.zpartials.save(cur.key, cur.data);
                    Some(format!(
                        "{at} byte(s) kept for resume — run zput again to continue"
                    ))
                }
                _ => None,
            };
            for line in &results {
                let _ = t.write_str(&format!("\n  {line}")).await;
            }
            let detail = format!("{area}/{} files={}", folder.unwrap_or(""), results.len());
            finish_failed(t, shared, authed, "zmodem-recv", &detail, zx, parked).await
        }
    }
}

/// Vet one ZFILE offer against the native upload gates. `Ok` carries the
/// in-flight state (staging seeded when a resumable partial exists);
/// `Err` is the refusal reason.
async fn vet_offer(
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    area: &str,
    folder: Option<&str>,
    info: &FileInfo,
) -> Result<InFlight, String> {
    // Strip any path the sender attached; the basename is the offer.
    let name = info
        .name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if name.is_empty()
        || name.len() > 128
        || name == "."
        || name == ".."
        || name.chars().any(char::is_control)
    {
        return Err("that file name is not acceptable".into());
    }
    // No clobbering — the FileService convention every upload path follows.
    let full = match folder {
        Some(f) => format!("{f}/{name}"),
        None => name.clone(),
    };
    match shared.files.node_by_path(area, &full).await {
        Ok(None) => {}
        Ok(Some(_)) => return Err(format!("{name} already exists here")),
        Err(e) => return Err(format!("the file library is unavailable: {e}")),
    }
    // Declared-size cap and the storage quota, checked fast on the declared
    // size (finalize re-checks the actual bytes).
    let declared = info.length;
    if declared.is_some_and(|d| d > MAX_ZPUT_BYTES) {
        return Err("file too large".into());
    }
    let quota = shared.config.read().upload_quota_bytes;
    if quota > 0 {
        let used = shared
            .files
            .uploaded_bytes(authed.account.id)
            .await
            .unwrap_or(0)
            .max(0) as u64;
        if used.saturating_add(declared.unwrap_or(0)) > quota {
            return Err("storage quota exceeded".into());
        }
    }
    // Resume: seed staging when a live partial fits under the declared size.
    let key = partial_key(authed.account.id, area, folder, &name);
    let staged = shared.zpartials.take(&key);
    let data = match declared {
        Some(d) if staged.len() as u64 >= d => Vec::new(), // stale: start over
        _ => staged,
    };
    Ok(InFlight {
        name,
        data,
        cap: declared.unwrap_or(MAX_ZPUT_BYTES).min(MAX_ZPUT_BYTES),
        key,
    })
}

/// Finalize one completed file with the native discipline (the HTXF/native
/// `UploadFinish` gates); returns the caller-facing outcome line.
async fn finalize_upload(
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    area: &str,
    folder: Option<&str>,
    done: InFlight,
) -> String {
    let InFlight { name, data, .. } = done;
    let size = data.len();
    let root = *blake3::hash(&data).as_bytes();
    let detail = format!("{area}/{} name={name} bytes={size}", folder.unwrap_or(""));
    // Hash-deny at the enforcement point: refused bytes are dropped, never
    // parked.
    if shared.moderation.is_denied(&root) {
        audit(
            shared,
            &authed.account.login,
            "zmodem-recv",
            format!("{detail} outcome=denied-hash"),
        );
        return format!("{name}: refused (that content is not allowed here)");
    }
    // Quota re-checked against the actual byte count.
    let quota = shared.config.read().upload_quota_bytes;
    if quota > 0 {
        let used = shared
            .files
            .uploaded_bytes(authed.account.id)
            .await
            .unwrap_or(0)
            .max(0) as u64;
        if used.saturating_add(size as u64) > quota {
            audit(
                shared,
                &authed.account.login,
                "zmodem-recv",
                format!("{detail} outcome=quota"),
            );
            return format!("{name}: refused (storage quota exceeded)");
        }
    }
    let blobs = shared.blobs.clone();
    let blob_id = match tokio::task::spawn_blocking(move || blobs.put(&data)).await {
        Ok(Ok(id)) => id,
        _ => return format!("{name}: the file store is unavailable; try again later"),
    };
    debug_assert_eq!(blob_id.0, root, "blob id is the blake3 of the bytes");
    let uploader = format!("{}@{}", authed.persona.screen_name, shared.origin_name());
    match shared
        .files
        .add_file(
            area,
            folder,
            &name,
            &blob_id.0,
            size as i64,
            "application/octet-stream",
            "",
            "",
            &uploader,
            authed.account.id,
        )
        .await
    {
        Ok(node) => {
            shared.bus.publish(ServerEvent::FileAdded {
                area: area.to_string(),
                id: node.id,
            });
            audit(
                shared,
                &authed.account.login,
                "zmodem-recv",
                format!("{detail} outcome=complete"),
            );
            format!("Received {name} ({size} bytes).")
        }
        Err(e) => {
            audit(
                shared,
                &authed.account.login,
                "zmodem-recv",
                format!("{detail} outcome=not-registered({e})"),
            );
            format!("{name}: not registered ({e})")
        }
    }
}

// ---------------------------------------------------------------------------
// Shared teardown + audit
// ---------------------------------------------------------------------------

/// Common failure teardown: cancel our side when the peer didn't, drain the
/// residue so line mode resumes cleanly, tell the caller, audit the outcome.
async fn finish_failed<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    action: &str,
    detail: &str,
    zx: Zx,
    extra: Option<String>,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut wire = Wire::new(t);
    let (label, message) = match &zx {
        Zx::Io(e) => (format!("io({e})"), None), // the caller is gone
        Zx::Timeout => {
            wire.abort().await;
            (
                "timeout".to_string(),
                Some("Transfer timed out.".to_string()),
            )
        }
        Zx::Cancelled => (
            "cancelled".to_string(),
            Some("Transfer cancelled.".to_string()),
        ),
        Zx::Protocol(e) => {
            wire.abort().await;
            (
                format!("protocol({e})"),
                Some(format!("Transfer failed: {e}.")),
            )
        }
        Zx::Refused(reason) => {
            wire.abort().await;
            (
                format!("refused({reason})"),
                Some(format!("Upload refused: {reason}.")),
            )
        }
    };
    if !matches!(zx, Zx::Io(_)) {
        wire.drain_residue().await;
    }
    audit(
        shared,
        &authed.account.login,
        action,
        format!("{detail} outcome={label}"),
    );
    if let Some(text) = message {
        let mut out = format!("\n{text}\n");
        if let Some(extra) = extra {
            out.push_str(&format!("({extra}.)\n"));
        }
        // Best-effort: the transport may already be down.
        let _ = t.write_str(&out).await;
    }
    Ok(())
}

/// Fire-and-forget audit record, same conventions as the door host.
fn audit(shared: &Arc<Shared>, actor: &str, action: &str, detail: String) {
    let pool = shared.pool.clone();
    let actor = actor.to_string();
    let action = action.to_string();
    tokio::spawn(async move {
        let _ = AuditRepo(&pool).record(&actor, &action, &detail).await;
    });
}
