//! Totality sweeps: every decoder must survive arbitrary and truncated bytes
//! without panicking. Uses a deterministic LCG so failures reproduce (std
//! only — no `rand`, no clocks).

#![forbid(unsafe_code)]

use rabbithole_portmap::{natpmp, pcp, upnp};

/// Deterministic 64-bit LCG; the top bits are decently mixed.
struct Lcg(u64);

impl Lcg {
    fn next_u8(&mut self) -> u8 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as u8
    }

    fn buf(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next_u8()).collect()
    }
}

/// Feed a buffer to every total decoder; all Results are intentionally
/// discarded — the only requirement is "no panic".
fn feed_all_decoders(bytes: &[u8]) {
    let _ = natpmp::ExternalAddressResponse::decode(bytes);
    let _ = natpmp::MapRequest::decode(bytes);
    let _ = natpmp::MapResponse::decode(bytes);
    let _ = pcp::MapRequest::decode(bytes);
    let _ = pcp::MapResponse::decode(bytes);
    // Text parsers over the same (possibly non-UTF8) bytes.
    let text = String::from_utf8_lossy(bytes);
    let _ = upnp::parse_location(&text);
    let _ = upnp::parse_external_ip(&text);
    let _ = upnp::parse_soap_result(&text);
}

#[test]
fn random_bytes_never_panic_any_decoder() {
    let mut rng = Lcg(0x5EED_CAFE_1234_5678);
    for _ in 0..5000 {
        let len = usize::from(rng.next_u8()) % 128;
        let buf = rng.buf(len);
        feed_all_decoders(&buf);
    }
}

#[test]
fn every_truncation_of_valid_messages_never_panics() {
    // Build one valid message per codec, then feed every prefix of it.
    let mut messages: Vec<Vec<u8>> = Vec::new();
    messages.push(natpmp::ExternalAddressRequest.encode());
    messages.push(
        natpmp::ExternalAddressResponse {
            result: natpmp::ResultCode::Success,
            epoch: 7,
            external_ip: std::net::Ipv4Addr::new(1, 2, 3, 4),
        }
        .encode(),
    );
    messages.push(
        natpmp::MapRequest {
            protocol: rabbithole_portmap::Protocol::Tcp,
            internal_port: 80,
            suggested_external_port: 8080,
            lifetime_secs: 3600,
        }
        .encode(),
    );
    messages.push(
        natpmp::MapResponse {
            protocol: rabbithole_portmap::Protocol::Udp,
            result: natpmp::ResultCode::OutOfResources,
            epoch: 9,
            internal_port: 1,
            external_port: 2,
            lifetime_secs: 3,
        }
        .encode(),
    );
    messages.push(
        pcp::MapRequest::new_v4(std::net::Ipv4Addr::LOCALHOST, [3; 12], 6, 443, 443, 100).encode(),
    );
    messages.push(
        pcp::MapResponse {
            result: pcp::ResultCode::NoResources,
            lifetime_secs: 1,
            epoch: 2,
            nonce: [4; 12],
            protocol: 17,
            internal_port: 5,
            assigned_external_port: 6,
            assigned_external_ip: std::net::Ipv6Addr::LOCALHOST,
        }
        .encode(),
    );

    for msg in messages {
        for cut in 0..=msg.len() {
            feed_all_decoders(&msg[..cut]);
        }
    }
}

#[test]
fn random_text_never_panics_upnp_parsers() {
    let mut rng = Lcg(0xBADD_ECAF_0000_0001);
    // Bias toward angle brackets and colons so the tag/header scanners are
    // actually exercised on malformed markup.
    let alphabet = b"<>/:\"= \r\nabcNewExternalIPAddressFaulterrorCode0123";
    for _ in 0..3000 {
        let len = usize::from(rng.next_u8()) % 200;
        let s: String = (0..len)
            .map(|_| alphabet[usize::from(rng.next_u8()) % alphabet.len()] as char)
            .collect();
        let _ = upnp::parse_location(&s);
        let _ = upnp::parse_external_ip(&s);
        let _ = upnp::parse_soap_result(&s);
    }
}
