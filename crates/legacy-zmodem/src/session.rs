//! Sans-IO session sketch: happy-path ZMODEM send/receive state machines.
//!
//! This module is deliberately a *sketch* — just enough structure for the
//! telnet integration slice to drive a straightforward transfer. The state
//! machines own no sockets and do no encoding: the caller decodes wire
//! bytes into [`RecvEvent`]/[`SendEvent`]s (via [`crate::header`] and
//! [`crate::subpacket`]) and performs the returned actions.
//!
//! ## Happy-path receive flow
//!
//! ```text
//!   sender                       receiver ([`Receiver`])
//!   ZRQINIT          ------>
//!                    <------     ZRINIT (CANFDX|CANOVIO|CANFC32)
//!   ZFILE + info     ------>
//!                    <------     ZRPOS(0)
//!   ZDATA(0)         ------>
//!   subpacket ZCRCG  ------>     (write bytes)
//!   subpacket ZCRCE  ------>     (write bytes)
//!   ZEOF(n)          ------>
//!                    <------     ZRINIT            (ready for next file)
//!   ZFIN             ------>
//!                    <------     ZFIN, then "OO"   (over and out)
//! ```
//!
//! The [`Sender`] mirrors the same flow from the other side; during
//! `Streaming` the *caller* pumps `ZDATA` subpackets (it owns the file
//! bytes) and reports [`SendEvent::DataExhausted`] to trigger `ZEOF`.
//!
//! ## Deliberately deferred (future slices)
//!
//! - Full error recovery: `ZNAK`/garbled-header retries, retry limits and
//!   timeouts, `ZRPOS` storm damping, `Attn` sequences from `ZSINIT`.
//! - `ZSKIP`/`ZCRC` file-exists and crash-recovery negotiation, `ZFREECNT`,
//!   `ZCHALLENGE`, `ZCOMMAND`.
//! - Multi-file batches on the send side (the receiver already loops back
//!   to `AwaitingFile` after each `ZEOF`).
//! - Escape-policy negotiation (`ESCCTL`/`ESC8`) and `ZCRCQ`-window pacing.
//!
//! Out-of-order but well-formed events yield
//! [`SessionError::UnexpectedEvent`] rather than silent misbehaviour, and
//! a stale `ZDATA`/`ZEOF` position is answered with a corrective `ZRPOS`
//! (the seed of real recovery).

use thiserror::Error;

use crate::header::{FrameType, Header, HeaderFormat, CANFC32, CANFDX, CANOVIO};
use crate::zdle::FrameEnd;
use crate::zfile::{FileInfo, FileInfoError};

/// Errors from the session state machines.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SessionError {
    /// An event arrived that this state cannot handle (see module docs on
    /// deferred recovery).
    #[error("unexpected {event} in state {state}")]
    UnexpectedEvent {
        /// Description of the current state.
        state: &'static str,
        /// Description of the offending event.
        event: &'static str,
    },
    /// A ZFILE info payload failed to parse.
    #[error("bad ZFILE info: {0}")]
    BadFileInfo(#[from] FileInfoError),
}

// ---------------------------------------------------------------------------
// Receiver
// ---------------------------------------------------------------------------

/// Receive-side states for the happy path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvState {
    /// Waiting for the sender's `ZRQINIT`.
    AwaitingInit,
    /// Sent `ZRINIT`; waiting for `ZFILE` (or `ZFIN`).
    AwaitingFile,
    /// Got a `ZFILE` header; waiting for its info subpacket.
    AwaitingFileInfo,
    /// Sent `ZRPOS`; waiting for `ZDATA`/`ZEOF` at `offset`.
    AwaitingData {
        /// The next file offset we expect.
        offset: u32,
    },
    /// Inside a `ZDATA` frame, consuming subpackets at `offset`.
    ReceivingData {
        /// The next file offset we expect.
        offset: u32,
    },
    /// Session finished (`ZFIN` exchanged, "OO" sent).
    Done,
}

