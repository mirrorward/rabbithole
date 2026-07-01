//! Protocol version constants and negotiation rules.

use serde::{Deserialize, Serialize};

/// The protocol version this build speaks natively.
///
/// Bumped when the wire format changes incompatibly. Additive changes
/// (new families, new message types, new optional fields, new capability
/// flags) do NOT bump the version — they are negotiated via
/// [`crate::CapabilitySet`] instead.
pub const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion(1);

/// The oldest peer version this build still understands.
pub const MIN_SUPPORTED_VERSION: ProtocolVersion = ProtocolVersion(1);

/// A protocol version number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProtocolVersion(pub u16);

impl ProtocolVersion {
    /// Pick the version both sides speak: the highest common version, or
    /// `None` if the ranges don't overlap.
    pub fn negotiate(ours: ProtocolVersion, theirs: ProtocolVersion) -> Option<ProtocolVersion> {
        let chosen = ours.min(theirs);
        (chosen >= MIN_SUPPORTED_VERSION).then_some(chosen)
    }
}

impl core::fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "rhp/{}", self.0)
    }
}

/// ALPN identifier offered on QUIC connections.
pub const ALPN: &[u8] = b"rhp/1";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negotiate_picks_lowest_common() {
        let v1 = ProtocolVersion(1);
        let v2 = ProtocolVersion(2);
        assert_eq!(ProtocolVersion::negotiate(v1, v2), Some(v1));
        assert_eq!(ProtocolVersion::negotiate(v2, v1), Some(v1));
        assert_eq!(ProtocolVersion::negotiate(v2, v2), Some(v2));
    }

    #[test]
    fn negotiate_rejects_below_minimum() {
        let v0 = ProtocolVersion(0);
        assert_eq!(ProtocolVersion::negotiate(PROTOCOL_VERSION, v0), None);
    }
}
