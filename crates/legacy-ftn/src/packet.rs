//! FTS-0001 packet header + FSC-0039 type-2+ variant.
//!
//! A `.PKT` file is a 58-byte header followed by a run of packed messages and
//! a two-byte `0x0000` terminator (see [`crate::message`]). All multi-byte
//! integers are **little-endian**.
//!
//! ## FTS-0001 type-2 header (58 bytes)
//!
//! ```text
//!  off  sz  field           notes
//!   0    2  origNode        u16
//!   2    2  destNode        u16
//!   4    2  year            u16 (full year, e.g. 2026)
//!   6    2  month           u16 (0 = January .. 11 = December)
//!   8    2  day             u16 (1..31)
//!  10    2  hour            u16 (0..23)
//!  12    2  minute          u16 (0..59)
//!  14    2  second          u16 (0..59)
//!  16    2  baud            u16
//!  18    2  packetType      u16  == 2
//!  20    2  origNet         u16
//!  22    2  destNet         u16
//!  24    1  productCodeLow  u8
//!  25    1  revisionLow     u8  (product revision / serial)
//!  26    8  password        8 bytes, NUL-padded
//!  34    2  origZone        u16  (a.k.a. qOrigZone)
//!  36    2  destZone        u16  (a.k.a. qDestZone)
//!  38   20  fill            reserved in plain type-2
//! ```
//!
//! ## FSC-0039 type-2+ extension (the 20 "fill" bytes, offsets 38..58)
//!
//! ```text
//!  off  sz  field           notes
//!  38    2  auxNet          u16  (origNet when origNode is a point, i.e. origNet == 0xFFFF)
//!  40    2  capValidCopy    u16  byte-swapped copy of the capability word
//!  42    1  productCodeHigh u8
//!  43    1  revisionHigh    u8
//!  44    2  capabilityWord  u16  (bit 0 set == type-2+)
//!  46    2  origZone        u16  (authoritative for 2+)
//!  48    2  destZone        u16
//!  50    2  origPoint       u16
//!  52    2  destPoint       u16
//!  54    4  productData      u32
//! ```
//!
//! **Detection.** A reader treats the packet as type-2+ only when the
//! capability word is non-zero *and* `capValidCopy == capabilityWord.swap_bytes()`.
//! Any other bit pattern (including all-zero fill from a legacy packer) falls
//! back to plain type-2, per FSC-0039's compatibility rule.

use crate::error::FtnError;
use crate::message::PackedMessage;
use crate::reader::Reader;
use crate::FtnAddress;

/// Fixed size of a type-2 / type-2+ packet header.
pub const HEADER_LEN: usize = 58;

/// The packet type value stored at offset 18 for both type-2 and type-2+.
pub const PACKET_TYPE_2: u16 = 2;

/// DOS-style broken-down timestamp as stored in the packet header.
///
/// Field ranges mirror FTS-0001 exactly; note `month` is **0-based**
/// (0 = January). Values are stored verbatim and never validated on decode so
/// that odd real-world packets round-trip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DosDateTime {
    /// Full year (e.g. 2026).
    pub year: u16,
    /// Month, 0 = January .. 11 = December.
    pub month: u16,
    /// Day of month, 1..31.
    pub day: u16,
    /// Hour, 0..23.
    pub hour: u16,
    /// Minute, 0..59.
    pub minute: u16,
    /// Second, 0..59.
    pub second: u16,
}

/// The type-2+ (FSC-0039) extension carried in the header's tail 20 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Type2Plus {
    /// Auxiliary net: the real origNet when `origNet` is `0xFFFF` (point).
    pub aux_net: u16,
    /// Capability word; bit 0 marks the packet as type-2+.
    pub capability: u16,
    /// High byte of the product code.
    pub product_code_high: u8,
    /// High byte of the product revision.
    pub revision_high: u8,
    /// Authoritative origin zone for type-2+.
    pub orig_zone: u16,
    /// Authoritative destination zone for type-2+.
    pub dest_zone: u16,
    /// Origin point.
    pub orig_point: u16,
    /// Destination point.
    pub dest_point: u16,
    /// Product-specific data word.
    pub product_data: u32,
}

