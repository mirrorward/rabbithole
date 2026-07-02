//! Source (DJ push) protocol: parse a `SOURCE`/`PUT` connect, decode Basic
//! auth and `ice-*`/`icy-*` metadata, and build the server's response.
//!
//! Two dialects converge here:
//!
//! - **Classic SHOUTcast/ICY** — `SOURCE /mount ICE/1.0` (and the ancient
//!   `SOURCE <password> /mount` form where the password sits inline on the
//!   request line), answered with `OK2`.
//! - **Icecast 2** — `PUT /mount HTTP/1.1`, an ordinary HTTP request answered
//!   with `HTTP/1.0 200 OK`.
//!
//! Both authenticate with an `Authorization: Basic <base64(user:pass)>` header
//! and advertise the station via `ice-name`, `ice-genre`, `ice-url`,
//! `ice-bitrate`, `ice-public`, and `content-type` (the `icy-` spellings are
//! accepted too). Parsing is total: any malformed input yields an
//! [`IcyError`], never a panic.

use data_encoding::{BASE64, BASE64_NOPAD};

use crate::http::{split_header, RequestHead};
use crate::meta::StationMeta;
use crate::IcyError;

/// Which source dialect a request used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceMethod {
    /// Classic SHOUTcast/ICY `SOURCE`.
    Source,
    /// Icecast 2 `PUT` (HTTP source).
    Put,
}

/// A parsed source (DJ) connect request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRequest {
    /// Mount point the source is publishing to, e.g. `/live`.
    pub mount: String,
    /// Dialect used ([`SourceMethod::Source`] or [`SourceMethod::Put`]).
    pub method: SourceMethod,
    /// Username from Basic auth (empty if none supplied).
    pub user: String,
    /// Password from Basic auth (or the inline SHOUTcast-v1 password).
    pub pass: String,
    /// Station metadata advertised by the source.
    pub metadata: StationMeta,
    /// Stream content type (`content-type`), defaulting to `audio/mpeg`.
    pub content_type: String,
}

/// Parses a source connect request from its raw request bytes (request line
/// plus headers, up to and optionally including the blank-line terminator).
///
/// Returns [`IcyError`] on an empty request, an unrecognised method, or a
/// malformed request line. Missing headers are tolerated — unauthenticated
/// requests parse fine (with empty credentials) so the caller can decide the
/// auth policy and answer with [`source_unauthorized`].
pub fn parse_source_request(raw: &[u8]) -> Result<SourceRequest, IcyError> {
    let head = RequestHead::parse(raw)?;

    let (method, mount) = match head.method.to_ascii_uppercase().as_str() {
        "SOURCE" => (SourceMethod::Source, head.target.clone()),
        "PUT" => (SourceMethod::Put, head.target.clone()),
        other => return Err(IcyError::UnsupportedMethod(other.to_string())),
    };

    // SHOUTcast v1 inline form: `SOURCE <password> /mount` — the target token
    // is actually the password and the real mount follows as the version slot.
    let (mount, inline_pass) = if method == SourceMethod::Source
        && !mount.starts_with('/')
        && head.version.starts_with('/')
    {
        (head.version.clone(), Some(mount))
    } else {
        (mount, None)
    };

    let mut user = String::new();
    let mut pass = inline_pass.unwrap_or_default();
    let mut metadata = StationMeta::default();
    let mut content_type = String::from("audio/mpeg");

    for line in &head.header_lines {
        let Some((name, value)) = split_header(line) else {
            continue;
        };
        match name.as_str() {
            "authorization" => {
                if let Some((u, p)) = parse_basic_auth(&value) {
                    user = u;
                    pass = p;
                }
            }
            "ice-name" | "icy-name" => metadata.name = value,
            "ice-genre" | "icy-genre" => metadata.genre = value,
            "ice-url" | "icy-url" => metadata.url = value,
            "ice-bitrate" | "icy-br" => metadata.bitrate = value.trim().parse().ok(),
            "ice-public" | "icy-pub" => metadata.is_public = parse_bool(&value),
            "content-type" if !value.is_empty() => content_type = value,
            _ => {}
        }
    }

    Ok(SourceRequest {
        mount,
        method,
        user,
        pass,
        metadata,
        content_type,
    })
}

/// Decodes an `Authorization: Basic <base64>` value into `(user, pass)`.
///
/// Returns `None` if the scheme is not `Basic` or the base64 is invalid.
/// Credentials with no colon are treated as a bare username with empty pass.
/// Both padded and unpadded base64 are accepted.
pub fn parse_basic_auth(value: &str) -> Option<(String, String)> {
    let rest = value.trim();
    let (scheme, encoded) = rest.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let encoded = encoded.trim();
    let decoded = BASE64
        .decode(encoded.as_bytes())
        .or_else(|_| BASE64_NOPAD.decode(encoded.as_bytes()))
        .ok()?;
    let text = String::from_utf8_lossy(&decoded);
    match text.split_once(':') {
        Some((u, p)) => Some((u.to_string(), p.to_string())),
        None => Some((text.into_owned(), String::new())),
    }
}

/// Interprets an ICE boolean header (`ice-public`/`icy-pub`): `1`, `true`, or
/// `yes` (case-insensitive) are true; everything else is false.
fn parse_bool(value: &str) -> bool {
    let v = value.trim();
    v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
}

/// Builds the success response the server sends to an accepted source.
///
/// SHOUTcast `SOURCE` gets the terse `OK2` handshake; Icecast `PUT` gets a
/// standard `HTTP/1.0 200 OK`.
pub fn source_ok(method: SourceMethod) -> String {
    match method {
        SourceMethod::Source => "OK2\r\nicy-caps:11\r\n\r\n".to_string(),
        SourceMethod::Put => "HTTP/1.0 200 OK\r\n\r\n".to_string(),
    }
}

