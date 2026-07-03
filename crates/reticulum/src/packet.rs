//! Reticulum wire packet — header bit-packing and a bounds-checked codec.
//!
//! The Reticulum packet layout (see
//! <https://reticulum.network/manual/understanding.html#wire-format> and
//! `RNS.Packet`) is:
//!
//! ```text
//! +---------+--------+--------------------------+---------+-------------+
//! | flags   | hops   | address field(s)         | context | data        |
//! | 1 byte  | 1 byte | 16 bytes (×1 or ×2)      | 1 byte  | 0..N bytes  |
//! +---------+--------+--------------------------+---------+-------------+
//! ```
//!
//! The `flags` byte packs six fields (MSB first):
//!
//! ```text
//! bit 7  : IFAC flag        (interface access-code present)
//! bit 6  : header type      (0 = HEADER_1 one address, 1 = HEADER_2 two addrs)
//! bit 5  : context flag
//! bit 4  : propagation type (0 = BROADCAST, 1 = TRANSPORT)
//! bits 3-2: destination type (0 SINGLE, 1 GROUP, 2 PLAIN, 3 LINK)
//! bits 1-0: packet type      (0 DATA, 1 ANNOUNCE, 2 LINKREQUEST, 3 PROOF)
//! ```
//!
//! A HEADER_2 packet carries two 16-byte address fields: the first is the
//! transport (next-hop) id, the second is the destination hash. HEADER_1
//! carries only the destination hash.
//!
//! **Divergence note:** when the IFAC flag is set, upstream Reticulum also
//! carries a variable-length IFAC authentication field. That field is an
//! interface-layer concern and is out of scope for this data-model slice — the
//! codec preserves the flag bit but does not read or write an IFAC body.
//!
//! The [`Packet::decode`] path performs bounds checks at every step and returns
//! a [`PacketError`] on truncated or malformed input; it never panics.
//!
//! # Size caps
//!
//! Reticulum fixes the wire MTU at [`MTU`] = 500 bytes. [`Packet::encode`] and
//! [`Packet::decode`] both enforce it (a longer buffer yields
//! [`PacketError::TooLarge`]); [`max_data_len`] gives the payload budget for a
//! header type.
//!
//! ```text
//! header type 1: 1 flags + 1 hops + 16 dest       + 1 context = 19 → 481 data
//! header type 2: 1 flags + 1 hops + 16 tid + 16 dest + 1 ctx  = 35 → 465 data
//! ```
//!
//! // SPEC-CHECK: upstream RNS advertises a single `Packet.MDU = 464` — it
//! // budgets the *maximum* header (35 bytes) plus one reserved IFAC byte for
//! // every packet regardless of header type. This codec does not model IFAC
//! // bodies, so it caps the encoded packet at MTU and exposes the exact
//! // per-header budget instead; an interop pass that adds IFAC fields must
//! // revisit `max_data_len`.
//!
//! # Packet hash
//!
//! [`Packet::packet_hash`] mirrors `RNS.Packet.get_hash()`: SHA-256 over the
//! *hashable part* — the flags byte masked to its destination-type and
//! packet-type bits, then destination hash, context, and payload. Hops, the
//! transport id, and the IFAC/header-type/context-flag/propagation bits are
//! all excluded, so the hash is stable while a packet is forwarded across the
//! mesh. [`Packet::truncated_hash`] (first 16 bytes) is what link
//! establishment derives its link id from.

use sha2::{Digest, Sha256};

use crate::destination::DESTINATION_HASH_LENGTH;

/// The Reticulum wire MTU: no encoded packet may exceed this many bytes.
pub const MTU: usize = 500;

const FLAG_IFAC: u8 = 0b1000_0000;
const FLAG_HEADER_TYPE: u8 = 0b0100_0000;
const FLAG_CONTEXT: u8 = 0b0010_0000;
const FLAG_PROPAGATION: u8 = 0b0001_0000;
const MASK_DESTINATION: u8 = 0b0000_1100;
const SHIFT_DESTINATION: u8 = 2;
const MASK_PACKET: u8 = 0b0000_0011;