/// A decoded packet header. `plus` is `Some` for type-2+, `None` for plain
/// type-2.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PacketHeader {
    /// Originating node number.
    pub orig_node: u16,
    /// Destination node number.
    pub dest_node: u16,
    /// Creation timestamp.
    pub date_time: DosDateTime,
    /// Session baud rate hint.
    pub baud: u16,
    /// Originating net number (`0xFFFF` when the origin is a point in 2+).
    pub orig_net: u16,
    /// Destination net number.
    pub dest_net: u16,
    /// Low byte of the product code.
    pub product_code_low: u8,
    /// Low byte of the product revision.
    pub revision_low: u8,
    /// Session password, 8 bytes, NUL-padded.
    pub password: [u8; 8],
    /// Origin zone (type-2 field; also the fallback for 2+).
    pub orig_zone: u16,
    /// Destination zone (type-2 field; also the fallback for 2+).
    pub dest_zone: u16,
    /// Type-2+ extension, present only when detected.
    pub plus: Option<Type2Plus>,
}

impl PacketHeader {
    /// The origin address, assembled from the appropriate fields. For type-2+
    /// the zone/point come from the extension, and `auxNet` substitutes for
    /// `origNet` when the latter is the `0xFFFF` point sentinel.
    pub fn orig_address(&self) -> FtnAddress {
        match &self.plus {
            Some(p) => {
                let net = if self.orig_net == 0xFFFF {
                    p.aux_net
                } else {
                    self.orig_net
                };
                FtnAddress::new(p.orig_zone, net, self.orig_node, p.orig_point)
            }
            None => FtnAddress::new(self.orig_zone, self.orig_net, self.orig_node, 0),
        }
    }

    /// The destination address, assembled from the appropriate fields.
    pub fn dest_address(&self) -> FtnAddress {
        match &self.plus {
            Some(p) => FtnAddress::new(p.dest_zone, self.dest_net, self.dest_node, p.dest_point),
            None => FtnAddress::new(self.dest_zone, self.dest_net, self.dest_node, 0),
        }
    }

    /// Encode the header to its fixed 58-byte wire form.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN);
        out.extend_from_slice(&self.orig_node.to_le_bytes());
        out.extend_from_slice(&self.dest_node.to_le_bytes());
        out.extend_from_slice(&self.date_time.year.to_le_bytes());
        out.extend_from_slice(&self.date_time.month.to_le_bytes());
        out.extend_from_slice(&self.date_time.day.to_le_bytes());
        out.extend_from_slice(&self.date_time.hour.to_le_bytes());
        out.extend_from_slice(&self.date_time.minute.to_le_bytes());
        out.extend_from_slice(&self.date_time.second.to_le_bytes());
        out.extend_from_slice(&self.baud.to_le_bytes());
        out.extend_from_slice(&PACKET_TYPE_2.to_le_bytes());
        out.extend_from_slice(&self.orig_net.to_le_bytes());
        out.extend_from_slice(&self.dest_net.to_le_bytes());
        out.push(self.product_code_low);
        out.push(self.revision_low);
        out.extend_from_slice(&self.password);
        out.extend_from_slice(&self.orig_zone.to_le_bytes());
        out.extend_from_slice(&self.dest_zone.to_le_bytes());

        match &self.plus {
            Some(p) => {
                out.extend_from_slice(&p.aux_net.to_le_bytes());
                out.extend_from_slice(&p.capability.swap_bytes().to_le_bytes());
                out.push(p.product_code_high);
                out.push(p.revision_high);
                out.extend_from_slice(&p.capability.to_le_bytes());
                out.extend_from_slice(&p.orig_zone.to_le_bytes());
                out.extend_from_slice(&p.dest_zone.to_le_bytes());
                out.extend_from_slice(&p.orig_point.to_le_bytes());
                out.extend_from_slice(&p.dest_point.to_le_bytes());
                out.extend_from_slice(&p.product_data.to_le_bytes());
            }
            None => out.extend_from_slice(&[0u8; 20]),
        }