/// Builds the `401 Unauthorized` response for a source that failed auth,
/// including a `WWW-Authenticate: Basic` challenge.
pub fn source_unauthorized() -> String {
    "HTTP/1.0 401 Unauthorized\r\n\
     WWW-Authenticate: Basic realm=\"RabbitHole Radio\"\r\n\
     \r\n"
        .to_string()
}

/// Builds the `403 Forbidden` response (e.g. mount already in use, or the
/// authenticated user is not allowed on this mount).
pub fn source_forbidden() -> String {
    "HTTP/1.0 403 Forbidden\r\n\r\n".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(s: &str) -> String {
        BASE64.encode(s.as_bytes())
    }

    #[test]
    fn parses_icecast_put() {
        let raw = format!(
            "PUT /live HTTP/1.1\r\n\
             Authorization: Basic {}\r\n\
             ice-name: Warren FM\r\n\
             ice-genre: Ambient\r\n\
             ice-bitrate: 128\r\n\
             ice-public: 1\r\n\
             content-type: audio/ogg\r\n\r\n",
            b64("source:hackme")
        );
        let req = parse_source_request(raw.as_bytes()).unwrap();
        assert_eq!(req.method, SourceMethod::Put);
        assert_eq!(req.mount, "/live");
        assert_eq!(req.user, "source");
        assert_eq!(req.pass, "hackme");
        assert_eq!(req.metadata.name, "Warren FM");
        assert_eq!(req.metadata.genre, "Ambient");
        assert_eq!(req.metadata.bitrate, Some(128));
        assert!(req.metadata.is_public);
        assert_eq!(req.content_type, "audio/ogg");
    }

    #[test]
    fn parses_classic_source() {
        let raw = format!(
            "SOURCE /mnt ICE/1.0\r\nAuthorization: Basic {}\r\nicy-name: Pirate\r\n\r\n",
            b64("dj:secret")
        );
        let req = parse_source_request(raw.as_bytes()).unwrap();
        assert_eq!(req.method, SourceMethod::Source);
        assert_eq!(req.mount, "/mnt");
        assert_eq!(req.user, "dj");
        assert_eq!(req.pass, "secret");
        assert_eq!(req.metadata.name, "Pirate");
        assert_eq!(req.content_type, "audio/mpeg"); // default
    }

    #[test]
    fn parses_shoutcast_v1_inline_password() {
        let raw = b"SOURCE hunter2 /stream\r\nicy-name:Old School\r\n\r\n";
        let req = parse_source_request(raw).unwrap();
        assert_eq!(req.method, SourceMethod::Source);
        assert_eq!(req.mount, "/stream");
        assert_eq!(req.pass, "hunter2");
        assert_eq!(req.user, "");
        assert_eq!(req.metadata.name, "Old School");
    }

    #[test]
    fn header_names_are_case_insensitive() {
        let raw = b"PUT /m HTTP/1.1\r\nICE-NAME: Caps\r\nContent-Type: audio/aac\r\n\r\n";
        let req = parse_source_request(raw).unwrap();
        assert_eq!(req.metadata.name, "Caps");
        assert_eq!(req.content_type, "audio/aac");
    }

    #[test]
    fn missing_auth_is_tolerated() {
        let raw = b"PUT /m HTTP/1.1\r\nice-name: NoAuth\r\n\r\n";
        let req = parse_source_request(raw).unwrap();
        assert_eq!(req.user, "");
        assert_eq!(req.pass, "");
    }

    #[test]
    fn public_flag_variants() {
        for (v, want) in [
            ("1", true),
            ("true", true),
            ("YES", true),
            ("0", false),
            ("", false),
        ] {
            let raw = format!("PUT /m HTTP/1.1\r\nice-public: {v}\r\n\r\n");
            let req = parse_source_request(raw.as_bytes()).unwrap();
            assert_eq!(req.metadata.is_public, want, "value={v:?}");
        }
    }

    #[test]
    fn unsupported_method_errors() {
        let err = parse_source_request(b"POST /m HTTP/1.1\r\n\r\n").unwrap_err();
        assert!(matches!(err, IcyError::UnsupportedMethod(_)));
    }

    #[test]
    fn empty_request_errors() {
        assert_eq!(
            parse_source_request(b"").unwrap_err(),
            IcyError::EmptyRequest
        );
        assert_eq!(
            parse_source_request(b"\r\n\r\n").unwrap_err(),
            IcyError::EmptyRequest
        );
    }

    #[test]
    fn basic_auth_no_colon() {
        let value = format!("Basic {}", b64("justuser"));
        assert_eq!(
            parse_basic_auth(&value),
            Some(("justuser".into(), String::new()))
        );
    }

    #[test]
    fn basic_auth_password_with_colon() {
        let value = format!("Basic {}", b64("u:p:with:colons"));
        assert_eq!(
            parse_basic_auth(&value),
            Some(("u".into(), "p:with:colons".into()))
        );
    }

    #[test]
    fn basic_auth_rejects_bad_base64_and_scheme() {
        assert_eq!(parse_basic_auth("Basic !!!not-base64!!!"), None);
        assert_eq!(parse_basic_auth("Bearer abc"), None);
        assert_eq!(parse_basic_auth("garbage"), None);
    }

    #[test]
    fn response_helpers() {
        assert!(source_ok(SourceMethod::Source).starts_with("OK2"));
        assert!(source_ok(SourceMethod::Put).starts_with("HTTP/1.0 200 OK"));
        assert!(source_unauthorized().contains("401"));
        assert!(source_unauthorized().contains("WWW-Authenticate: Basic"));
        assert!(source_forbidden().contains("403"));
    }
}
