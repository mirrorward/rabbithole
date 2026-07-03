//! NAT-PMP codec (RFC 6886) — the NAT Port Mapping Protocol.
//!
//! NAT-PMP is a tiny binary protocol a host uses to ask its NAT gateway (on
//! UDP port [`PORT`]) for its public address and to create UDP/TCP port
//! mappings. Every multi-byte field is **network byte order** (big-endian).
//!
//! ## Version byte
//!
//! The first byte of every NAT-PMP message is the version, always
//! [`VERSION`] = 0. Its successor PCP (see [`crate::pcp`]) uses version 2 in
//! the same byte position on the same UDP port, so a receiver distinguishes
//! the two protocols by this leading byte: `0` → NAT-PMP, `2` → PCP.
//!
//! ## Message layouts
//!
//! ```text
//! External-address request (2 bytes):
//!   0        1
//!   ┌────────┬────────┐
//!   │ vers=0 │ op = 0 │
//!   └────────┴────────┘
//!
//! External-address response (12 bytes):
//!   0        1        2        4                    8                    12
//!   ┌────────┬────────┬────────┬────────────────────┬────────────────────┐
//!   │ vers=0 │ op=128 │ result │  seconds-of-epoch   │  external IPv4      │
//!   └────────┴────────┴─(u16)──┴──────(u32)──────────┴──────(u32)─────────┘
//!
//! MAP request (12 bytes):        op = 1 (UDP) or 2 (TCP)
//!   0        1        2        4                6                8                12
//!   ┌────────┬────────┬────────┬────────────────┬────────────────┬────────────────┐
//!   │ vers=0 │ op     │ resv=0 │ internal port  │ suggested ext  │  lifetime secs │
//!   └────────┴────────┴─(u16)──┴─────(u16)──────┴─────(u16)──────┴─────(u32)───────┘
//!
//! MAP response (16 bytes):       op = 129 (UDP) or 130 (TCP)
//!   0        1        2        4                8                10               12          16
//!   ┌────────┬────────┬────────┬────────────────┬────────────────┬────────────────┬───────────┐
//!   │ vers=0 │ op     │ result │ seconds-of-epoch│ internal port  │ mapped ext port│ lifetime  │
//!   └────────┴────────┴─(u16)──┴─────(u32)───────┴─────(u16)──────┴─────(u16)──────┴──(u32)─────┘
//! ```
//!
//! In a response the opcode has the high bit set: response op = request op +
//! [`RESPONSE_BIT`] (128). Decoding is **total**: any short or malformed input
//! yields [`NatPmpError`], never a panic.

use std::net::Ipv4Addr;

use thiserror::Error;

use crate::Protocol;

/// NAT-PMP protocol version — the leading byte of every message.
pub const VERSION: u8 = 0;

/// Gateway UDP port NAT-PMP (and PCP) requests are sent to.
pub const PORT: u16 = 5351;

/// Opcode: request the external (public) IPv4 address.
pub const OP_EXTERNAL: u8 = 0;
/// Opcode: map a UDP port.
pub const OP_MAP_UDP: u8 = 1;
/// Opcode: map a TCP port.
pub const OP_MAP_TCP: u8 = 2;

/// Added to a request opcode to form the matching response opcode (the high
/// bit of the opcode byte marks a response).
pub const RESPONSE_BIT: u8 = 0x80;

/// A NAT-PMP result code (RFC 6886 §3.5). Unknown codes are preserved via
/// [`ResultCode::Other`] so a decoder never rejects an unrecognised value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultCode {
    /// 0 — success.
    Success,
    /// 1 — the gateway does not support this NAT-PMP version.
    UnsupportedVersion,
    /// 2 — the operation is refused (e.g. box configured to deny mappings).
    NotAuthorized,
    /// 3 — the gateway cannot reach its own external network.
    NetworkFailure,
    /// 4 — the gateway is out of resources (too many existing mappings).
    OutOfResources,
    /// 5 — the gateway does not support this opcode.
    UnsupportedOpcode,
    /// Any other (reserved / future) code, kept verbatim.
    Other(u16),
}

impl ResultCode {
    /// Interpret the 16-bit result field.
    pub const fn from_u16(v: u16) -> Self {
        match v {
            0 => ResultCode::Success,
            1 => ResultCode::UnsupportedVersion,
            2 => ResultCode::NotAuthorized,
            3 => ResultCode::NetworkFailure,
            4 => ResultCode::OutOfResources,
            5 => ResultCode::UnsupportedOpcode,
            other => ResultCode::Other(other),
        }
    }

    /// The on-wire 16-bit value.
    pub const fn as_u16(self) -> u16 {
        match self {
            ResultCode::Success => 0,
            ResultCode::UnsupportedVersion => 1,
            ResultCode::NotAuthorized => 2,
            ResultCode::NetworkFailure => 3,
            ResultCode::OutOfResources => 4,
            ResultCode::UnsupportedOpcode => 5,
            ResultCode::Other(v) => v,
        }
    }

