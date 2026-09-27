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
//!   clobbering), checkpoint subpackets to private disk staging under the
//!   declared-size/quota caps, and on `ZEOF` finalize with the native
//!   upload discipline — blake3, the moderation hash-deny list, quota
//!   re-checked on actual bytes, content-addressed blob commit,
//!   target-conditional insertion + [`ServerEvent::FileAdded`]. Batches work: each `ZFILE`
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
//! - **Uploads**: interrupted `zput` staging is checkpointed to private disk files
//!   with validated target metadata and a 30-minute TTL,
//!   keyed by `(account, area, folder, name)`. When the same account
//!   re-offers the same destination, the server arms
//!   [`Receiver::set_resume_offset`] and answers the `ZFILE` with
//!   `ZRPOS(staged)`; the client seeks and sends only the tail. Each CRC-validated
//!   subpacket is durable before acknowledgement, including across restarts.
//!
//! ## Teardown discipline
//!
//! Every exit path returns the session to a usable line-mode prompt: on
//! our aborts (timeout, protocol error, refusal) the classic cancel
//! sequence (eight CANs + eight backspaces) is sent, and either way any
//! in-flight transfer residue is drained (bounded quiet-wait) so stray
//! frames never replay into `read_line` as garbage commands.

use std::collections::VecDeque;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

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

use crate::legacy_staging::{Lease, Offer, Protocol, Target};
use crate::Shared;

/// Idle budget for one read or write during a transfer; a peer that goes
/// quiet longer than this gets the session aborted (and, for uploads, its
/// staging parked for resume).
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Hard ceiling on one `zput` upload's staged bytes (ZMODEM offsets are
/// 32-bit anyway; larger files belong on the native transfer path). Matches
/// the Hotline HTXF in-memory staging bound.
const MAX_ZPUT_BYTES: u64 = 64 * 1024 * 1024;

/// Consecutive CANs from the peer that abort the session (the spec's five).
const CANCEL_CANS: u32 = 5;

/// A header cannot grow indefinitely or keep an operation alive with noise.
const MAX_HEADER_SCAN: usize = 16 * 1024;
const MAX_HEADER_BUFFER: usize = 1024;
/// Total pre-data policy refusals in a batch, independently of corruption.
/// Successful files do not replenish this cap or permit unbounded skip text.
const MAX_DECLINED_OFFERS: usize = 8;

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
// Transfer-level errors
// ---------------------------------------------------------------------------

/// Why a transfer stopped before `Finished`.
enum Zx {
    /// The transport failed or hit EOF (the caller is likely gone).
    Io(io::Error),
    /// The peer went quiet past [`IDLE_TIMEOUT`].
    Timeout,
    /// The peer struck CANs (or sent ZABORT/ZFERR).
    Cancelled,
    /// One malformed header was discarded; the state machine may request it again.
    BadHeader,
    /// An unrecoverable error or exhausted recovery budget.
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

    /// The next header, skipping bounded line noise/trailers and honoring
    /// cancels. A malformed candidate is reported for bounded retransmission.
    async fn next_header(&mut self) -> Result<DecodedHeader, Zx> {
        self.next_header_with_budget(IDLE_TIMEOUT).await
    }

    async fn next_header_with_budget(&mut self, budget: Duration) -> Result<DecodedHeader, Zx> {
        tokio::time::timeout(budget, self.read_header())
            .await
            .map_err(|_| Zx::Timeout)?
    }

