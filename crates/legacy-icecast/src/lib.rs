//! Icecast/SHOUTcast (ICY) protocol codec for RabbitHole's radio (Wave 11).
//!
//! This crate is the pure protocol seam between DJ encoders, media players, and
//! RabbitHole's [`rabbithole-audio`](https://docs.rs) core: it parses and
//! renders the ICY wire format and does the fiddly in-band metadata math, but
//! owns no sockets, no audio codecs, and no server state. It pairs with (and
//! deliberately does not depend on) the audio crate — a host wires the two
//! together in a later slice.
//!
//! Three surfaces, in signal order:
//!
//! - **Source (DJ push)** — [`parse_source_request`] decodes a classic
//!   SHOUTcast `SOURCE` or an Icecast 2 `PUT`, including `Authorization: Basic`
//!   and the `ice-*`/`icy-*` station headers, into a [`SourceRequest`].
//!   [`source_ok`], [`source_unauthorized`], and [`source_forbidden`] build the
//!   server's replies.
//! - **Listener (player pull)** — [`parse_listener_request`] reads the client
//!   `GET` (and its `Icy-MetaData: 1` opt-in); [`build_listener_response`]
//!   renders the `ICY 200 OK` head with the station's `icy-*` headers and the
//!   negotiated `icy-metaint`.
//! - **Metadata interleaving** — [`IcyMetaInterleaver`] and [`format_metadata`]
//!   splice `StreamTitle='…';` blocks into the audio byte stream at exact
//!   `icy-metaint` boundaries, emitting the single `0x00` byte when nothing
//!   changed.
//!
//! Every parser is total: malformed input yields an [`IcyError`], never a
//! panic.
//!
//! ```
//! use rabbithole_legacy_icecast::{
//!     build_listener_response, parse_listener_request, IcyMetaInterleaver, StationMeta,
//!     DEFAULT_METAINT,
//! };
//!
//! let req = parse_listener_request(b"GET /live HTTP/1.0\r\nIcy-MetaData: 1\r\n\r\n").unwrap();
//! assert!(req.wants_metadata);
//!
//! let meta = StationMeta { name: "Warren FM".into(), ..StationMeta::default() };
//! let metaint = req.wants_metadata.then_some(DEFAULT_METAINT);
//! let head = build_listener_response(&meta, "audio/mpeg", metaint);
//! assert!(head.contains("icy-metaint:8192\r\n"));
//!
//! let mut weaver = IcyMetaInterleaver::new(DEFAULT_METAINT);
//! weaver.set_title("Artist - Track");
//! let _wire = weaver.push(&[0u8; DEFAULT_METAINT]); // audio + one metadata block
//! ```

#![forbid(unsafe_code)]

mod http;
mod listener;
mod meta;
mod metaint;
mod source;

pub use listener::{build_listener_response, parse_listener_request, ListenerRequest};
pub use meta::StationMeta;
pub use metaint::{format_metadata, IcyMetaInterleaver, DEFAULT_METAINT};
pub use source::{
    parse_basic_auth, parse_source_request, source_forbidden, source_ok, source_unauthorized,
    SourceMethod, SourceRequest,
};

/// Errors produced while parsing ICY requests.
///
/// These cover only structurally unusable input; missing-but-optional headers
/// (auth, station metadata) are tolerated by the parsers and surfaced as empty
/// fields so the caller owns the auth/policy decision.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IcyError {
    /// The request contained no request line at all.
    #[error("empty request")]
    EmptyRequest,
    /// The request line was missing a method and/or target token.
    #[error("malformed request line")]
    MalformedRequestLine,
    /// The request method is not one this surface handles.
    #[error("unsupported method: {0}")]
    UnsupportedMethod(String),
}

#[cfg(test)]
mod fuzzish_tests {
    use super::*;

    /// A small deterministic pseudo-random byte generator (SplitMix64) so the
    /// fuzz-ish sweep needs no dev-dependency.
    struct Rng(u64);
    impl Rng {
        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn byte(&mut self) -> u8 {
            (self.next_u64() & 0xFF) as u8
        }
    }

    #[test]
    fn parsers_never_panic_on_random_bytes() {
        let mut rng = Rng(0xDEAD_BEEF);
        for _ in 0..5_000 {
            let len = (rng.next_u64() % 256) as usize;
            let buf: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
            // None of these may panic regardless of input.
            let _ = parse_source_request(&buf);
            let _ = parse_listener_request(&buf);
            let text = String::from_utf8_lossy(&buf);
            let _ = parse_basic_auth(&text);
        }
    }

    #[test]
    fn parsers_never_panic_on_structured_junk() {
        let cases: &[&[u8]] = &[
            b"",
            b"\r\n",
            b"   \r\n\r\n",
            b"SOURCE",
            b"SOURCE ",
            b"PUT /m",
            b"GET /m HTTP/1.1\r\nIcy-MetaData",
            b"PUT /m HTTP/1.1\r\nAuthorization: Basic\r\n\r\n",
            b"PUT /m HTTP/1.1\r\nAuthorization: Basic \xff\xfe\r\n\r\n",
            b"SOURCE \xff\xfe\xfd /m\r\n\r\n",
            b":::::\r\n:::::\r\n",
            b"GET /\0\0\0 HTTP/1.0\r\nIcy-MetaData: 1\r\n",
        ];
        for case in cases {
            let _ = parse_source_request(case);
            let _ = parse_listener_request(case);
        }
    }

    #[test]
    fn interleaver_never_panics_on_arbitrary_metaint_and_chunks() {
        let mut rng = Rng(0x1234_5678);
        for _ in 0..500 {
            let metaint = (rng.next_u64() % 64) as usize; // includes 0 -> clamps to 1
            let mut w = IcyMetaInterleaver::new(metaint);
            for _ in 0..5 {
                if rng.byte() & 1 == 0 {
                    w.set_title(format!("t{}", rng.byte()));
                }
                let n = (rng.next_u64() % 200) as usize;
                let chunk: Vec<u8> = (0..n).map(|_| rng.byte()).collect();
                let _ = w.push(&chunk);
            }
        }
    }
}
