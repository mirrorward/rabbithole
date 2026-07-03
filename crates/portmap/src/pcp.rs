//! PCP codec (RFC 6887) — the Port Control Protocol, NAT-PMP's successor.
//!
//! PCP replaces NAT-PMP on the same gateway UDP port ([`crate::natpmp::PORT`],
//! 5351) with a richer, IPv6-aware format. This module implements the **MAP**
//! opcode — the direct analogue of a NAT-PMP MAP: create/refresh/delete a
//! port mapping for one internal port.
//!
//! ## Version byte distinguishes NAT-PMP from PCP
//!
//! Byte 0 of every PCP message is the version, always [`VERSION`] = **2**.
//! NAT-PMP (see [`crate::natpmp`]) uses version **0** in the same position on
//! the same port, so the leading byte alone tells the two protocols apart. A
//! PCP-only gateway answers a NAT-PMP (v0) request with an error, and vice
//! versa — the [`crate::mapper`] uses that to fall back between them.
//!
//! ## Common header (24 bytes, RFC 6887 §7)
//!
//! ```text
//!   0        1        2        3        4                    8
//!   ┌────────┬────────┬────────┬────────┬────────────────────┐
//!   │ vers=2 │ R|opcode│  (request: reserved 16 bits       ) │  requested/
//!   │        │        │  (response: reserved 8 | result 8 )  │  granted
//!   ├────────┴────────┴────────┴────────┼────────────────────┤  lifetime
//!   │           requested/granted lifetime (u32)             │  (u32)
//!   ├───────────────────────────────────┴────────────────────┤
//!   │  request: client IP (128 bits) │ response: epoch(u32)+  │
//!   │                                 │ reserved(96 bits)      │
//!   └────────────────────────────────────────────────────────┘
//! ```
//!
//! In a request the opcode byte has its top bit (`R`) clear; in a response the
//! top bit is set: response opcode byte = [`RESPONSE_BIT`] | opcode. The last
//! 12 bytes of the header differ by direction: a request carries the client's
//! own IP (128-bit, IPv4 sent as an IPv4-mapped IPv6 address), a response
//! carries a 32-bit epoch and 96 reserved bits.
//!
//! ## MAP opcode-specific payload (36 bytes, RFC 6887 §11.1)
//!
//! ```text
//!   ┌──────────────────────────┐  mapping nonce (96 bits / 12 bytes)
//!   ├────────┬─────────────────┤  protocol (u8, IANA) | reserved (24 bits)
//!   ├────────┴────────┬────────┤  internal port (u16) | suggested ext (u16)
//!   ├─────────────────┴────────┤  suggested external IP (128 bits / 16 bytes)
//!   └──────────────────────────┘
//! ```
//!
//! A full MAP request/response is therefore [`MAP_MSG_LEN`] = 24 + 36 = 60
//! bytes. Decoding is **total**: any short or malformed input yields
//! [`PcpError`], never a panic.

use std::net::{Ipv4Addr, Ipv6Addr};

use thiserror::Error;

/// PCP protocol version — the leading byte of every message.
pub const VERSION: u8 = 2;

/// PCP opcode: MAP (create/refresh/delete a port mapping).
pub const OP_MAP: u8 = 1;

/// Top bit of the opcode byte, set on responses (the `R` bit).
pub const RESPONSE_BIT: u8 = 0x80;

/// Length of the common PCP header.
pub const HEADER_LEN: usize = 24;

/// Length of the MAP opcode-specific payload.
pub const MAP_PAYLOAD_LEN: usize = 36;

/// Length of a full MAP request or response (header + payload).
pub const MAP_MSG_LEN: usize = HEADER_LEN + MAP_PAYLOAD_LEN;

/// Length of the MAP mapping nonce.
pub const NONCE_LEN: usize = 12;