/// Number of address fields carried by each header type.
const HEADER_1_ADDRS: usize = 1;
const HEADER_2_ADDRS: usize = 2;

/// Header type — how many address fields the packet carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderType {
    /// One address field (the destination hash).
    Header1,
    /// Two address fields (transport id, then destination hash).
    Header2,
}

impl HeaderType {
    /// Encoded size of everything except the payload: flags, hops, the
    /// address field(s), and the context byte.
    pub fn encoded_header_len(self) -> usize {
        let addrs = match self {
            HeaderType::Header1 => HEADER_1_ADDRS,
            HeaderType::Header2 => HEADER_2_ADDRS,
        };
        2 + addrs * DESTINATION_HASH_LENGTH + 1
    }
}

/// Maximum payload length for a packet of the given header type such that the
/// encoded packet fits in [`MTU`] (481 for HEADER_1, 465 for HEADER_2 — see
/// the module-level SPEC-CHECK note on upstream's flat `MDU = 464`).
pub fn max_data_len(header_type: HeaderType) -> usize {
    MTU - header_type.encoded_header_len()
}

/// Packet propagation type (the single-bit field in the header).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropagationType {
    /// Broadcast onto the local interface(s).
    Broadcast,
    /// Routed through the transport layer.
    Transport,
}

/// Destination type addressed by the packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DestinationType {
    /// A single identified destination (asymmetric crypto).
    Single,
    /// A group destination (shared symmetric key).
    Group,
    /// A plaintext destination.
    Plain,
    /// A link endpoint.
    Link,
}

impl DestinationType {
    fn to_bits(self) -> u8 {
        match self {
            DestinationType::Single => 0,
            DestinationType::Group => 1,
            DestinationType::Plain => 2,
            DestinationType::Link => 3,
        }
    }

    fn from_bits(bits: u8) -> Self {
        match bits & 0b11 {
            0 => DestinationType::Single,
            1 => DestinationType::Group,
            2 => DestinationType::Plain,
            _ => DestinationType::Link,
        }
    }
}

/// Packet type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketType {
    /// A data packet.
    Data,
    /// An announce packet (identity + destination advertisement).
    Announce,
    /// A link request.
    LinkRequest,
    /// A proof packet.
    Proof,
}

impl PacketType {
    fn to_bits(self) -> u8 {
        match self {
            PacketType::Data => 0,
            PacketType::Announce => 1,
            PacketType::LinkRequest => 2,
            PacketType::Proof => 3,
        }
    }

    fn from_bits(bits: u8) -> Self {
        match bits & 0b11 {
            0 => PacketType::Data,
            1 => PacketType::Announce,
            2 => PacketType::LinkRequest,
            _ => PacketType::Proof,
        }
    }
}

/// Common context byte values (`RNS.Packet` contexts). The context byte is
/// otherwise carried verbatim.
///
/// // SPEC-CHECK: `NONE`/`LRPROOF` match upstream. The link-lifecycle values
/// // (`KEEPALIVE`, `LINKIDENTIFY`, `LINKCLOSE`, `LINKPROOF`, `LRRTT`) follow
/// // the RNS `Packet.py` constants as understood at 0.7/0.8; they are pinned
/// // in `context_bytes_are_pinned` so an interop pass can adjust them in one
/// // place.
pub mod context {
    /// No special context.
    pub const NONE: u8 = 0x00;
    /// Link keepalive.
    pub const KEEPALIVE: u8 = 0xFA;
    /// Link peer identification (identity revealed over an established link).
    pub const LINKIDENTIFY: u8 = 0xFB;
    /// Link teardown notice.
    pub const LINKCLOSE: u8 = 0xFC;
    /// Proof for a packet sent over an established link.
    pub const LINKPROOF: u8 = 0xFD;
    /// Link-request round-trip-time measurement message.
    pub const LRRTT: u8 = 0xFE;
    /// Link-request proof context.
    pub const LRPROOF: u8 = 0xFF;
}