    /// Whether this code is [`ResultCode::Success`].
    pub const fn is_success(self) -> bool {
        matches!(self, ResultCode::Success)
    }
}

/// Errors from decoding a NAT-PMP message. Encoding never fails.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NatPmpError {
    /// The buffer is shorter than the fixed message length requires.
    #[error("short NAT-PMP message: have {have} bytes, need {need}")]
    Short {
        /// Bytes available.
        have: usize,
        /// Bytes the layout requires.
        need: usize,
    },
    /// The version byte was not [`VERSION`] (0). A `2` here means the peer
    /// actually spoke PCP; see [`crate::pcp`].
    #[error("unexpected NAT-PMP version byte {0} (expected 0)")]
    BadVersion(u8),
    /// The opcode did not have the response bit set, or was not one of the
    /// expected response opcodes.
    #[error("unexpected NAT-PMP response opcode {0}")]
    BadOpcode(u8),
}

/// External-address request (opcode 0). Encodes to the fixed 2 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExternalAddressRequest;

impl ExternalAddressRequest {
    /// Serialize the 2-byte request: `[version, OP_EXTERNAL]`.
    pub fn encode(&self) -> Vec<u8> {
        vec![VERSION, OP_EXTERNAL]
    }
}

/// External-address response (opcode 128).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExternalAddressResponse {
    /// Result code.
    pub result: ResultCode,
    /// Gateway's seconds-since-epoch counter (its uptime; a reset signals the
    /// gateway rebooted and mappings must be re-established).
    pub epoch: u32,
    /// The public IPv4 address the gateway presents to the internet.
    pub external_ip: Ipv4Addr,
}

impl ExternalAddressResponse {
    /// Fixed on-wire length.
    pub const LEN: usize = 12;

    /// Serialize the 12-byte response (mostly for tests/mock gateways).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::LEN);
        out.push(VERSION);
        out.push(OP_EXTERNAL + RESPONSE_BIT);
        out.extend_from_slice(&self.result.as_u16().to_be_bytes());
        out.extend_from_slice(&self.epoch.to_be_bytes());
        out.extend_from_slice(&self.external_ip.octets());
        out
    }

    /// Decode a 12-byte external-address response. Total: short or malformed
    /// input yields [`NatPmpError`].
    pub fn decode(buf: &[u8]) -> Result<Self, NatPmpError> {
        if buf.len() < Self::LEN {
            return Err(NatPmpError::Short {
                have: buf.len(),
                need: Self::LEN,
            });
        }
        if buf[0] != VERSION {
            return Err(NatPmpError::BadVersion(buf[0]));
        }
        if buf[1] != OP_EXTERNAL + RESPONSE_BIT {
            return Err(NatPmpError::BadOpcode(buf[1]));
        }
        let result = ResultCode::from_u16(u16::from_be_bytes([buf[2], buf[3]]));
        let epoch = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let external_ip = Ipv4Addr::new(buf[8], buf[9], buf[10], buf[11]);
        Ok(Self {
            result,
            epoch,
            external_ip,
        })
    }
}

/// MAP request (opcode 1 = UDP, 2 = TCP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapRequest {
    /// Which transport to map.
    pub protocol: Protocol,
    /// The internal (LAN-side) port we are listening on.
    pub internal_port: u16,
    /// The external port we would like assigned (0 = "any"; conventionally the
    /// same as the internal port).
    pub suggested_external_port: u16,
    /// Requested lifetime of the mapping in seconds (0 = delete this mapping).
    pub lifetime_secs: u32,
}

impl MapRequest {
    /// Fixed on-wire length.
    pub const LEN: usize = 12;

    /// The request opcode for this protocol (1 = UDP, 2 = TCP).
    pub const fn opcode(&self) -> u8 {
        match self.protocol {
            Protocol::Udp => OP_MAP_UDP,
            Protocol::Tcp => OP_MAP_TCP,
        }
    }

    /// Serialize the 12-byte MAP request.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::LEN);
        out.push(VERSION);
        out.push(self.opcode());
        out.extend_from_slice(&0u16.to_be_bytes()); // reserved, must be 0
        out.extend_from_slice(&self.internal_port.to_be_bytes());
        out.extend_from_slice(&self.suggested_external_port.to_be_bytes());
        out.extend_from_slice(&self.lifetime_secs.to_be_bytes());
        out
    }

    /// Decode a 12-byte MAP request (for tests/mock gateways). Total.
    pub fn decode(buf: &[u8]) -> Result<Self, NatPmpError> {
        if buf.len() < Self::LEN {
            return Err(NatPmpError::Short {
                have: buf.len(),
                need: Self::LEN,
            });
        }
        if buf[0] != VERSION {
            return Err(NatPmpError::BadVersion(buf[0]));
        }
        let protocol = match buf[1] {
            OP_MAP_UDP => Protocol::Udp,
            OP_MAP_TCP => Protocol::Tcp,
            other => return Err(NatPmpError::BadOpcode(other)),
        };
        let internal_port = u16::from_be_bytes([buf[4], buf[5]]);
        let suggested_external_port = u16::from_be_bytes([buf[6], buf[7]]);
        let lifetime_secs = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
        Ok(Self {
            protocol,
            internal_port,
            suggested_external_port,
            lifetime_secs,
        })
    }
}

