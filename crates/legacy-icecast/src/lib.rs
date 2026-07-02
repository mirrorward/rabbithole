//! Icecast/SHOUTcast (ICY) protocol codec for RabbitHole's radio (Wave 11).
//!
//! This crate is the pure protocol seam between DJ encoders, media players, and
//! RabbitHole's [`rabbithole-audio`](https://docs.rs) core: it parses and
//! renders the ICY wire format and does the fiddly in-band metadata math, but
//! owns no sockets, no audio codecs, and no server state. It pairs with (and
//! deliberately does not depend on) the audio crate — a host wires the two
//! together in a later slice.
//!
//! Five surfaces, in signal order:
//!
//! - **Source (DJ push)** — [`parse_source_request`] decodes a classic
//!   SHOUTcast `SOURCE` or an Icecast 2 `PUT`, including `Authorization: Basic`
//!   and the `ice-*`/`icy-*` station headers, into a [`SourceRequest`].
//!   [`source_ok`], [`source_unauthorized`], and [`source_forbidden`] build the
//!   server's replies.
//! - **Mid-stream updates (admin)** — [`parse_metadata_update`] decodes the
//!   `GET /admin/metadata?mode=updinfo&song=…` title-change request (and the
//!   SHOUTcast v1 `admin.cgi` spelling) into a [`MetadataUpdate`], with query
//!   [`percent_decode`]-ing and both `pass=` and Basic-auth credential forms;
//!   [`metadata_update_ok`], [`metadata_update_failed`], and
//!   [`metadata_update_unauthorized`] build the replies.
//! - **Listener (player pull)** — [`parse_listener_request`] reads the client
//!   `GET` (and its `Icy-MetaData: 1` opt-in); [`build_listener_response`]
//!   renders the `ICY 200 OK` head with the station's `icy-*` headers and the
//!   negotiated `icy-metaint`.
//! - **Metadata interleaving** — [`IcyMetaInterleaver`] and [`format_metadata`]
//!   splice `StreamTitle='…';` blocks into the audio byte stream at exact
//!   `icy-metaint` boundaries, emitting the single `0x00` byte when nothing
//!   changed. [`encode_stream_title`] / [`parse_stream_title`] are the pure
//!   payload codec underneath.
//! - **Metadata de-interleaving (player side)** — [`MetaintReader`] and
//!   [`parse_metaint_stream`] run the inverse: strip the in-band blocks back
//!   out of a received stream, yielding clean audio plus decoded
//!   [`StreamTitle`] updates.
//!
//! Every parser is total: malformed input yields an [`IcyError`], never a
//! panic.
//!
//! ```
//! use rabbithole_legacy_icecast::{
//!     build_listener_response, parse_listener_request, IcyMetaInterleaver, MetaintReader,
//!     StationMeta, DEFAULT_METAINT,
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
//! let wire = weaver.push(&[0u8; DEFAULT_METAINT]); // audio + one metadata block
//!
//! // …and the player side recovers both from the wire bytes.
//! let mut reader = MetaintReader::new(DEFAULT_METAINT);
//! let chunk = reader.push(&wire);
//! assert_eq!(chunk.audio, vec![0u8; DEFAULT_METAINT]);
//! assert_eq!(chunk.updates[0].title, "Artist - Track");
//! ```

#![forbid(unsafe_code)]

mod admin;
mod http;
mod listener;
mod meta;
mod metaint;
mod metaread;
mod source;

pub use admin::{
    metadata_update_failed, metadata_update_ok, metadata_update_unauthorized,
    parse_metadata_update, percent_decode, MetadataUpdate,
};
pub use listener::{build_listener_response, parse_listener_request, ListenerRequest};
pub use meta::StationMeta;
pub use metaint::{
    encode_stream_title, format_metadata, parse_stream_title, IcyMetaInterleaver, StreamTitle,
    DEFAULT_METAINT,
};
pub use metaread::{parse_metaint_stream, DeinterleavedChunk, MetaintReader};
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
    /// The request target is not a recognised admin metadata endpoint
    /// (`/admin/metadata` or `/admin.cgi`).
    #[error("unsupported target: {0}")]
    UnsupportedTarget(String),
    /// The admin request's `mode` parameter is missing or not `updinfo`.
    #[error("unsupported admin mode: {0}")]
    UnsupportedMode(String),
    /// A required query parameter (e.g. `song`) was absent.
    #[error("missing required parameter: {0}")]
    MissingParameter(String),
    /// An in-band metadata payload carried no `StreamTitle='…'` attribute.
    #[error("malformed metadata block")]
    MalformedMetadata,
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
            let _ = parse_metadata_update(&buf);
            let _ = parse_stream_title(&buf);
            let text = String::from_utf8_lossy(&buf);
            let _ = parse_basic_auth(&text);
            let _ = percent_decode(&text);
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
            b"GET /admin/metadata HTTP/1.0\r\n\r\n",
            b"GET /admin/metadata? HTTP/1.0\r\n\r\n",
            b"GET /admin.cgi?mode=updinfo&song HTTP/1.0\r\n\r\n",
            b"GET /admin/metadata?mode=updinfo&song=%%%%&pass=%2 HTTP/1.0\r\n\r\n",
            b"GET /admin/metadata?&&&==&=x&mode=updinfo&song=a HTTP/1.0\r\n\r\n",
        ];
        for case in cases {
            let _ = parse_source_request(case);
            let _ = parse_listener_request(case);
            let _ = parse_metadata_update(case);
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

    #[test]
    fn metaint_reader_never_panics_on_random_bytes() {
        let mut rng = Rng(0xFEED_FACE);
        for _ in 0..500 {
            let metaint = (rng.next_u64() % 64) as usize; // includes 0 -> clamps to 1
            let mut r = MetaintReader::new(metaint);
            for _ in 0..5 {
                let n = (rng.next_u64() % 200) as usize;
                let chunk: Vec<u8> = (0..n).map(|_| rng.byte()).collect();
                let _ = r.push(&chunk);
            }
        }
    }
}