/// A decoded/decodable Reticulum packet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    /// IFAC flag bit (see the module-level divergence note).
    pub ifac: bool,
    /// Header type (implies the number of address fields).
    pub header_type: HeaderType,
    /// Context flag bit.
    pub context_flag: bool,
    /// Propagation type.
    pub propagation_type: PropagationType,
    /// Destination type.
    pub destination_type: DestinationType,
    /// Packet type.
    pub packet_type: PacketType,
    /// Hop count.
    pub hops: u8,
    /// Transport (next-hop) id — present iff `header_type == Header2`.
    pub transport_id: Option<[u8; DESTINATION_HASH_LENGTH]>,
    /// Destination hash (the address).
    pub destination_hash: [u8; DESTINATION_HASH_LENGTH],
    /// Context byte.
    pub context: u8,
    /// Packet payload.
    pub data: Vec<u8>,
}

/// Errors produced while decoding a packet from bytes.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PacketError {
    /// The input was shorter than the fixed header for its header type.
    #[error("packet truncated: need at least {needed} bytes, got {got}")]
    Truncated {
        /// Minimum number of bytes required.
        needed: usize,
        /// Number of bytes actually available.
        got: usize,
    },
    /// A HEADER_2 packet was constructed/encoded without a transport id, or a
    /// HEADER_1 packet was given one.
    #[error("header type / transport-id mismatch")]
    HeaderTransportMismatch,
    /// The encoded packet (or the buffer offered for decoding) exceeds the
    /// Reticulum [`MTU`].
    #[error("packet of {size} bytes exceeds the {mtu}-byte Reticulum MTU", size = .0, mtu = MTU)]
    TooLarge(usize),
}

impl Packet {
    /// Construct a HEADER_1 (single-address) packet.
    pub fn new_header1(
        destination_type: DestinationType,
        packet_type: PacketType,
        destination_hash: [u8; DESTINATION_HASH_LENGTH],
        context: u8,
        data: Vec<u8>,
    ) -> Self {
        Self {
            ifac: false,
            header_type: HeaderType::Header1,
            context_flag: false,
            propagation_type: PropagationType::Broadcast,
            destination_type,
            packet_type,
            hops: 0,
            transport_id: None,
            destination_hash,
            context,
            data,
        }
    }

    /// Construct a HEADER_2 (transport-routed) packet.
    pub fn new_header2(
        destination_type: DestinationType,
        packet_type: PacketType,
        transport_id: [u8; DESTINATION_HASH_LENGTH],
        destination_hash: [u8; DESTINATION_HASH_LENGTH],
        context: u8,
        data: Vec<u8>,
    ) -> Self {
        Self {
            ifac: false,
            header_type: HeaderType::Header2,
            context_flag: false,
            propagation_type: PropagationType::Transport,
            destination_type,
            packet_type,
            hops: 0,
            transport_id: Some(transport_id),
            destination_hash,
            context,
            data,
        }
    }

    /// Number of address fields this packet carries.
    fn address_fields(&self) -> usize {
        match self.header_type {
            HeaderType::Header1 => HEADER_1_ADDRS,
            HeaderType::Header2 => HEADER_2_ADDRS,
        }
    }

    /// Pack the two-field `flags` byte.
    fn flags(&self) -> u8 {
        let mut flags = 0u8;
        if self.ifac {
            flags |= FLAG_IFAC;
        }
        if matches!(self.header_type, HeaderType::Header2) {
            flags |= FLAG_HEADER_TYPE;
        }
        if self.context_flag {
            flags |= FLAG_CONTEXT;
        }
        if matches!(self.propagation_type, PropagationType::Transport) {
            flags |= FLAG_PROPAGATION;
        }
        flags |= self.destination_type.to_bits() << SHIFT_DESTINATION;
        flags |= self.packet_type.to_bits();
        flags
    }

