//! The RHP frame: the single message shape carried on every control stream.
//!
//! A frame is a small header plus an opaque postcard-encoded payload. The
//! payload is *not* part of the frame's own serde tree: it is decoded
//! separately, keyed by `(family, message_type)`. This is deliberate —
//! a receiver that doesn't know a message type can still parse the frame,
//! route it, and answer `Unsupported`, instead of failing to decode the
//! whole stream (the forward-compatibility lesson from Hotline's TLV bag
//! and OSCAR's SNACs).

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::error::ErrorCode;
use crate::version::{ProtocolVersion, PROTOCOL_VERSION};

/// Client-chosen identifier correlating requests with replies.
///
/// `0` is reserved for pushes (which correlate with nothing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestId(pub u64);

impl RequestId {
    pub const PUSH: RequestId = RequestId(0);
}

/// What role a frame plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FrameKind {
    /// Peer-initiated; expects exactly one `Reply` with the same id.
    Request,
    /// Answer to a `Request`; echoes its id; may carry an error code.
    Reply,
    /// Server-initiated notification; id is `RequestId::PUSH`; no answer.
    Push,
}

/// Message family — the protocol's namespaces.
///
/// A `u8` newtype (not a Rust enum) so frames with families this build
/// doesn't know still decode and can be answered with `Unsupported`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Family(pub u8);

impl Family {
    /// Session: hello, auth, keepalive, resume, capabilities.
    pub const SESSION: Family = Family(0);
    /// Presence: roster, buddy lists, states (incl. Cheshire mode).
    pub const PRESENCE: Family = Family(1);
    /// Chat rooms: public, private, ad-hoc.
    pub const CHAT: Family = Family(2);
    /// Direct messages and attachments.
    pub const DM: Family = Family(3);
    /// Message bases (boards).
    pub const BOARD: Family = Family(4);
    /// File areas, metadata, transfers.
    pub const FILE: Family = Family(5);
    /// The Warren: swarm advertisement, sources, capability tokens.
    pub const SWARM: Family = Family(6);
    /// Administration and remote config.
    pub const ADMIN: Family = Family(7);
    /// Tunnels: server-to-server federation.
    pub const FEDERATION: Family = Family(8);
    /// Radio: stations, now-playing, votes.
    pub const RADIO: Family = Family(9);
    /// Wishing Well: the request system.
    pub const WISHING_WELL: Family = Family(10);
}

/// An RHP frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Frame {
    pub version: ProtocolVersion,
    pub kind: FrameKind,
    pub family: Family,
    pub message_type: u16,
    pub id: RequestId,
    /// Only meaningful on `Reply` frames; `None` = success.
    pub error: Option<ErrorCode>,
    /// Postcard-encoded message body, keyed by `(family, message_type)`.
    pub payload: Payload,
}

/// Opaque encoded message body.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Payload(pub Vec<u8>);

/// A typed RHP message: something that knows its family and type number
/// and can live in a frame payload.
pub trait Message: Serialize + DeserializeOwned {
    const FAMILY: Family;
    const MESSAGE_TYPE: u16;
}

impl Frame {
    /// Build a request frame from a typed message.
    pub fn request<M: Message>(id: RequestId, msg: &M) -> Result<Frame, crate::ProtoError> {
        Ok(Frame {
            version: PROTOCOL_VERSION,
            kind: FrameKind::Request,
            family: M::FAMILY,
            message_type: M::MESSAGE_TYPE,
            id,
            error: None,
            payload: Payload(postcard::to_allocvec(msg)?),
        })
    }

    /// Build a success reply to `request` from a typed message.
    pub fn reply_to<M: Message>(request: &Frame, msg: &M) -> Result<Frame, crate::ProtoError> {
        Ok(Frame {
            version: request.version,
            kind: FrameKind::Reply,
            family: M::FAMILY,
            message_type: M::MESSAGE_TYPE,
            id: request.id,
            error: None,
            payload: Payload(postcard::to_allocvec(msg)?),
        })
    }

    /// Build an error reply to `request` (empty payload).
    pub fn error_reply(request: &Frame, code: ErrorCode) -> Frame {
        Frame {
            version: request.version,
            kind: FrameKind::Reply,
            family: request.family,
            message_type: request.message_type,
            id: request.id,
            error: Some(code),
            payload: Payload::default(),
        }
    }

    /// Build a push frame from a typed message.
    pub fn push<M: Message>(msg: &M) -> Result<Frame, crate::ProtoError> {
        Ok(Frame {
            version: PROTOCOL_VERSION,
            kind: FrameKind::Push,
            family: M::FAMILY,
            message_type: M::MESSAGE_TYPE,
            id: RequestId::PUSH,
            error: None,
            payload: Payload(postcard::to_allocvec(msg)?),
        })
    }

    /// Decode the payload as a typed message, checking `(family, type)`.
    ///
    /// Returns `None` when the frame is not carrying `M` (wrong family or
    /// type) — decode errors for a *matching* frame are real errors.
    pub fn decode<M: Message>(&self) -> Option<Result<M, crate::ProtoError>> {
        (self.family == M::FAMILY && self.message_type == M::MESSAGE_TYPE)
            .then(|| postcard::from_bytes(&self.payload.0).map_err(Into::into))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct TestMsg {
        n: u32,
        s: String,
    }
    impl Message for TestMsg {
        const FAMILY: Family = Family::SESSION;
        const MESSAGE_TYPE: u16 = 999;
    }

    #[test]
    fn request_reply_roundtrip() {
        let msg = TestMsg {
            n: 7,
            s: "warren".into(),
        };
        let req = Frame::request(RequestId(42), &msg).unwrap();
        assert_eq!(req.kind, FrameKind::Request);
        assert_eq!(req.decode::<TestMsg>().unwrap().unwrap(), msg);

        let reply = Frame::reply_to(&req, &msg).unwrap();
        assert_eq!(reply.id, RequestId(42));
        assert_eq!(reply.error, None);
    }

    #[test]
    fn error_reply_has_empty_payload() {
        let req = Frame::request(
            RequestId(1),
            &TestMsg {
                n: 0,
                s: String::new(),
            },
        )
        .unwrap();
        let err = Frame::error_reply(&req, ErrorCode::Forbidden);
        assert_eq!(err.error, Some(ErrorCode::Forbidden));
        assert!(err.payload.0.is_empty());
    }

    #[test]
    fn decode_rejects_wrong_type() {
        #[derive(Debug, Serialize, Deserialize)]
        struct Other;
        impl Message for Other {
            const FAMILY: Family = Family::CHAT;
            const MESSAGE_TYPE: u16 = 1;
        }
        let req = Frame::request(
            RequestId(1),
            &TestMsg {
                n: 0,
                s: String::new(),
            },
        )
        .unwrap();
        assert!(req.decode::<Other>().is_none());
    }
}