/// A PCP result code (RFC 6887 §7.4). Unknown codes are preserved via
/// [`ResultCode::Other`] so a decoder never rejects an unrecognised value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultCode {
    /// 0 — success.
    Success,
    /// 1 — gateway does not support this PCP version.
    UnsupportedVersion,
    /// 2 — the client is not authorized to make this request.
    NotAuthorized,
    /// 3 — the request was malformed.
    MalformedRequest,
    /// 4 — unsupported opcode.
    UnsupportedOpcode,
    /// 8 — the gateway has no resources for the mapping.
    NoResources,
    /// 9 — the gateway does not support the requested protocol.
    UnsupportedProtocol,
    /// Any other code, kept verbatim (covers the remaining RFC codes and
    /// future ones).
    Other(u8),
}

impl ResultCode {
    /// Interpret the result byte.
    pub const fn from_u8(v: u8) -> Self {
        match v {
            0 => ResultCode::Success,
            1 => ResultCode::UnsupportedVersion,
            2 => ResultCode::NotAuthorized,
            3 => ResultCode::MalformedRequest,
            4 => ResultCode::UnsupportedOpcode,
            8 => ResultCode::NoResources,
            9 => ResultCode::UnsupportedProtocol,
            other => ResultCode::Other(other),
        }
    }

    /// The on-wire byte value.
    pub const fn as_u8(self) -> u8 {
        match self {
            ResultCode::Success => 0,
            ResultCode::UnsupportedVersion => 1,
            ResultCode::NotAuthorized => 2,
            ResultCode::MalformedRequest => 3,
            ResultCode::UnsupportedOpcode => 4,
            ResultCode::NoResources => 8,
            ResultCode::UnsupportedProtocol => 9,
            ResultCode::Other(v) => v,
        }
    }

    /// Whether this code is [`ResultCode::Success`].
    pub const fn is_success(self) -> bool {
        matches!(self, ResultCode::Success)
    }
}

/// Errors from decoding a PCP message. Encoding never fails.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PcpError {
    /// The buffer is shorter than the fixed message length requires.
    #[error("short PCP message: have {have} bytes, need {need}")]
    Short {
        /// Bytes available.
        have: usize,
        /// Bytes the layout requires.
        need: usize,
    },
    /// The version byte was not [`VERSION`] (2). A `0` here means the peer
    /// actually spoke NAT-PMP; see [`crate::natpmp`].
    #[error("unexpected PCP version byte {0} (expected 2)")]
    BadVersion(u8),
    /// The opcode (low 7 bits) was not the expected one.
    #[error("unexpected PCP opcode {0}")]
    BadOpcode(u8),
    /// A response was expected but the R bit was clear (or vice versa).
    #[error("PCP R bit did not match the expected direction")]
    BadDirection,
}

/// A PCP MAP request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapRequest {
    /// Requested lifetime in seconds (0 = delete the mapping).
    pub lifetime_secs: u32,
    /// The client's own IP as the gateway sees it on the LAN (IPv4 supplied as
    /// an IPv4-mapped IPv6 address). The gateway validates it against the
    /// packet source, so it must be the real internal source address.
    pub client_ip: Ipv6Addr,
    /// The 96-bit mapping nonce, chosen by the client; it must be echoed by
    /// the gateway and reused to refresh/delete the same mapping.
    pub nonce: [u8; NONCE_LEN],
    /// The IANA protocol number ([`crate::Protocol::iana`]; 6 = TCP, 17 = UDP,
    /// 0 = all protocols).
    pub protocol: u8,
    /// The internal (LAN-side) port to map.
    pub internal_port: u16,
    /// The external port requested (0 = any).
    pub suggested_external_port: u16,
    /// The external IP requested (all-zeros = any; IPv4 as IPv4-mapped IPv6).
    pub suggested_external_ip: Ipv6Addr,
}

impl MapRequest {
    /// Build a MAP request from an IPv4 client address (the common home case),
    /// leaving the suggested external IP unspecified.
    pub fn new_v4(
        client_ip: Ipv4Addr,
        nonce: [u8; NONCE_LEN],
        protocol: u8,
        internal_port: u16,
        suggested_external_port: u16,
        lifetime_secs: u32,
    ) -> Self {
        Self {
            lifetime_secs,
            client_ip: client_ip.to_ipv6_mapped(),
            nonce,
            protocol,
            internal_port,
            suggested_external_port,
            suggested_external_ip: Ipv6Addr::UNSPECIFIED,
        }
    }