    /// Encode the packet to bytes.
    ///
    /// Returns [`PacketError::HeaderTransportMismatch`] if the header type and
    /// the presence of a transport id disagree, and [`PacketError::TooLarge`]
    /// if the encoded packet would exceed the [`MTU`] (see [`max_data_len`]).
    pub fn encode(&self) -> Result<Vec<u8>, PacketError> {
        let has_transport = self.transport_id.is_some();
        let wants_transport = matches!(self.header_type, HeaderType::Header2);
        if has_transport != wants_transport {
            return Err(PacketError::HeaderTransportMismatch);
        }
        let total = self.header_type.encoded_header_len() + self.data.len();
        if total > MTU {
            return Err(PacketError::TooLarge(total));
        }

        let addr_bytes = self.address_fields() * DESTINATION_HASH_LENGTH;
        let mut out = Vec::with_capacity(2 + addr_bytes + 1 + self.data.len());
        out.push(self.flags());
        out.push(self.hops);
        if let Some(tid) = &self.transport_id {
            out.extend_from_slice(tid);
        }
        out.extend_from_slice(&self.destination_hash);
        out.push(self.context);
        out.extend_from_slice(&self.data);
        Ok(out)
    }

    /// Decode a packet from bytes with full bounds checking. Never panics on
    /// truncated or arbitrary input. Buffers longer than the [`MTU`] are
    /// rejected with [`PacketError::TooLarge`] before any parsing.
    pub fn decode(bytes: &[u8]) -> Result<Self, PacketError> {
        if bytes.len() > MTU {
            return Err(PacketError::TooLarge(bytes.len()));
        }
        // Minimum: flags + hops.
        let mut cursor = 0usize;
        let read = |cursor: &mut usize, n: usize| -> Result<core::ops::Range<usize>, PacketError> {
            let start = *cursor;
            let end = start.checked_add(n).ok_or(PacketError::Truncated {
                needed: usize::MAX,
                got: bytes.len(),
            })?;
            if end > bytes.len() {
                return Err(PacketError::Truncated {
                    needed: end,
                    got: bytes.len(),
                });
            }
            *cursor = end;
            Ok(start..end)
        };

        let flags = bytes.first().copied().ok_or(PacketError::Truncated {
            needed: 2,
            got: bytes.len(),
        })?;
        cursor += 1;
        let hops = bytes.get(cursor).copied().ok_or(PacketError::Truncated {
            needed: 2,
            got: bytes.len(),
        })?;
        cursor += 1;

        let ifac = flags & FLAG_IFAC != 0;
        let header_type = if flags & FLAG_HEADER_TYPE != 0 {
            HeaderType::Header2
        } else {
            HeaderType::Header1
        };
        let context_flag = flags & FLAG_CONTEXT != 0;
        let propagation_type = if flags & FLAG_PROPAGATION != 0 {
            PropagationType::Transport
        } else {
            PropagationType::Broadcast
        };
        let destination_type =
            DestinationType::from_bits((flags & MASK_DESTINATION) >> SHIFT_DESTINATION);
        let packet_type = PacketType::from_bits(flags & MASK_PACKET);

        let transport_id = if matches!(header_type, HeaderType::Header2) {
            let range = read(&mut cursor, DESTINATION_HASH_LENGTH)?;
            let mut tid = [0u8; DESTINATION_HASH_LENGTH];
            tid.copy_from_slice(&bytes[range]);
            Some(tid)
        } else {
            None
        };

        let dest_range = read(&mut cursor, DESTINATION_HASH_LENGTH)?;
        let mut destination_hash = [0u8; DESTINATION_HASH_LENGTH];
        destination_hash.copy_from_slice(&bytes[dest_range]);

        let ctx_range = read(&mut cursor, 1)?;
        let context = bytes[ctx_range.start];

        let data = bytes[cursor..].to_vec();

        Ok(Self {
            ifac,
            header_type,
            context_flag,
            propagation_type,
            destination_type,
            packet_type,
            hops,
            transport_id,
            destination_hash,
            context,
            data,
        })
    }