impl RecvState {
    fn name(self) -> &'static str {
        match self {
            RecvState::AwaitingInit => "AwaitingInit",
            RecvState::AwaitingFile => "AwaitingFile",
            RecvState::AwaitingFileInfo => "AwaitingFileInfo",
            RecvState::AwaitingData { .. } => "AwaitingData",
            RecvState::ReceivingData { .. } => "ReceivingData",
            RecvState::Done => "Done",
        }
    }
}

/// Decoded input for the receiver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecvEvent {
    /// A header arrived (decode with [`crate::header::decode_header`]).
    Header(Header),
    /// A data subpacket arrived (decode with
    /// [`crate::subpacket::decode_subpacket`]).
    Data {
        /// The unescaped payload.
        payload: Vec<u8>,
        /// How the subpacket was terminated.
        end: FrameEnd,
    },
}

impl RecvEvent {
    fn name(&self) -> &'static str {
        match self {
            RecvEvent::Header(_) => "header",
            RecvEvent::Data { .. } => "data subpacket",
        }
    }
}

/// What the receiver's driver must do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecvAction {
    /// Encode and transmit this header in the given format.
    SendHeader {
        /// The header to send.
        header: Header,
        /// The wire format to use.
        format: HeaderFormat,
    },
    /// Open (create/truncate) the described file for writing.
    OpenFile(FileInfo),
    /// Append these bytes at the given file offset.
    WriteData {
        /// Offset of the first byte.
        offset: u32,
        /// The bytes to write.
        data: Vec<u8>,
    },
    /// Close the file opened by the last [`RecvAction::OpenFile`].
    CloseFile,
    /// Transmit the literal `"OO"` (over-and-out) bytes.
    SendOverAndOut,
    /// The session is complete; tear down.
    Finished,
}

/// Happy-path ZMODEM receiver (see module docs for the flow and what is
/// deferred).
#[derive(Debug)]
pub struct Receiver {
    state: RecvState,
}

impl Default for Receiver {
    fn default() -> Self {
        Self::new()
    }
}

impl Receiver {
    /// A receiver waiting for the sender's opening `ZRQINIT`.
    pub fn new() -> Self {
        Receiver {
            state: RecvState::AwaitingInit,
        }
    }

    /// The current state, for logging/inspection.
    pub fn state(&self) -> RecvState {
        self.state
    }

    /// The `ZRINIT` this receiver advertises: full-duplex, overlapped I/O,
    /// 32-bit CRC, no buffer-size limit (streaming).
    fn zrinit() -> RecvAction {
        RecvAction::SendHeader {
            header: Header::with_flags(FrameType::Zrinit, 0, 0, 0, CANFDX | CANOVIO | CANFC32),
            format: HeaderFormat::Hex,
        }
    }