    /// Serialize the 60-byte MAP request.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(MAP_MSG_LEN);
        // Header.
        out.push(VERSION);
        out.push(OP_MAP); // R bit clear = request
        out.extend_from_slice(&[0, 0]); // reserved (16 bits)
        out.extend_from_slice(&self.lifetime_secs.to_be_bytes());
        out.extend_from_slice(&self.client_ip.octets());
        // MAP payload.
        out.extend_from_slice(&self.nonce);
        out.push(self.protocol);
        out.extend_from_slice(&[0, 0, 0]); // reserved (24 bits)
        out.extend_from_slice(&self.internal_port.to_be_bytes());
        out.extend_from_slice(&self.suggested_external_port.to_be_bytes());
        out.extend_from_slice(&self.suggested_external_ip.octets());
        out
    }

    /// Decode a 60-byte MAP request (for tests/mock gateways). Total.
    pub fn decode(buf: &[u8]) -> Result<Self, PcpError> {
        if buf.len() < MAP_MSG_LEN {
            return Err(PcpError::Short {
                have: buf.len(),
                need: MAP_MSG_LEN,
            });
        }
        if buf[0] != VERSION {
            return Err(PcpError::BadVersion(buf[0]));
        }
        if buf[1] & RESPONSE_BIT != 0 {
            return Err(PcpError::BadDirection);
        }
        if buf[1] & !RESPONSE_BIT != OP_MAP {
            return Err(PcpError::BadOpcode(buf[1] & !RESPONSE_BIT));
        }
        let lifetime_secs = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let client_ip = read_ip16(&buf[8..24]);
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&buf[24..36]);
        let protocol = buf[36];
        let internal_port = u16::from_be_bytes([buf[40], buf[41]]);
        let suggested_external_port = u16::from_be_bytes([buf[42], buf[43]]);
        let suggested_external_ip = read_ip16(&buf[44..60]);
        Ok(Self {
            lifetime_secs,
            client_ip,
            nonce,
            protocol,
            internal_port,
            suggested_external_port,
            suggested_external_ip,
        })
    }
}

/// A PCP MAP response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MapResponse {
    /// Result code.
    pub result: ResultCode,
    /// The lifetime in seconds the gateway granted (may be shorter than
    /// requested; on an error this is how long to wait before retrying).
    pub lifetime_secs: u32,
    /// The gateway's epoch time; a discontinuity signals a reboot and that
    /// mappings must be re-created.
    pub epoch: u32,
    /// The mapping nonce echoed back (must match the request's).
    pub nonce: [u8; NONCE_LEN],
    /// The IANA protocol number echoed back.
    pub protocol: u8,
    /// The internal port echoed back.
    pub internal_port: u16,
    /// The external port the gateway assigned.
    pub assigned_external_port: u16,
    /// The external IP the gateway assigned (IPv4 as IPv4-mapped IPv6).
    pub assigned_external_ip: Ipv6Addr,
}