/// MAP response (opcode 129 = UDP, 130 = TCP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapResponse {
    /// Which transport was mapped.
    pub protocol: Protocol,
    /// Result code.
    pub result: ResultCode,
    /// Gateway's seconds-since-epoch counter.
    pub epoch: u32,
    /// Internal port echoed back.
    pub internal_port: u16,
    /// The external port the gateway actually assigned (may differ from the
    /// suggested one).
    pub external_port: u16,
    /// The lifetime in seconds the gateway granted (may be shorter than
    /// requested).
    pub lifetime_secs: u32,
}

impl MapResponse {
    /// Fixed on-wire length.
    pub const LEN: usize = 16;

    /// The response opcode for this protocol (129 = UDP, 130 = TCP).
    pub const fn opcode(&self) -> u8 {
        RESPONSE_BIT
            + match self.protocol {
                Protocol::Udp => OP_MAP_UDP,
                Protocol::Tcp => OP_MAP_TCP,
            }
    }

    /// Serialize the 16-byte MAP response (mostly for tests/mock gateways).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(Self::LEN);
        out.push(VERSION);
        out.push(self.opcode());
        out.extend_from_slice(&self.result.as_u16().to_be_bytes());
        out.extend_from_slice(&self.epoch.to_be_bytes());
        out.extend_from_slice(&self.internal_port.to_be_bytes());
        out.extend_from_slice(&self.external_port.to_be_bytes());
        out.extend_from_slice(&self.lifetime_secs.to_be_bytes());
        out
    }

    /// Decode a 16-byte MAP response. Total: short or malformed input yields
    /// [`NatPmpError`].
    pub fn decode(buf: &[u8]) -> Result<Self, NatPmpError> {
        if buf.len() < Self::LEN {
            return Err(NatPmpError::Short {
                have: buf.len(),
                need: Self::LEN,
            });
        }
        if buf[0] != VERSION {
            return Err(NatPmpError::BadVersion(buf[0]));
        }
        let protocol = match buf[1] {
            x if x == OP_MAP_UDP + RESPONSE_BIT => Protocol::Udp,
            x if x == OP_MAP_TCP + RESPONSE_BIT => Protocol::Tcp,
            other => return Err(NatPmpError::BadOpcode(other)),
        };
        let result = ResultCode::from_u16(u16::from_be_bytes([buf[2], buf[3]]));
        let epoch = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let internal_port = u16::from_be_bytes([buf[8], buf[9]]);
        let external_port = u16::from_be_bytes([buf[10], buf[11]]);
        let lifetime_secs = u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]);
        Ok(Self {
            protocol,
            result,
            epoch,
            internal_port,
            external_port,
            lifetime_secs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_request_is_two_bytes() {
        assert_eq!(ExternalAddressRequest.encode(), vec![0, 0]);
    }

    #[test]
    fn external_response_round_trips() {
        let resp = ExternalAddressResponse {
            result: ResultCode::Success,
            epoch: 123,
            external_ip: Ipv4Addr::new(203, 0, 113, 5),
        };
        let bytes = resp.encode();
        assert_eq!(bytes.len(), ExternalAddressResponse::LEN);
        assert_eq!(ExternalAddressResponse::decode(&bytes).unwrap(), resp);
    }

    #[test]
    fn map_request_round_trips_both_protocols() {
        for protocol in [Protocol::Udp, Protocol::Tcp] {
            let req = MapRequest {
                protocol,
                internal_port: 4653,
                suggested_external_port: 4653,
                lifetime_secs: 7200,
            };
            let bytes = req.encode();
            assert_eq!(bytes.len(), MapRequest::LEN);
            assert_eq!(MapRequest::decode(&bytes).unwrap(), req);
        }
    }

    #[test]
    fn map_response_round_trips_both_protocols() {
        for protocol in [Protocol::Udp, Protocol::Tcp] {
            let resp = MapResponse {
                protocol,
                result: ResultCode::Success,
                epoch: 999,
                internal_port: 4653,
                external_port: 34567,
                lifetime_secs: 3600,
            };
            let bytes = resp.encode();
            assert_eq!(bytes.len(), MapResponse::LEN);
            assert_eq!(MapResponse::decode(&bytes).unwrap(), resp);
        }
    }

    #[test]
    fn short_and_bad_version_never_panic() {
        assert!(matches!(
            MapResponse::decode(&[0, 129, 0]),
            Err(NatPmpError::Short { .. })
        ));
        // version 2 = PCP spoken on the shared port.
        let mut b = MapResponse {
            protocol: Protocol::Udp,
            result: ResultCode::Success,
            epoch: 0,
            internal_port: 1,
            external_port: 2,
            lifetime_secs: 3,
        }
        .encode();
        b[0] = 2;
        assert_eq!(MapResponse::decode(&b), Err(NatPmpError::BadVersion(2)));
    }
}