    /// Feed one event; get the actions the driver must perform, in order.
    pub fn advance(&mut self, event: RecvEvent) -> Result<Vec<RecvAction>, SessionError> {
        let unexpected = SessionError::UnexpectedEvent {
            state: self.state.name(),
            event: event.name(),
        };
        match (self.state, event) {
            (RecvState::AwaitingInit, RecvEvent::Header(h))
                if h.frame_type == FrameType::Zrqinit =>
            {
                self.state = RecvState::AwaitingFile;
                Ok(vec![Self::zrinit()])
            }
            // Senders may repeat ZRQINIT until our ZRINIT lands.
            (RecvState::AwaitingFile, RecvEvent::Header(h))
                if h.frame_type == FrameType::Zrqinit =>
            {
                Ok(vec![Self::zrinit()])
            }
            (RecvState::AwaitingFile, RecvEvent::Header(h)) if h.frame_type == FrameType::Zfile => {
                self.state = RecvState::AwaitingFileInfo;
                Ok(vec![])
            }
            (RecvState::AwaitingFile, RecvEvent::Header(h)) if h.frame_type == FrameType::Zfin => {
                self.state = RecvState::Done;
                Ok(vec![
                    RecvAction::SendHeader {
                        header: Header::new(FrameType::Zfin),
                        format: HeaderFormat::Hex,
                    },
                    RecvAction::SendOverAndOut,
                    RecvAction::Finished,
                ])
            }
            (RecvState::AwaitingFileInfo, RecvEvent::Data { payload, .. }) => {
                let info = FileInfo::decode(&payload)?;
                self.state = RecvState::AwaitingData { offset: 0 };
                Ok(vec![
                    RecvAction::OpenFile(info),
                    RecvAction::SendHeader {
                        header: Header::with_pos(FrameType::Zrpos, 0),
                        format: HeaderFormat::Hex,
                    },
                ])
            }
            (RecvState::AwaitingData { offset }, RecvEvent::Header(h))
                if h.frame_type == FrameType::Zdata =>
            {
                if h.pos() == offset {
                    self.state = RecvState::ReceivingData { offset };
                    Ok(vec![])
                } else {
                    // Position mismatch: re-anchor the sender. (Full ZRPOS
                    // storm handling is deferred; see module docs.)
                    Ok(vec![RecvAction::SendHeader {
                        header: Header::with_pos(FrameType::Zrpos, offset),
                        format: HeaderFormat::Hex,
                    }])
                }
            }
            (RecvState::AwaitingData { offset }, RecvEvent::Header(h))
                if h.frame_type == FrameType::Zeof =>
            {
                if h.pos() == offset {
                    self.state = RecvState::AwaitingFile;
                    Ok(vec![RecvAction::CloseFile, Self::zrinit()])
                } else {
                    Ok(vec![RecvAction::SendHeader {
                        header: Header::with_pos(FrameType::Zrpos, offset),
                        format: HeaderFormat::Hex,
                    }])
                }
            }
            (RecvState::ReceivingData { offset }, RecvEvent::Data { payload, end }) => {
                let new_offset = offset.wrapping_add(payload.len() as u32);
                let mut actions = vec![RecvAction::WriteData {
                    offset,
                    data: payload,
                }];
                match end {
                    FrameEnd::Zcrcg => {
                        self.state = RecvState::ReceivingData { offset: new_offset };
                    }
                    FrameEnd::Zcrce => {
                        self.state = RecvState::AwaitingData { offset: new_offset };
                    }
                    FrameEnd::Zcrcq => {
                        self.state = RecvState::ReceivingData { offset: new_offset };
                        actions.push(RecvAction::SendHeader {
                            header: Header::with_pos(FrameType::Zack, new_offset),
                            format: HeaderFormat::Hex,
                        });
                    }
                    FrameEnd::Zcrcw => {
                        self.state = RecvState::AwaitingData { offset: new_offset };
                        actions.push(RecvAction::SendHeader {
                            header: Header::with_pos(FrameType::Zack, new_offset),
                            format: HeaderFormat::Hex,
                        });
                    }
                }
                Ok(actions)
            }
            _ => Err(unexpected),
        }
    }
}

// ---------------------------------------------------------------------------
// Sender
// ---------------------------------------------------------------------------

/// Send-side states for the happy path (single file).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendState {
    /// Not started; call [`Sender::start`].
    Start,
    /// Sent `ZRQINIT`; waiting for the receiver's `ZRINIT`.
    AwaitingRinit,
    /// Sent `ZFILE` + info; waiting for `ZRPOS` (or `ZSKIP`, deferred).
    AwaitingFileAck,
    /// The caller is streaming `ZDATA` subpackets from `offset`.
    Streaming {
        /// Where the current data frame started.
        offset: u32,
    },
    /// Sent `ZEOF`; waiting for the receiver's next `ZRINIT`.
    AwaitingEofAck,
    /// Sent `ZFIN`; waiting for the receiver's `ZFIN`.
    AwaitingFinAck,
    /// Session finished.
    Done,
}

impl SendState {
    fn name(self) -> &'static str {
        match self {
            SendState::Start => "Start",
            SendState::AwaitingRinit => "AwaitingRinit",
            SendState::AwaitingFileAck => "AwaitingFileAck",
            SendState::Streaming { .. } => "Streaming",
            SendState::AwaitingEofAck => "AwaitingEofAck",
            SendState::AwaitingFinAck => "AwaitingFinAck",
            SendState::Done => "Done",
        }
    }
}