impl MapResponse {
    /// Serialize the 60-byte MAP response (mostly for tests/mock gateways).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(MAP_MSG_LEN);
        // Header.
        out.push(VERSION);
        out.push(OP_MAP | RESPONSE_BIT); // R bit set = response
        out.push(0); // reserved (8 bits)
        out.push(self.result.as_u8());
        out.extend_from_slice(&self.lifetime_secs.to_be_bytes());
        out.extend_from_slice(&self.epoch.to_be_bytes());
        out.extend_from_slice(&[0u8; 12]); // reserved (96 bits)
                                           // MAP payload.
        out.extend_from_slice(&self.nonce);
        out.push(self.protocol);
        out.extend_from_slice(&[0, 0, 0]); // reserved (24 bits)
        out.extend_from_slice(&self.internal_port.to_be_bytes());
        out.extend_from_slice(&self.assigned_external_port.to_be_bytes());
        out.extend_from_slice(&self.assigned_external_ip.octets());
        out
    }

    /// Decode a 60-byte MAP response. Total: short or malformed input yields
    /// [`PcpError`].
    pub fn decode(buf: &[u8]) -> Result<Self, PcpError> {
        if buf.len() < MAP_MSG_LEN {
            return Err(PcpError::Short {
                have: buf.len(),
                need: MAP_MSG_LEN,
            });
        }
        if buf[0] != VERSION {
            return Err(PcpError::BadVersion(buf[0]));
        }
        if buf[1] & RESPONSE_BIT == 0 {
            return Err(PcpError::BadDirection);
        }
        if buf[1] & !RESPONSE_BIT != OP_MAP {
            return Err(PcpError::BadOpcode(buf[1] & !RESPONSE_BIT));
        }
        let result = ResultCode::from_u8(buf[3]);
        let lifetime_secs = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let epoch = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
        // buf[12..24] reserved.
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&buf[24..36]);
        let protocol = buf[36];
        let internal_port = u16::from_be_bytes([buf[40], buf[41]]);
        let assigned_external_port = u16::from_be_bytes([buf[42], buf[43]]);
        let assigned_external_ip = read_ip16(&buf[44..60]);
        Ok(Self {
            result,
            lifetime_secs,
            epoch,
            nonce,
            protocol,
            internal_port,
            assigned_external_port,
            assigned_external_ip,
        })
    }
}

/// Read a 16-byte IPv6 address from a slice known to be exactly 16 bytes.
fn read_ip16(bytes: &[u8]) -> Ipv6Addr {
    let mut octets = [0u8; 16];
    octets.copy_from_slice(bytes);
    Ipv6Addr::from(octets)
}

/// Convenience: interpret a PCP 16-byte address as an IPv4 if it is in the
/// IPv4-mapped range, else keep it as IPv6.
pub fn ip16_to_ipaddr(ip: Ipv6Addr) -> std::net::IpAddr {
    match ip.to_ipv4_mapped() {
        Some(v4) => std::net::IpAddr::V4(v4),
        None => std::net::IpAddr::V6(ip),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_request_round_trips() {
        let req = MapRequest::new_v4(
            Ipv4Addr::new(192, 168, 1, 50),
            [0xAA; NONCE_LEN],
            17, // UDP
            4653,
            4653,
            7200,
        );
        let bytes = req.encode();
        assert_eq!(bytes.len(), MAP_MSG_LEN);
        assert_eq!(MapRequest::decode(&bytes).unwrap(), req);
    }

    #[test]
    fn map_response_round_trips() {
        let resp = MapResponse {
            result: ResultCode::Success,
            lifetime_secs: 3600,
            epoch: 42,
            nonce: [0xBB; NONCE_LEN],
            protocol: 6, // TCP
            internal_port: 4654,
            assigned_external_port: 51000,
            assigned_external_ip: Ipv4Addr::new(203, 0, 113, 9).to_ipv6_mapped(),
        };
        let bytes = resp.encode();
        assert_eq!(bytes.len(), MAP_MSG_LEN);
        let got = MapResponse::decode(&bytes).unwrap();
        assert_eq!(got, resp);
        assert_eq!(
            ip16_to_ipaddr(got.assigned_external_ip),
            std::net::IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9))
        );
    }

    #[test]
    fn direction_and_version_are_checked() {
        let req = MapRequest::new_v4(Ipv4Addr::LOCALHOST, [0; NONCE_LEN], 17, 1, 1, 1);
        let bytes = req.encode();
        // Decoding a request as a response must reject on direction.
        assert_eq!(MapResponse::decode(&bytes), Err(PcpError::BadDirection));
        // NAT-PMP (v0) bytes fed to PCP reject on version.
        let mut v0 = bytes.clone();
        v0[0] = 0;
        assert_eq!(MapRequest::decode(&v0), Err(PcpError::BadVersion(0)));
        // Short buffers never panic.
        assert!(matches!(
            MapResponse::decode(&[2, 0x81, 0]),
            Err(PcpError::Short { .. })
        ));
    }
}
