//! Sans-IO model of the door ↔ remote byte pump: [`BridgeBuffer`] and
//! [`BridgeStats`].
//!
//! A running door produces and consumes raw 8-bit bytes — classic doors emit
//! CP437 box-drawing art, so the pump must be **8-bit clean**: no UTF-8
//! validation, no lossy conversion, every byte value `0x00..=0xFF` passes
//! through intact.
//!
//! For [`IoMode::Socket`] sessions the remote leg is a telnet stream, where
//! `0xFF` ([`IAC`]) is the telnet escape byte. A literal `0xFF` in door
//! output must be **doubled** (`FF FF`) on the way out, and doubled `FF`s
//! from the remote collapsed back to one on the way in. [`BridgeBuffer`]
//! performs exactly this transform (and nothing more — telnet *negotiation*
//! is the transport's job; see below) while keeping per-direction byte
//! counts in a [`BridgeStats`].
//!
//! ## Purity & the process seam
//!
//! Everything here is a pure transform over byte slices: no sockets, no
//! pipes, no clocks. The driving slice (tokio) reads a chunk from one side,
//! calls [`BridgeBuffer::door_to_remote`] or
//! [`BridgeBuffer::remote_to_door`], and writes the output buffer to the
//! other side. Decoding is chunk-boundary safe: an [`IAC`] that ends one
//! chunk is held as pending state and resolved by the next chunk (or by
//! [`BridgeBuffer::finish_remote_to_door`] at stream end). Rates are
//! computed from injected [`Duration`]s — no ambient clock.
//!
//! By the time bytes reach this bridge, the transport layer (the telnet
//! server) has already stripped option negotiation (`IAC WILL/WONT/DO/DONT`,
//! subnegotiation, …); the only telnet artifact crossing the seam is the
//! `0xFF` doubling handled here. A lone `IAC` followed by a non-`IAC` byte
//! is therefore passed through verbatim rather than interpreted.

use std::time::Duration;

use crate::door::IoMode;

/// The telnet *Interpret As Command* escape byte (`0xFF`).
pub const IAC: u8 = 0xFF;

/// Per-session byte accounting for one bridged door session.
///
/// Counts are **payload** (unescaped) bytes in each direction, so they match
/// what the door and the caller actually exchanged, independent of how many
/// extra `IAC` bytes escaping added on the wire. Counters saturate instead
/// of wrapping.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BridgeStats {
    /// Payload bytes pumped door → remote (door output).
    pub door_to_remote: u64,
    /// Payload bytes pumped remote → door (caller keystrokes, uploads).
    pub remote_to_door: u64,
}

impl BridgeStats {
    /// Total payload bytes in both directions.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.door_to_remote.saturating_add(self.remote_to_door)
    }

    /// Average door → remote rate in bytes/second over the injected
    /// `elapsed` wall time (`0.0` when `elapsed` is zero).
    #[must_use]
    pub fn door_to_remote_bps(&self, elapsed: Duration) -> f64 {
        bps(self.door_to_remote, elapsed)
    }

    /// Average remote → door rate in bytes/second over the injected
    /// `elapsed` wall time (`0.0` when `elapsed` is zero).
    #[must_use]
    pub fn remote_to_door_bps(&self, elapsed: Duration) -> f64 {
        bps(self.remote_to_door, elapsed)
    }

    /// Average combined rate in bytes/second over the injected `elapsed`
    /// wall time (`0.0` when `elapsed` is zero).
    #[must_use]
    pub fn total_bps(&self, elapsed: Duration) -> f64 {
        bps(self.total(), elapsed)
    }
}

/// `bytes / elapsed` in bytes per second, `0.0` for a zero duration.
fn bps(bytes: u64, elapsed: Duration) -> f64 {
    let secs = elapsed.as_secs_f64();
    if secs > 0.0 {
        bytes as f64 / secs
    } else {
        0.0
    }
}