/// Decoded input for the sender.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendEvent {
    /// A header arrived from the receiver.
    Header(Header),
    /// The caller has streamed all file data up to `offset` and wants to
    /// close the frame with `ZEOF`.
    DataExhausted {
        /// The file offset one past the last byte sent.
        offset: u32,
    },
}

impl SendEvent {
    fn name(&self) -> &'static str {
        match self {
            SendEvent::Header(_) => "header",
            SendEvent::DataExhausted { .. } => "data exhausted",
        }
    }
}

/// What the sender's driver must do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendAction {
    /// Encode and transmit this header in the given format.
    SendHeader {
        /// The header to send.
        header: Header,
        /// The wire format to use.
        format: HeaderFormat,
    },
    /// Transmit the ZFILE info block as a `ZCRCW` subpacket.
    SendFileInfo(FileInfo),
    /// Stream file data from this offset as `ZDATA` subpackets, then feed
    /// [`SendEvent::DataExhausted`].
    StreamData {
        /// The offset the receiver asked to start (or resume) from.
        from: u32,
    },
    /// Transmit the literal `"OO"` (over-and-out) bytes.
    SendOverAndOut,
    /// The session is complete; tear down.
    Finished,
}

/// Happy-path single-file ZMODEM sender sketch.
#[derive(Debug)]
pub struct Sender {
    state: SendState,
    info: FileInfo,
    /// Whether the receiver advertised CANFC32 (drives header/CRC width).
    peer_can_fc32: bool,
}

impl Sender {
    /// A sender that will offer `info` once started.
    pub fn new(info: FileInfo) -> Self {
        Sender {
            state: SendState::Start,
            info,
            peer_can_fc32: false,
        }
    }

    /// The current state, for logging/inspection.
    pub fn state(&self) -> SendState {
        self.state
    }

    /// Whether the receiver can check 32-bit CRCs (valid after `ZRINIT`).
    pub fn peer_can_fc32(&self) -> bool {
        self.peer_can_fc32
    }

    fn binary_format(&self) -> HeaderFormat {
        if self.peer_can_fc32 {
            HeaderFormat::Bin32
        } else {
            HeaderFormat::Bin16
        }
    }

    /// Kick off the session: emits `ZRQINIT`.
    pub fn start(&mut self) -> Result<Vec<SendAction>, SessionError> {
        if self.state != SendState::Start {
            return Err(SessionError::UnexpectedEvent {
                state: self.state.name(),
                event: "start",
            });
        }
        self.state = SendState::AwaitingRinit;
        Ok(vec![SendAction::SendHeader {
            header: Header::new(FrameType::Zrqinit),
            format: HeaderFormat::Hex,
        }])
    }

