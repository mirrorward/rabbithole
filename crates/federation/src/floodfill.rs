//! Board flood-fill model: how signed board events propagate between peers.
//!
//! Federation is subscription-driven gossip. A server declares interest in a
//! board with a [`Subscription`]; thereafter peers advertise what they hold
//! and fetch what they lack:
//!
//! 1. [`IHave`] — "for board X, I hold these event ids" (an advertisement,
//!    typically filtered against the local seen-set / Bloom filter before
//!    replying).
//! 2. [`PullRequest`] — "send me these ids I don't have yet".
//! 3. [`PushEvents`] — the requested [`FedEvent`]s, each carrying the *raw
//!    signed board-event bytes* plus its content id.
//!
//! The events flow **unchanged**: a [`FedEvent`] is the opaque postcard of a
//! `rabbithole_server_core::events::SignedEvent`, kept as an untyped byte
//! blob so this crate needs no dependency on the store or event schema. The
//! receiver decodes and verifies it against the origin server's key on
//! ingest.

use serde::{Deserialize, Serialize};

/// A standing interest: this server wants `board_slug` from `peer_key`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subscription {
    /// The peer server's Ed25519 public identity key.
    pub peer_key: [u8; 32],
    /// The board slug being subscribed to (e.g. `"rabbit.general"`).
    pub board_slug: String,
}

/// Advertisement of held event ids for a board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IHave {
    /// The board these ids belong to.
    pub board: String,
    /// Content ids (blake3 of each signed event) the sender holds.
    pub event_ids: Vec<[u8; 32]>,
}

/// Request for specific events the requester is missing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    /// The board the requested ids belong to.
    pub board: String,
    /// Content ids being requested.
    pub event_ids: Vec<[u8; 32]>,
}

/// One federated event on the wire: its content id and the raw, signed
/// board-event bytes exactly as minted by the origin server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FedEvent {
    /// blake3 content id of the signed event (must match the decoded bytes;
    /// the receiver re-derives and checks it on ingest).
    pub id: [u8; 32],
    /// Opaque postcard of the origin's `SignedEvent`. Not interpreted here.
    pub bytes: Vec<u8>,
}

/// Delivery of requested events for a board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushEvents {
    /// The board these events belong to.
    pub board: String,
    /// The events, in no guaranteed order.
    pub events: Vec<FedEvent>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(value: &T) -> T
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        postcard::from_bytes(&postcard::to_allocvec(value).unwrap()).unwrap()
    }

    #[test]
    fn subscription_roundtrip() {
        let s = Subscription {
            peer_key: [1u8; 32],
            board_slug: "rabbit.general".into(),
        };
        assert_eq!(roundtrip(&s), s);
    }

    #[test]
    fn ihave_and_pull_roundtrip() {
        let ih = IHave {
            board: "rabbit.general".into(),
            event_ids: vec![[2u8; 32], [3u8; 32]],
        };
        assert_eq!(roundtrip(&ih), ih);

        let pr = PullRequest {
            board: "rabbit.general".into(),
            event_ids: vec![[3u8; 32]],
        };
        assert_eq!(roundtrip(&pr), pr);
    }

    #[test]
    fn push_events_roundtrip_preserves_raw_bytes() {
        let push = PushEvents {
            board: "rabbit.general".into(),
            events: vec![
                FedEvent {
                    id: [4u8; 32],
                    bytes: vec![0, 1, 2, 3, 255, 254],
                },
                FedEvent {
                    id: [5u8; 32],
                    bytes: vec![],
                },
            ],
        };
        let back = roundtrip(&push);
        assert_eq!(back, push);
        assert_eq!(back.events[0].bytes, vec![0, 1, 2, 3, 255, 254]);
    }

    #[test]
    fn decoders_never_panic_on_garbage() {
        assert!(postcard::from_bytes::<PushEvents>(&[0xff; 4]).is_err());
        assert!(postcard::from_bytes::<IHave>(&[0xff, 0xff]).is_err());
    }
}