/// The sans-IO byte pump for one door session.
///
/// Construct one per session from the door's [`IoMode`]:
///
/// * [`IoMode::Stdio`] — both directions are pure CP437-safe passthrough.
/// * [`IoMode::Socket`] — door → remote doubles [`IAC`] bytes; remote → door
///   collapses doubled [`IAC`]s, carrying a trailing `IAC` across chunk
///   boundaries.
///
/// ```
/// use rabbithole_legacy_doors::{BridgeBuffer, IoMode};
///
/// let mut bridge = BridgeBuffer::new(IoMode::Socket);
/// let mut wire = Vec::new();
/// bridge.door_to_remote(&[0xB0, 0xFF, 0xB2], &mut wire);
/// assert_eq!(wire, [0xB0, 0xFF, 0xFF, 0xB2]); // IAC doubled
///
/// let mut back = Vec::new();
/// bridge.remote_to_door(&wire, &mut back);
/// assert_eq!(back, [0xB0, 0xFF, 0xB2]); // and collapsed again
/// assert_eq!(bridge.stats().door_to_remote, 3);
/// assert_eq!(bridge.stats().remote_to_door, 3);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BridgeBuffer {
    escape_iac: bool,
    /// A chunk ended in a lone `IAC`; its meaning is decided by the next
    /// remote → door chunk (or by `finish_remote_to_door`).
    pending_iac: bool,
    stats: BridgeStats,
}

impl BridgeBuffer {
    /// A bridge configured for `mode` ([`IoMode::Socket`] turns on IAC
    /// escaping).
    #[must_use]
    pub fn new(mode: IoMode) -> Self {
        BridgeBuffer {
            escape_iac: mode == IoMode::Socket,
            pending_iac: false,
            stats: BridgeStats::default(),
        }
    }

    /// Whether this bridge doubles/collapses [`IAC`] bytes.
    #[must_use]
    pub fn escapes_iac(&self) -> bool {
        self.escape_iac
    }

    /// Byte counts so far (payload bytes, both directions).
    #[must_use]
    pub fn stats(&self) -> BridgeStats {
        self.stats
    }

    /// Whether a remote → door chunk ended in an unresolved lone [`IAC`].
    #[must_use]
    pub fn has_pending_iac(&self) -> bool {
        self.pending_iac
    }

    /// Transform one chunk of door output for the remote side, appending to
    /// `out`. Passthrough in stdio mode; doubles every [`IAC`] in socket
    /// mode. Returns the number of bytes appended.
    pub fn door_to_remote(&mut self, input: &[u8], out: &mut Vec<u8>) -> usize {
        let before = out.len();
        if self.escape_iac {
            for run in input.split_inclusive(|&b| b == IAC) {
                out.extend_from_slice(run);
                if run.last() == Some(&IAC) {
                    out.push(IAC);
                }
            }
        } else {
            out.extend_from_slice(input);
        }
        self.stats.door_to_remote = self.stats.door_to_remote.saturating_add(input.len() as u64);
        out.len() - before
    }

    /// Transform one chunk of remote input for the door, appending to `out`.
    /// Passthrough in stdio mode; collapses doubled [`IAC`]s in socket mode,
    /// holding a chunk-final lone [`IAC`] as pending state for the next call
    /// (see [`finish_remote_to_door`](Self::finish_remote_to_door) for
    /// stream end). Returns the number of bytes appended.
    pub fn remote_to_door(&mut self, input: &[u8], out: &mut Vec<u8>) -> usize {
        let before = out.len();
        if self.escape_iac {
            for &byte in input {
                if self.pending_iac {
                    self.pending_iac = false;
                    if byte == IAC {
                        // IAC IAC — one literal 0xFF of payload.
                        out.push(IAC);
                    } else {
                        // Lone IAC + other byte: negotiation was already
                        // stripped upstream, so pass both through verbatim.
                        out.push(IAC);
                        out.push(byte);
                    }
                } else if byte == IAC {
                    self.pending_iac = true;
                } else {
                    out.push(byte);
                }
            }
        } else {
            out.extend_from_slice(input);
        }
        let produced = out.len() - before;
        self.stats.remote_to_door = self.stats.remote_to_door.saturating_add(produced as u64);
        produced
    }

    /// Flush decoder state at remote-stream end: a still-pending lone
    /// [`IAC`] is emitted as a literal byte. Returns the number of bytes
    /// appended (`0` or `1`).
    pub fn finish_remote_to_door(&mut self, out: &mut Vec<u8>) -> usize {
        if self.pending_iac {
            self.pending_iac = false;
            out.push(IAC);
            self.stats.remote_to_door = self.stats.remote_to_door.saturating_add(1);
            1
        } else {
            0
        }
    }
}