    /// Feed one event; get the actions the driver must perform, in order.
    pub fn advance(&mut self, event: SendEvent) -> Result<Vec<SendAction>, SessionError> {
        let unexpected = SessionError::UnexpectedEvent {
            state: self.state.name(),
            event: event.name(),
        };
        match (self.state, event) {
            (SendState::AwaitingRinit, SendEvent::Header(h))
                if h.frame_type == FrameType::Zrinit =>
            {
                self.peer_can_fc32 = h.zf0() & CANFC32 != 0;
                self.state = SendState::AwaitingFileAck;
                Ok(vec![
                    SendAction::SendHeader {
                        header: Header::new(FrameType::Zfile),
                        format: self.binary_format(),
                    },
                    SendAction::SendFileInfo(self.info.clone()),
                ])
            }
            (SendState::AwaitingFileAck, SendEvent::Header(h))
                if h.frame_type == FrameType::Zrpos =>
            {
                let from = h.pos();
                self.state = SendState::Streaming { offset: from };
                Ok(vec![
                    SendAction::SendHeader {
                        header: Header::with_pos(FrameType::Zdata, from),
                        format: self.binary_format(),
                    },
                    SendAction::StreamData { from },
                ])
            }
            // Mid-stream rewind: the receiver missed something. Minimal
            // handling — re-anchor and stream again (damping is deferred).
            (SendState::Streaming { .. }, SendEvent::Header(h))
                if h.frame_type == FrameType::Zrpos =>
            {
                let from = h.pos();
                self.state = SendState::Streaming { offset: from };
                Ok(vec![
                    SendAction::SendHeader {
                        header: Header::with_pos(FrameType::Zdata, from),
                        format: self.binary_format(),
                    },
                    SendAction::StreamData { from },
                ])
            }
            // ZACKs during streaming (from ZCRCQ/ZCRCW subpackets) need no
            // action on the happy path.
            (SendState::Streaming { .. }, SendEvent::Header(h))
                if h.frame_type == FrameType::Zack =>
            {
                Ok(vec![])
            }
            (SendState::Streaming { .. }, SendEvent::DataExhausted { offset }) => {
                self.state = SendState::AwaitingEofAck;
                Ok(vec![SendAction::SendHeader {
                    header: Header::with_pos(FrameType::Zeof, offset),
                    format: HeaderFormat::Hex,
                }])
            }
            (SendState::AwaitingEofAck, SendEvent::Header(h))
                if h.frame_type == FrameType::Zrinit =>
            {
                // Single-file happy path: nothing more to offer, finish.
                self.state = SendState::AwaitingFinAck;
                Ok(vec![SendAction::SendHeader {
                    header: Header::new(FrameType::Zfin),
                    format: HeaderFormat::Hex,
                }])
            }
            (SendState::AwaitingFinAck, SendEvent::Header(h))
                if h.frame_type == FrameType::Zfin =>
            {
                self.state = SendState::Done;
                Ok(vec![SendAction::SendOverAndOut, SendAction::Finished])
            }
            _ => Err(unexpected),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_action(actions: &[RecvAction], idx: usize) -> &Header {
        match &actions[idx] {
            RecvAction::SendHeader { header, .. } => header,
            other => panic!("expected SendHeader, got {other:?}"),
        }
    }

    #[test]
    fn receiver_happy_path_full_flow() {
        let mut rx = Receiver::new();

        // ZRQINIT -> ZRINIT
        let actions = rx
            .advance(RecvEvent::Header(Header::new(FrameType::Zrqinit)))
            .unwrap();
        assert_eq!(header_action(&actions, 0).frame_type, FrameType::Zrinit);
        assert_eq!(header_action(&actions, 0).zf0(), CANFDX | CANOVIO | CANFC32);
        assert_eq!(rx.state(), RecvState::AwaitingFile);

        // ZFILE header, then the info subpacket -> OpenFile + ZRPOS(0)
        assert!(rx
            .advance(RecvEvent::Header(Header::new(FrameType::Zfile)))
            .unwrap()
            .is_empty());
        let info = FileInfo {
            length: Some(11),
            ..FileInfo::new("carrot.txt")
        };
        let actions = rx
            .advance(RecvEvent::Data {
                payload: info.encode().unwrap(),
                end: FrameEnd::Zcrcw,
            })
            .unwrap();
        assert_eq!(actions[0], RecvAction::OpenFile(info));
        let rpos = header_action(&actions, 1);
        assert_eq!(rpos.frame_type, FrameType::Zrpos);
        assert_eq!(rpos.pos(), 0);

        // ZDATA(0), then two subpackets.
        assert!(rx
            .advance(RecvEvent::Header(Header::with_pos(FrameType::Zdata, 0)))
            .unwrap()
            .is_empty());
        let actions = rx
            .advance(RecvEvent::Data {
                payload: b"hello ".to_vec(),
                end: FrameEnd::Zcrcg,
            })
            .unwrap();
        assert_eq!(
            actions,
            vec![RecvAction::WriteData {
                offset: 0,
                data: b"hello ".to_vec()
            }]
        );
        let actions = rx
            .advance(RecvEvent::Data {
                payload: b"world".to_vec(),
                end: FrameEnd::Zcrce,
            })
            .unwrap();
        assert_eq!(
            actions[0],
            RecvAction::WriteData {
                offset: 6,
                data: b"world".to_vec()
            }
        );
        assert_eq!(rx.state(), RecvState::AwaitingData { offset: 11 });

        // ZEOF(11) -> CloseFile + fresh ZRINIT.
        let actions = rx
            .advance(RecvEvent::Header(Header::with_pos(FrameType::Zeof, 11)))
            .unwrap();
        assert_eq!(actions[0], RecvAction::CloseFile);
        assert_eq!(header_action(&actions, 1).frame_type, FrameType::Zrinit);

        // ZFIN -> ZFIN + "OO" + Finished.
        let actions = rx
            .advance(RecvEvent::Header(Header::new(FrameType::Zfin)))
            .unwrap();
        assert_eq!(header_action(&actions, 0).frame_type, FrameType::Zfin);
        assert_eq!(actions[1], RecvAction::SendOverAndOut);
        assert_eq!(actions[2], RecvAction::Finished);
        assert_eq!(rx.state(), RecvState::Done);
    }

    #[test]
    fn receiver_acks_zcrcq_and_zcrcw() {
        let mut rx = Receiver::new();
        rx.advance(RecvEvent::Header(Header::new(FrameType::Zrqinit)))
            .unwrap();
        rx.advance(RecvEvent::Header(Header::new(FrameType::Zfile)))
            .unwrap();
        let info = FileInfo::new("f");
        rx.advance(RecvEvent::Data {
            payload: info.encode().unwrap(),
            end: FrameEnd::Zcrcw,
        })
        .unwrap();
        rx.advance(RecvEvent::Header(Header::with_pos(FrameType::Zdata, 0)))
            .unwrap();

        let actions = rx
            .advance(RecvEvent::Data {
                payload: vec![1, 2, 3],
                end: FrameEnd::Zcrcq,
            })
            .unwrap();
        let ack = header_action(&actions, 1);
        assert_eq!(ack.frame_type, FrameType::Zack);
        assert_eq!(ack.pos(), 3);
        assert_eq!(rx.state(), RecvState::ReceivingData { offset: 3 });

        let actions = rx
            .advance(RecvEvent::Data {
                payload: vec![4, 5],
                end: FrameEnd::Zcrcw,
            })
            .unwrap();
        assert_eq!(header_action(&actions, 1).pos(), 5);
        assert_eq!(rx.state(), RecvState::AwaitingData { offset: 5 });
    }

    #[test]
    fn receiver_reanchors_bad_positions() {
        let mut rx = Receiver::new();
        rx.advance(RecvEvent::Header(Header::new(FrameType::Zrqinit)))
            .unwrap();
        rx.advance(RecvEvent::Header(Header::new(FrameType::Zfile)))
            .unwrap();
        rx.advance(RecvEvent::Data {
            payload: FileInfo::new("f").encode().unwrap(),
            end: FrameEnd::Zcrcw,
        })
        .unwrap();

        // ZDATA at the wrong offset -> corrective ZRPOS, state unchanged.
        let actions = rx
            .advance(RecvEvent::Header(Header::with_pos(FrameType::Zdata, 512)))
            .unwrap();
        let rpos = header_action(&actions, 0);
        assert_eq!(rpos.frame_type, FrameType::Zrpos);
        assert_eq!(rpos.pos(), 0);
        assert_eq!(rx.state(), RecvState::AwaitingData { offset: 0 });

        // Same for a premature ZEOF.
        let actions = rx
            .advance(RecvEvent::Header(Header::with_pos(FrameType::Zeof, 512)))
            .unwrap();
        assert_eq!(header_action(&actions, 0).frame_type, FrameType::Zrpos);
    }

    #[test]
    fn receiver_rejects_out_of_order_events() {
        let mut rx = Receiver::new();
        let err = rx
            .advance(RecvEvent::Data {
                payload: vec![],
                end: FrameEnd::Zcrcg,
            })
            .unwrap_err();
        assert_eq!(
            err,
            SessionError::UnexpectedEvent {
                state: "AwaitingInit",
                event: "data subpacket"
            }
        );
    }

    #[test]
    fn sender_happy_path_full_flow() {
        let mut tx = Sender::new(FileInfo {
            length: Some(5),
            ..FileInfo::new("f.bin")
        });

        let actions = tx.start().unwrap();
        assert!(matches!(
            actions[0],
            SendAction::SendHeader {
                header: Header {
                    frame_type: FrameType::Zrqinit,
                    ..
                },
                ..
            }
        ));

        // Receiver's ZRINIT advertises CANFC32 -> binary-32 headers.
        let rinit = Header::with_flags(FrameType::Zrinit, 0, 0, 0, CANFDX | CANOVIO | CANFC32);
        let actions = tx.advance(SendEvent::Header(rinit)).unwrap();
        assert!(tx.peer_can_fc32());
        assert!(matches!(
            actions[0],
            SendAction::SendHeader {
                header: Header {
                    frame_type: FrameType::Zfile,
                    ..
                },
                format: HeaderFormat::Bin32,
            }
        ));
        assert!(matches!(actions[1], SendAction::SendFileInfo(_)));

        // ZRPOS(0) -> ZDATA(0) + stream.
        let actions = tx
            .advance(SendEvent::Header(Header::with_pos(FrameType::Zrpos, 0)))
            .unwrap();
        assert!(matches!(actions[1], SendAction::StreamData { from: 0 }));

        // Caller finished streaming -> ZEOF(5).
        let actions = tx.advance(SendEvent::DataExhausted { offset: 5 }).unwrap();
        match &actions[0] {
            SendAction::SendHeader { header, .. } => {
                assert_eq!(header.frame_type, FrameType::Zeof);
                assert_eq!(header.pos(), 5);
            }
            other => panic!("expected ZEOF, got {other:?}"),
        }

        // ZRINIT -> ZFIN; ZFIN -> OO + Finished.
        let actions = tx
            .advance(SendEvent::Header(Header::new(FrameType::Zrinit)))
            .unwrap();
        assert!(matches!(
            actions[0],
            SendAction::SendHeader {
                header: Header {
                    frame_type: FrameType::Zfin,
                    ..
                },
                ..
            }
        ));
        let actions = tx
            .advance(SendEvent::Header(Header::new(FrameType::Zfin)))
            .unwrap();
        assert_eq!(
            actions,
            vec![SendAction::SendOverAndOut, SendAction::Finished]
        );
        assert_eq!(tx.state(), SendState::Done);
    }

    #[test]
    fn sender_without_fc32_uses_bin16() {
        let mut tx = Sender::new(FileInfo::new("f"));
        tx.start().unwrap();
        let rinit = Header::with_flags(FrameType::Zrinit, 0, 0, 0, CANFDX);
        let actions = tx.advance(SendEvent::Header(rinit)).unwrap();
        assert!(!tx.peer_can_fc32());
        assert!(matches!(
            actions[0],
            SendAction::SendHeader {
                format: HeaderFormat::Bin16,
                ..
            }
        ));
    }

    #[test]
    fn sender_handles_midstream_rewind_and_acks() {
        let mut tx = Sender::new(FileInfo::new("f"));
        tx.start().unwrap();
        tx.advance(SendEvent::Header(Header::with_flags(
            FrameType::Zrinit,
            0,
            0,
            0,
            CANFC32,
        )))
        .unwrap();
        tx.advance(SendEvent::Header(Header::with_pos(FrameType::Zrpos, 0)))
            .unwrap();

        // A ZACK mid-stream is fine and requires nothing.
        assert!(tx
            .advance(SendEvent::Header(Header::with_pos(FrameType::Zack, 128)))
            .unwrap()
            .is_empty());

        // A mid-stream ZRPOS rewinds.
        let actions = tx
            .advance(SendEvent::Header(Header::with_pos(FrameType::Zrpos, 256)))
            .unwrap();
        assert!(matches!(actions[1], SendAction::StreamData { from: 256 }));
        assert_eq!(tx.state(), SendState::Streaming { offset: 256 });
    }

    #[test]
    fn sender_rejects_out_of_order_events() {
        let mut tx = Sender::new(FileInfo::new("f"));
        let err = tx
            .advance(SendEvent::Header(Header::new(FrameType::Zrpos)))
            .unwrap_err();
        assert_eq!(
            err,
            SessionError::UnexpectedEvent {
                state: "Start",
                event: "header"
            }
        );
        // start() twice is also rejected.
        tx.start().unwrap();
        assert!(tx.start().is_err());
    }

    /// The two state machines drive each other through a whole transfer,
    /// exchanging *encoded wire bytes* end to end.
    #[test]
    fn sender_and_receiver_complete_a_wire_level_transfer() {
        use crate::header::decode_header;
        use crate::subpacket::{decode_subpacket, encode_subpacket};

        let file_bytes = b"The quick brown rabbit jumps down the hole.".to_vec();
        let info = FileInfo {
            length: Some(file_bytes.len() as u64),
            ..FileInfo::new("rabbit.txt")
        };
        let mut tx = Sender::new(info);
        let mut rx = Receiver::new();
        let mut received: Vec<u8> = Vec::new();
        let mut finished = false;

        // Queue of wire buffers travelling sender -> receiver.
        let mut to_rx: Vec<Vec<u8>> = Vec::new();
        // Perform sender actions, encoding onto the wire.
        let do_tx = |actions: Vec<SendAction>,
                     to_rx: &mut Vec<Vec<u8>>,
                     tx_state: &mut Option<u32>| {
            for action in actions {
                match action {
                    SendAction::SendHeader { header, format } => to_rx.push(header.encode(format)),
                    SendAction::SendFileInfo(info) => to_rx.push(
                        encode_subpacket(&info.encode().unwrap(), FrameEnd::Zcrcw, true).unwrap(),
                    ),
                    SendAction::StreamData { from } => *tx_state = Some(from),
                    SendAction::SendOverAndOut => to_rx.push(b"OO".to_vec()),
                    SendAction::Finished => {}
                }
            }
        };

        let mut stream_from: Option<u32> = None;
        do_tx(tx.start().unwrap(), &mut to_rx, &mut stream_from);

        // Pump until done (bounded so a bug cannot loop forever).
        for _ in 0..64 {
            // If the sender owes data, stream it in two subpackets + ZEOF.
            if let Some(from) = stream_from.take() {
                let rest = &file_bytes[from as usize..];
                let mid = rest.len() / 2;
                to_rx.push(encode_subpacket(&rest[..mid], FrameEnd::Zcrcg, true).unwrap());
                to_rx.push(encode_subpacket(&rest[mid..], FrameEnd::Zcrce, true).unwrap());
                let end = file_bytes.len() as u32;
                do_tx(
                    tx.advance(SendEvent::DataExhausted { offset: end })
                        .unwrap(),
                    &mut to_rx,
                    &mut stream_from,
                );
            }
            if to_rx.is_empty() {
                break;
            }
            // Receiver consumes one sender emission.
            let wire = to_rx.remove(0);
            let event = if wire == b"OO" {
                continue; // over-and-out needs no receiver event
            } else if wire[0] == crate::ZPAD {
                RecvEvent::Header(decode_header(&wire).unwrap().header)
            } else {
                let sub = decode_subpacket(&wire, true).unwrap();
                RecvEvent::Data {
                    payload: sub.payload,
                    end: sub.end,
                }
            };
            // Track whether we're mid-ZDATA: data subpackets are not
            // headers, so route by event kind via the receiver itself.
            let actions = rx.advance(event).unwrap();
            for action in actions {
                match action {
                    RecvAction::SendHeader { header, .. } => {
                        // Receiver headers go back to the sender.
                        do_tx(
                            tx.advance(SendEvent::Header(header)).unwrap(),
                            &mut to_rx,
                            &mut stream_from,
                        );
                    }
                    RecvAction::OpenFile(info) => assert_eq!(info.name, "rabbit.txt"),
                    RecvAction::WriteData { offset, data } => {
                        assert_eq!(offset as usize, received.len());
                        received.extend_from_slice(&data);
                    }
                    RecvAction::CloseFile => assert_eq!(received, file_bytes),
                    RecvAction::SendOverAndOut => {}
                    RecvAction::Finished => finished = true,
                }
            }
        }

        assert!(finished, "session did not finish");
        assert_eq!(received, file_bytes);
        assert_eq!(rx.state(), RecvState::Done);
        assert_eq!(tx.state(), SendState::Done);
    }
}