        debug_assert_eq!(out.len(), HEADER_LEN);
        out
    }

    /// Decode a header from the first [`HEADER_LEN`] bytes of `buf`.
    ///
    /// Returns [`FtnError::Truncated`] if fewer than 58 bytes are available and
    /// [`FtnError::PacketType`] if the type field is not 2.
    pub fn decode(buf: &[u8]) -> Result<Self, FtnError> {
        let mut r = Reader::new(buf);
        let orig_node = r.u16_le()?;
        let dest_node = r.u16_le()?;
        let date_time = DosDateTime {
            year: r.u16_le()?,
            month: r.u16_le()?,
            day: r.u16_le()?,
            hour: r.u16_le()?,
            minute: r.u16_le()?,
            second: r.u16_le()?,
        };
        let baud = r.u16_le()?;
        let pkt_type = r.u16_le()?;
        if pkt_type != PACKET_TYPE_2 {
            return Err(FtnError::PacketType(pkt_type));
        }
        let orig_net = r.u16_le()?;
        let dest_net = r.u16_le()?;
        let product_code_low = r.u8()?;
        let revision_low = r.u8()?;
        let password = r.array::<8>()?;
        let orig_zone = r.u16_le()?;
        let dest_zone = r.u16_le()?;

        // Tail 20 bytes: parse the FSC-0039 extension, then decide.
        let aux_net = r.u16_le()?;
        let cap_valid_copy = r.u16_le()?;
        let product_code_high = r.u8()?;
        let revision_high = r.u8()?;
        let capability = r.u16_le()?;
        let plus_orig_zone = r.u16_le()?;
        let plus_dest_zone = r.u16_le()?;
        let orig_point = r.u16_le()?;
        let dest_point = r.u16_le()?;
        let product_data = r.u32_le()?;

        let is_2plus = capability != 0 && cap_valid_copy == capability.swap_bytes();
        let plus = if is_2plus {
            Some(Type2Plus {
                aux_net,
                capability,
                product_code_high,
                revision_high,
                orig_zone: plus_orig_zone,
                dest_zone: plus_dest_zone,
                orig_point,
                dest_point,
                product_data,
            })
        } else {
            None
        };

        Ok(PacketHeader {
            orig_node,
            dest_node,
            date_time,
            baud,
            orig_net,
            dest_net,
            product_code_low,
            revision_low,
            password,
            orig_zone,
            dest_zone,
            plus,
        })
    }
}

/// A full packet: header plus the packed messages it carries.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Packet {
    /// The 58-byte header.
    pub header: PacketHeader,
    /// Messages packed after the header.
    pub messages: Vec<PackedMessage>,
}