    async fn read_header(&mut self) -> Result<DecodedHeader, Zx> {
        let mut scanned = 0;
        loop {
            let before = self.buf.len();
            self.skip_to_zpad()?;
            scanned += before - self.buf.len();
            if scanned > MAX_HEADER_SCAN {
                return Err(Zx::Protocol("header scan limit reached".into()));
            }
            if self.buf.is_empty() {
                self.refill().await?;
                continue;
            }
            match decode_header(&self.buf) {
                Ok(decoded) => {
                    self.buf.drain(..decoded.consumed);
                    return Ok(decoded);
                }
                Err(HeaderError::Incomplete) if self.buf.len() < MAX_HEADER_BUFFER => {
                    self.refill().await?
                }
                Err(HeaderError::Cancelled) => return Err(Zx::Cancelled),
                // Shed the entire pad prefix, once, then let the next call
                // resynchronize. Retain following bytes: a valid next header
                // may already be in this same read. Each fault spends a retry.
                Err(_) => {
                    let pads = self.buf.iter().take_while(|&&b| b == ZPAD).count();
                    self.buf.drain(..pads.max(1));
                    return Err(Zx::BadHeader);
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

    /// A receiver must still answer a retransmission request for its final
    /// ZFIN. Keep this within the existing quiet/total cleanup bounds; OO,
    /// disconnect or quiet ends cleanup without delaying the shell forever.
    async fn finish_receive(&mut self, rx: &mut Receiver) -> Result<(), Zx> {
        tokio::time::timeout(DRAIN_MAX, async {
            loop {
                if self.buf.windows(2).any(|bytes| bytes == b"OO") {
                    self.buf.clear();
                    return Ok(());
                }
                self.skip_to_zpad()?;
                if !self.buf.is_empty() {
                    let actions = match decode_header(&self.buf) {
                        Ok(decoded) => {
                            self.buf.drain(..decoded.consumed);
                            if matches!(
                                decoded.header.frame_type,
                                FrameType::Znak | FrameType::Zfin
                            ) {
                                rx.advance(RecvEvent::Header(decoded.header))?
                            } else {
                                vec![]
                            }
                        }
                        Err(HeaderError::Cancelled) => return Err(Zx::Cancelled),
                        Err(HeaderError::Incomplete) if self.buf.len() < MAX_HEADER_BUFFER => {
                            vec![]
                        }
                        Err(_) => {
                            let pads = self.buf.iter().take_while(|&&b| b == ZPAD).count();
                            self.buf.drain(..pads.max(1));
                            rx.corrupt_header()?
                        }
                    };
                    for action in actions {
                        if let RecvAction::SendHeader { header, format } = action {
                            self.send(&header.encode(format)).await?;
                        }
                    }
                    if !self.buf.is_empty()
                        && !matches!(decode_header(&self.buf), Err(HeaderError::Incomplete))
                    {
                        continue;
                    }
                }
                match tokio::time::timeout(DRAIN_QUIET, self.t.read_binary()).await {
                    Ok(Ok(Some(bytes))) => self.buf.extend_from_slice(&bytes),
                    _ => return Ok(()),
                }
            }
        })
        .await
        .unwrap_or(Ok(()))
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
        Ok(skipped) => {
            // The receiver's own trailing "OO" (this codec's receiver sends
            // one) must not replay into line mode as a command.
            wire.drain_residue().await;
            if !skipped {
                if let Err(e) = shared.files.record_download(target.id).await {
                    tracing::warn!("zmodem download counter failed: {e}");
                }
            }
            audit(
                shared,
                &authed.account.login,
                "zmodem-send",
                format!(
                    "{detail} outcome={}",
                    if skipped { "skipped" } else { "complete" }
                ),
            );
            t.write_str(if skipped {
                "\nZMODEM file skipped by receiver.\n"
            } else {
                "\nZMODEM send complete.\n"
            })
            .await
        }
        Err(zx) => finish_failed(t, shared, authed, "zmodem-send", &detail, zx, None).await,
    }
}

/// Drive the codec [`Sender`] to completion over the wire.
async fn drive_send<S>(wire: &mut Wire<'_, S>, mut tx: Sender, bytes: &[u8]) -> Result<bool, Zx>
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
                SendAction::Finished => return Ok(tx.skipped()),
            }
        }
        let decoded = match wire.next_header().await {
            Ok(decoded) => decoded,
            Err(Zx::BadHeader) => {
                pending.extend(tx.corrupt_header()?);
                continue;
            }
            Err(error) => return Err(error),
        };
        if matches!(
            decoded.header.frame_type,
            FrameType::Zabort | FrameType::Zferr | FrameType::Zcan
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
    /// Staged bytes so far (seeded from the validated durable prefix).
    data: Vec<u8>,
    /// Per-file byte ceiling (declared size capped by [`MAX_ZPUT_BYTES`]).
    cap: u64,
    /// Exclusive destination claim, retained across final publication.
    lease: Lease,
    target: Target,
    /// False after a checkpoint error: never promise that failed IO was saved.
    durable: bool,
    /// Whether the sender declared a length (else `cap` is the ceiling).
    declared: bool,
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
    let mut declined = 0;

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
                Err(Zx::BadHeader) => match rx.corrupt_header() {
                    Ok(actions) => {
                        let mut failed = None;
                        for action in actions {
                            if let RecvAction::SendHeader { header, format } = action {
                                if let Err(error) = wire.send(&header.encode(format)).await {
                                    failed = Some(error);
                                    break;
                                }
                            }
                        }
                        if let Some(error) = failed {
                            break Err(error);
                        }
                        continue;
                    }
                    Err(error) => break Err(error.into()),
                },
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
                    Err(OfferError::Declined(reason)) => {
                        declined += 1;
                        if declined > MAX_DECLINED_OFFERS {
                            break Err(Zx::Refused("too many declined file offers".into()));
                        }
                        results.push(format!("Skipped: {reason}"));
                        let actions = match rx.decline_file() {
                            Ok(actions) => actions,
                            Err(error) => break Err(error.into()),
                        };
                        let mut failed = None;
                        for action in actions {
                            if let RecvAction::SendHeader { header, format } = action {
                                if let Err(error) = wire.send(&header.encode(format)).await {
                                    failed = Some(error);
                                    break;
                                }
                            }
                        }
                        if let Some(error) = failed {
                            break Err(error);
                        }
                        continue;
                    }
                    Err(OfferError::Fatal(reason)) => break Err(Zx::Refused(reason)),
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
                        // With no declared length the ceiling is the burrow's
                        // limit: the file is too big, and nothing of it is
                        // kept for a resume that could only fail again.
                        failed = Some(Zx::Refused(if cur.declared {
                            "more data than declared".into()
                        } else {
                            crate::upload_gate::Refusal::TooBig {
                                max: crate::upload_gate::file_ceiling(shared, MAX_ZPUT_BYTES),
                            }
                            .line()
                        }));
                        if let Err(error) = cur.lease.discard().await {
                            tracing::warn!(%error, "cannot discard refused upload checkpoint");
                        }
                        current = None;
                        break;
                    }
                    // CRC verification preceded this action. Commit before any
                    // later SendHeader ACK, so acknowledged bytes survive a kill.
                    if let Err(error) = cur.lease.append(offset as u64, &data).await {
                        tracing::warn!(%error, "upload checkpoint failed");
                        cur.durable = false;
                        failed = Some(Zx::Refused(
                            "upload staging is unavailable; try again later".into(),
                        ));
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
            break wire.finish_receive(&mut rx).await;
        }
    };

    match outcome {
        Ok(()) => {
            // A compliant sender answers our ZFIN with its own "OO"; eat it
            // so it never replays into line mode as a command.
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
                Some(cur) if cur.durable && !cur.data.is_empty() => {
                    let at = cur.data.len();
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
enum OfferError {
    Declined(String),
    Fatal(String),
}

async fn vet_offer(
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    area: &str,
    folder: Option<&str>,
    info: &FileInfo,
) -> Result<InFlight, OfferError> {
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
        return Err(OfferError::Declined(
            "that file name is not acceptable".into(),
        ));
    }
    // No clobbering — the FileService convention every upload path follows.
    let full = match folder {
        Some(f) => format!("{f}/{name}"),
        None => name.clone(),
    };
    match shared.files.node_by_path(area, &full).await {
        Ok(None) => {}
        Ok(Some(_)) => return Err(OfferError::Declined(format!("{name} already exists here"))),
        Err(e) => {
            return Err(OfferError::Fatal(format!(
                "the file library is unavailable: {e}"
            )))
        }
    }
    // Declared-size cap and the storage quota, checked fast on the declared
    // size (finalize re-checks the actual bytes).
    let declared = info.length;
    if declared.is_some_and(|d| d > MAX_ZPUT_BYTES) {
        return Err(OfferError::Declined("file too large".into()));
    }
    if let Err(refused) =
        crate::upload_gate::check(shared, authed.account.id, declared.unwrap_or(0)).await
    {
        return Err(match refused {
            crate::upload_gate::Refusal::Unavailable => OfferError::Fatal(refused.line()),
            _ => OfferError::Declined(refused.line()),
        });
    }
    // Bind the durable target to canonical database IDs. A replaced folder
    // at the same visible path cannot inherit the previous folder's prefix.
    let canonical = shared
        .files
        .area(area)
        .await
        .map_err(|e| OfferError::Fatal(e.to_string()))?;
    let parent = match folder {
        Some(path) => {
            let node = shared
                .files
                .node_by_path(&canonical.slug, path)
                .await
                .map_err(|e| OfferError::Fatal(e.to_string()))?
                .filter(|node| node.kind == rabbithole_server_core::files::KIND_FOLDER)
                .ok_or_else(|| OfferError::Declined("destination folder is unavailable".into()))?;
            Some(node.id)
        }
        None => None,
    };
    let cap = declared
        .unwrap_or(MAX_ZPUT_BYTES)
        .min(crate::upload_gate::file_ceiling(shared, MAX_ZPUT_BYTES));
    let target = Target {
        protocol: Protocol::Zmodem,
        account: authed.account.id,
        area: canonical.id,
        parent,
        name: name.clone(),
    };
    let (lease, data) = shared
        .upload_staging
        .claim(
            target.clone(),
            Offer {
                length: info.length,
                mtime: info.mtime,
            },
            cap,
        )
        .await
        .map_err(|error| {
            tracing::warn!(%error, "cannot claim upload checkpoint");
            OfferError::Declined("upload staging unavailable or destination already active".into())
        })?;
    Ok(InFlight {
        name,
        data,
        cap,
        lease,
        target,
        durable: true,
        declared: declared.is_some(),
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
    let lease = done.lease.clone();
    let result = finalize_upload_inner(shared, authed, area, folder, done).await;
    match result {
        Ok(message) => {
            if let Err(error) = lease.discard().await {
                tracing::warn!(%error, "cannot clean completed upload checkpoint");
            }
            message
        }
        Err(message) => message,
    }
}

async fn finalize_upload_inner(
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    area: &str,
    folder: Option<&str>,
    done: InFlight,
) -> Result<String, String> {
    let InFlight {
        name, data, target, ..
    } = done;
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
        return Ok(format!(
            "{name}: refused (that content is not allowed here)"
        ));
    }
    // The largest file and the account's space, re-checked against the
    // actual byte count, and held until the file is recorded.
    let _commit = crate::upload_gate::commit_lock(shared).await;
    if let Err(refused) = crate::upload_gate::check(shared, authed.account.id, size as u64).await {
        let (outcome, said) = match refused {
            crate::upload_gate::Refusal::TooBig { .. } => ("too-big", "file too large"),
            crate::upload_gate::Refusal::OverQuota { .. } => ("quota", "storage quota exceeded"),
            crate::upload_gate::Refusal::Unavailable => ("unavailable", "try again later"),
        };
        audit(
            shared,
            &authed.account.login,
            "zmodem-recv",
            format!("{detail} outcome={outcome}"),
        );
        let message = format!("{name}: refused ({said})");
        return if matches!(refused, crate::upload_gate::Refusal::Unavailable) {
            Err(message)
        } else {
            Ok(message)
        };
    }
    let blobs = shared.blobs.clone();
    let blob_id = match tokio::task::spawn_blocking(move || blobs.put(&data)).await {
        Ok(Ok(id)) => id,
        _ => {
            return Err(format!(
                "{name}: the file store is unavailable; try again later"
            ))
        }
    };
    debug_assert_eq!(blob_id.0, root, "blob id is the blake3 of the bytes");
    let uploader = format!("{}@{}", authed.persona.screen_name, shared.origin_name());
    match shared
        .files
        // Validate the captured database identities and original placement in
        // the insertion itself: quota/blob awaits must not redirect the upload
        // to a newly created folder at the same path.
        .add_file_if_target(
            area,
            folder,
            target.area,
            target.parent,
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
        Ok(Some(id)) => {
            shared.bus.publish(ServerEvent::FileAdded {
                area: area.to_string(),
                id,
            });
            audit(
                shared,
                &authed.account.login,
                "zmodem-recv",
                format!("{detail} outcome=complete"),
            );
            Ok(format!("Received {name} ({size} bytes)."))
        }
        Ok(None) => {
            audit(
                shared,
                &authed.account.login,
                "zmodem-recv",
                format!("{detail} outcome=destination-changed-or-occupied"),
            );
            Ok(format!(
                "{name}: refused (destination changed during upload or file already exists)"
            ))
        }
        Err(e) => {
            audit(
                shared,
                &authed.account.login,
                "zmodem-recv",
                format!("{detail} outcome=not-registered({e})"),
            );
            let message = format!("{name}: not registered ({e})");
            if matches!(e, rabbithole_server_core::FileError::Exists) {
                Ok(message)
            } else {
                Err(message)
            }
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
        Zx::BadHeader => {
            wire.abort().await;
            (
                "bad-header".to_string(),
                Some("Transfer failed: invalid header.".to_string()),
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

#[cfg(test)]
#[path = "zmodem/recovery_tests.rs"]
mod recovery_tests;

#[cfg(test)]
mod staging_tests;