    /// The *hashable part* of the packet, per `RNS.Packet.get_hashable_part`:
    ///
    /// ```text
    /// (flags & 0b0000_1111) || destination_hash || context || data
    /// ```
    ///
    /// The masked flags keep only the destination-type and packet-type bits;
    /// hops and the HEADER_2 transport id are excluded. Everything a
    /// transport node rewrites while forwarding is therefore outside the
    /// hash, which is what makes it usable for de-duplication and link ids.
    pub fn hashable_part(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + DESTINATION_HASH_LENGTH + 1 + self.data.len());
        out.push(self.flags() & 0b0000_1111);
        out.extend_from_slice(&self.destination_hash);
        out.push(self.context);
        out.extend_from_slice(&self.data);
        out
    }

    /// The full 32-byte packet hash: `SHA-256(hashable_part)`.
    pub fn packet_hash(&self) -> [u8; 32] {
        Sha256::digest(self.hashable_part()).into()
    }

    /// The truncated 16-byte packet hash — the first
    /// [`DESTINATION_HASH_LENGTH`] bytes of [`packet_hash`](Self::packet_hash).
    /// Link establishment derives its link id from this value of the link
    /// request packet.
    pub fn truncated_hash(&self) -> [u8; DESTINATION_HASH_LENGTH] {
        let mut out = [0u8; DESTINATION_HASH_LENGTH];
        out.copy_from_slice(&self.packet_hash()[..DESTINATION_HASH_LENGTH]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(seed: u8) -> [u8; DESTINATION_HASH_LENGTH] {
        [seed; DESTINATION_HASH_LENGTH]
    }

    #[test]
    fn header1_roundtrip() {
        let p = Packet::new_header1(
            DestinationType::Single,
            PacketType::Data,
            addr(0xAB),
            context::NONE,
            vec![1, 2, 3, 4],
        );
        let encoded = p.encode().unwrap();
        // flags + hops + 16 addr + 1 context + 4 data
        assert_eq!(encoded.len(), 2 + 16 + 1 + 4);
        let decoded = Packet::decode(&encoded).unwrap();
        assert_eq!(decoded, p);
    }

    #[test]
    fn header2_roundtrip_with_transport_id() {
        let p = Packet::new_header2(
            DestinationType::Link,
            PacketType::LinkRequest,
            addr(0x11),
            addr(0x22),
            context::LRPROOF,
            vec![9, 9, 9],
        );
        let encoded = p.encode().unwrap();
        assert_eq!(encoded.len(), 2 + 32 + 1 + 3);
        let decoded = Packet::decode(&encoded).unwrap();
        assert_eq!(decoded, p);
        assert_eq!(decoded.transport_id, Some(addr(0x11)));
        assert_eq!(decoded.destination_hash, addr(0x22));
    }

    #[test]
    fn all_type_combinations_roundtrip() {
        let dtypes = [
            DestinationType::Single,
            DestinationType::Group,
            DestinationType::Plain,
            DestinationType::Link,
        ];
        let ptypes = [
            PacketType::Data,
            PacketType::Announce,
            PacketType::LinkRequest,
            PacketType::Proof,
        ];
        for dt in dtypes {
            for pt in ptypes {
                let mut p = Packet::new_header1(dt, pt, addr(1), 0x07, vec![]);
                p.ifac = true;
                p.context_flag = true;
                p.propagation_type = PropagationType::Transport;
                p.hops = 42;
                let decoded = Packet::decode(&p.encode().unwrap()).unwrap();
                assert_eq!(decoded, p, "roundtrip failed for {dt:?}/{pt:?}");
            }
        }
    }

    #[test]
    fn empty_data_roundtrip() {
        let p = Packet::new_header1(
            DestinationType::Plain,
            PacketType::Proof,
            addr(7),
            context::NONE,
            vec![],
        );
        let decoded = Packet::decode(&p.encode().unwrap()).unwrap();
        assert_eq!(decoded, p);
        assert!(decoded.data.is_empty());
    }

    #[test]
    fn encode_rejects_header_transport_mismatch() {
        let mut p = Packet::new_header1(
            DestinationType::Single,
            PacketType::Data,
            addr(1),
            0,
            vec![],
        );
        p.transport_id = Some(addr(2)); // header1 but has a transport id
        assert_eq!(p.encode(), Err(PacketError::HeaderTransportMismatch));
    }

    #[test]
    fn decode_truncated_never_panics() {
        // Every truncation point of a valid header2 packet must error, not panic.
        let full = Packet::new_header2(
            DestinationType::Single,
            PacketType::Announce,
            addr(3),
            addr(4),
            0,
            vec![1, 2, 3],
        )
        .encode()
        .unwrap();
        for len in 0..full.len() {
            // The final valid length (context present) is the smallest complete
            // packet; anything shorter than header+addr+context must error.
            let res = Packet::decode(&full[..len]);
            let min_valid = 2 + 32 + 1;
            if len < min_valid {
                assert!(res.is_err(), "expected error at len {len}");
            }
        }
    }

    #[test]
    fn decode_arbitrary_bytes_never_panics() {
        // Deterministic pseudo-random sweep — decoder must be total.
        let mut state: u64 = 0x9E3779B97F4A7C15;
        for _ in 0..5000 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let len = (state >> 56) as usize % 80;
            let mut buf = Vec::with_capacity(len);
            let mut s = state;
            for _ in 0..len {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1);
                buf.push((s >> 33) as u8);
            }
            // Must not panic; may Ok or Err.
            let _ = Packet::decode(&buf);
        }
    }

    #[test]
    fn empty_input_errors() {
        assert!(matches!(
            Packet::decode(&[]),
            Err(PacketError::Truncated { .. })
        ));
    }

    #[test]
    fn pinned_wire_layout_vector() {
        // Hand-assembled from the header diagram: IFAC=0, HEADER_2 (0x40),
        // context flag 0, TRANSPORT (0x10), LINK destination (0b11 << 2),
        // LINKREQUEST (0b10) → flags 0x5E; hops 7; transport id 0x11×16;
        // destination 0x22×16; context LRPROOF (0xFF); data AA BB.
        let mut p = Packet::new_header2(
            DestinationType::Link,
            PacketType::LinkRequest,
            addr(0x11),
            addr(0x22),
            context::LRPROOF,
            vec![0xAA, 0xBB],
        );
        p.hops = 7;
        let mut expected = vec![0x5E, 0x07];
        expected.extend_from_slice(&[0x11; 16]);
        expected.extend_from_slice(&[0x22; 16]);
        expected.push(0xFF);
        expected.extend_from_slice(&[0xAA, 0xBB]);
        assert_eq!(p.encode().unwrap(), expected);
    }

    #[test]
    fn context_bytes_are_pinned() {
        // SPEC-CHECK anchor: adjust here (and only here) if an interop pass
        // finds different upstream values.
        assert_eq!(context::NONE, 0x00);
        assert_eq!(context::KEEPALIVE, 0xFA);
        assert_eq!(context::LINKIDENTIFY, 0xFB);
        assert_eq!(context::LINKCLOSE, 0xFC);
        assert_eq!(context::LINKPROOF, 0xFD);
        assert_eq!(context::LRRTT, 0xFE);
        assert_eq!(context::LRPROOF, 0xFF);
    }

    #[test]
    fn max_data_len_budgets() {
        assert_eq!(HeaderType::Header1.encoded_header_len(), 19);
        assert_eq!(HeaderType::Header2.encoded_header_len(), 35);
        assert_eq!(max_data_len(HeaderType::Header1), 481);
        assert_eq!(max_data_len(HeaderType::Header2), 465);
    }

    #[test]
    fn encode_enforces_mtu() {
        let fits = Packet::new_header1(
            DestinationType::Single,
            PacketType::Data,
            addr(1),
            context::NONE,
            vec![0; max_data_len(HeaderType::Header1)],
        );
        let encoded = fits.encode().unwrap();
        assert_eq!(encoded.len(), MTU);
        assert_eq!(Packet::decode(&encoded).unwrap(), fits);

        let over = Packet::new_header1(
            DestinationType::Single,
            PacketType::Data,
            addr(1),
            context::NONE,
            vec![0; max_data_len(HeaderType::Header1) + 1],
        );
        assert_eq!(over.encode(), Err(PacketError::TooLarge(MTU + 1)));

        let over2 = Packet::new_header2(
            DestinationType::Single,
            PacketType::Data,
            addr(1),
            addr(2),
            context::NONE,
            vec![0; max_data_len(HeaderType::Header2) + 1],
        );
        assert_eq!(over2.encode(), Err(PacketError::TooLarge(MTU + 1)));
    }

    #[test]
    fn decode_enforces_mtu() {
        assert_eq!(
            Packet::decode(&vec![0u8; MTU + 1]),
            Err(PacketError::TooLarge(MTU + 1))
        );
        // Exactly MTU decodes fine.
        assert!(Packet::decode(&vec![0u8; MTU]).is_ok());
    }

    #[test]
    fn hashable_part_masks_forwarding_fields() {
        let base = Packet::new_header1(
            DestinationType::Single,
            PacketType::LinkRequest,
            addr(0x42),
            context::NONE,
            vec![1, 2, 3],
        );

        // Hops mutate in transit — hash must be stable.
        let mut hopped = base.clone();
        hopped.hops = 99;
        assert_eq!(base.packet_hash(), hopped.packet_hash());

        // A transport node rewriting to HEADER_2 (inserting a transport id,
        // switching propagation) must not change the hash either.
        let mut routed = base.clone();
        routed.header_type = HeaderType::Header2;
        routed.transport_id = Some(addr(0x77));
        routed.propagation_type = PropagationType::Transport;
        routed.ifac = true;
        routed.context_flag = true;
        assert_eq!(base.packet_hash(), routed.packet_hash());
        assert_eq!(base.truncated_hash(), routed.truncated_hash());

        // The addressed/typed content *is* covered.
        let mut retyped = base.clone();
        retyped.packet_type = PacketType::Data;
        assert_ne!(base.packet_hash(), retyped.packet_hash());
        let mut readdressed = base.clone();
        readdressed.destination_hash = addr(0x43);
        assert_ne!(base.packet_hash(), readdressed.packet_hash());
        let mut recontexted = base.clone();
        recontexted.context = context::LRRTT;
        assert_ne!(base.packet_hash(), recontexted.packet_hash());
        let mut redata = base;
        redata.data = vec![1, 2, 4];
        assert_ne!(redata.packet_hash(), hopped.packet_hash());
    }

    #[test]
    fn hash_survives_encode_decode() {
        let p = Packet::new_header2(
            DestinationType::Link,
            PacketType::Proof,
            addr(9),
            addr(8),
            context::LRPROOF,
            vec![5; 40],
        );
        let decoded = Packet::decode(&p.encode().unwrap()).unwrap();
        assert_eq!(p.packet_hash(), decoded.packet_hash());
        assert_eq!(p.truncated_hash(), decoded.truncated_hash());
    }

    #[test]
    fn pinned_truncated_hash_vector() {
        // Deterministic packet → pinned truncated hash, so the hashable-part
        // layout cannot drift silently.
        let p = Packet::new_header1(
            DestinationType::Single,
            PacketType::LinkRequest,
            addr(0x22),
            context::NONE,
            vec![0xAA, 0xBB],
        );
        assert_eq!(
            p.hashable_part(),
            {
                let mut v = vec![0x02];
                v.extend_from_slice(&[0x22; 16]);
                v.push(0x00);
                v.extend_from_slice(&[0xAA, 0xBB]);
                v
            },
            "hashable part layout drifted"
        );
        assert_eq!(
            hex::encode(p.truncated_hash()),
            "5aee627e4891134ad17f4b56e432a70d"
        );
    }
}