impl Packet {
    /// Encode the whole packet: header, packed messages, `0x0000` terminator.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = self.header.encode();
        out.extend_from_slice(&crate::message::encode_messages(&self.messages));
        out
    }

    /// Decode a whole packet from `buf`.
    pub fn decode(buf: &[u8]) -> Result<Self, FtnError> {
        if buf.len() < HEADER_LEN {
            return Err(FtnError::Truncated {
                at: 0,
                need: HEADER_LEN,
                len: buf.len(),
            });
        }
        let header = PacketHeader::decode(&buf[..HEADER_LEN])?;
        let messages = crate::message::decode_messages(&buf[HEADER_LEN..])?;
        Ok(Packet { header, messages })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_type2() -> PacketHeader {
        PacketHeader {
            orig_node: 464,
            dest_node: 1,
            date_time: DosDateTime {
                year: 2026,
                month: 6, // July (0-based)
                day: 2,
                hour: 13,
                minute: 30,
                second: 45,
            },
            baud: 9600,
            orig_net: 280,
            dest_net: 104,
            product_code_low: 0xFE,
            revision_low: 1,
            password: *b"secret\0\0",
            orig_zone: 2,
            dest_zone: 1,
            plus: None,
        }
    }

    #[test]
    fn type2_golden_bytes() {
        let h = sample_type2();
        let bytes = h.encode();
        assert_eq!(bytes.len(), HEADER_LEN);
        #[rustfmt::skip]
        let expected: Vec<u8> = vec![
            0xD0, 0x01,             // origNode 464
            0x01, 0x00,             // destNode 1
            0xEA, 0x07,             // year 2026
            0x06, 0x00,             // month 6
            0x02, 0x00,             // day 2
            0x0D, 0x00,             // hour 13
            0x1E, 0x00,             // minute 30
            0x2D, 0x00,             // second 45
            0x80, 0x25,             // baud 9600
            0x02, 0x00,             // packet type 2
            0x18, 0x01,             // origNet 280
            0x68, 0x00,             // destNet 104
            0xFE,                   // productCodeLow
            0x01,                   // revisionLow
            b's', b'e', b'c', b'r', b'e', b't', 0x00, 0x00, // password
            0x02, 0x00,             // origZone 2
            0x01, 0x00,             // destZone 1
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // fill (20)
        ];
        assert_eq!(bytes, expected);
    }

    #[test]
    fn type2_roundtrip() {
        let h = sample_type2();
        let decoded = PacketHeader::decode(&h.encode()).unwrap();
        assert_eq!(decoded, h);
        assert!(decoded.plus.is_none());
        assert_eq!(decoded.orig_address().to_string(), "2:280/464");
        assert_eq!(decoded.dest_address().to_string(), "1:104/1");
    }

    #[test]
    fn type2plus_roundtrip_and_detection() {
        let mut h = sample_type2();
        h.orig_net = 0xFFFF; // origin is a point => real net in aux_net
        h.plus = Some(Type2Plus {
            aux_net: 280,
            capability: 0x0001,
            product_code_high: 0xFD,
            revision_high: 2,
            orig_zone: 2,
            dest_zone: 1,
            orig_point: 7,
            dest_point: 0,
            product_data: 0xDEADBEEF,
        });
        let bytes = h.encode();
        let decoded = PacketHeader::decode(&bytes).unwrap();
        assert_eq!(decoded, h);
        let plus = decoded.plus.unwrap();
        assert_eq!(plus.capability, 0x0001);
        // capValidCopy at offset 40 must be the byte-swapped capability word.
        assert_eq!([bytes[40], bytes[41]], 0x0001u16.swap_bytes().to_le_bytes());
        assert_eq!(decoded.orig_address().to_string(), "2:280/464.7");
    }

    #[test]
    fn absent_capability_falls_back_to_type2() {
        // A header whose fill region is non-zero garbage but with a zero
        // capability word must decode as plain type-2.
        let mut h = sample_type2();
        h.plus = Some(Type2Plus {
            aux_net: 1234,
            capability: 0, // absent
            ..Default::default()
        });
        let bytes = h.encode();
        let decoded = PacketHeader::decode(&bytes).unwrap();
        assert!(decoded.plus.is_none());
    }

    #[test]
    fn mismatched_valid_copy_falls_back() {
        let mut bytes = sample_type2().encode();
        // Write a capability word but leave capValidCopy wrong.
        bytes[44] = 0x01;
        bytes[45] = 0x00;
        // capValidCopy (40..42) stays 0x0000 => not the swap of 0x0001.
        let decoded = PacketHeader::decode(&bytes).unwrap();
        assert!(decoded.plus.is_none());
    }

    #[test]
    fn decode_rejects_wrong_type() {
        let mut bytes = sample_type2().encode();
        bytes[18] = 3; // packet type 3
        assert_eq!(PacketHeader::decode(&bytes), Err(FtnError::PacketType(3)));
    }

    #[test]
    fn decode_truncated_is_error_not_panic() {
        let bytes = sample_type2().encode();
        for n in 0..HEADER_LEN {
            assert!(PacketHeader::decode(&bytes[..n]).is_err());
        }
    }
}
