//! Listener (player pull) protocol: parse the client `GET` and build the ICY
//! stream response headers.
//!
//! A player fetches a mount with a plain `GET /mount HTTP/1.0` and signals it
//! can decode in-band metadata with `Icy-MetaData: 1`. The server answers with
//! an `ICY 200 OK` status line, the station's `icy-*` headers, the audio
//! `content-type`, and — only when the listener asked for it — an
//! `icy-metaint:<n>` header promising a metadata block every `n` audio bytes
//! (see [`crate::metaint`]).

use crate::http::RequestHead;
use crate::meta::StationMeta;
use crate::IcyError;

/// A parsed listener (player) request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListenerRequest {
    /// Mount the listener wants to play, e.g. `/live`.
    pub mount: String,
    /// Whether the listener sent `Icy-MetaData: 1` (can decode in-band
    /// metadata, so the response should include `icy-metaint`).
    pub wants_metadata: bool,
    /// The client's `User-Agent`, if present.
    pub user_agent: Option<String>,
}

/// Parses a listener `GET` request.
///
/// Errors with [`IcyError::UnsupportedMethod`] for a non-`GET` request and the
/// usual [`IcyError`] variants for an empty or malformed request line.
pub fn parse_listener_request(raw: &[u8]) -> Result<ListenerRequest, IcyError> {
    let head = RequestHead::parse(raw)?;
    if !head.method.eq_ignore_ascii_case("GET") {
        return Err(IcyError::UnsupportedMethod(head.method));
    }
    let wants_metadata = head
        .header("icy-metadata")
        .map(|v| v.trim() == "1")
        .unwrap_or(false);
    Ok(ListenerRequest {
        mount: head.target.clone(),
        wants_metadata,
        user_agent: head.header("user-agent"),
    })
}

/// Builds the full ICY stream response head for a listener.
///
/// Emits the `ICY 200 OK` status line, the station's `icy-*` headers, the audio
/// `content_type`, and (when `metaint` is `Some`) an `icy-metaint` header. Pass
/// `metaint` as `Some(n)` exactly when the listener's request had
/// `wants_metadata` set; pass `None` for a metadata-free stream. The returned
/// string ends with the blank line that separates headers from the audio body.
pub fn build_listener_response(
    meta: &StationMeta,
    content_type: &str,
    metaint: Option<usize>,
) -> String {
    let mut out = String::from("ICY 200 OK\r\n");
    out.push_str(&meta.render_icy_headers());
    out.push_str("content-type:");
    out.push_str(content_type);
    out.push_str("\r\n");
    if let Some(n) = metaint {
        out.push_str(&format!("icy-metaint:{n}\r\n"));
    }
    out.push_str("\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metaint::DEFAULT_METAINT;

    #[test]
    fn parses_get_with_metadata() {
        let raw = b"GET /live HTTP/1.0\r\nIcy-MetaData: 1\r\nUser-Agent: WinampMPEG/5.0\r\n\r\n";
        let req = parse_listener_request(raw).unwrap();
        assert_eq!(req.mount, "/live");
        assert!(req.wants_metadata);
        assert_eq!(req.user_agent.as_deref(), Some("WinampMPEG/5.0"));
    }

    #[test]
    fn parses_get_without_metadata() {
        let req = parse_listener_request(b"GET /m HTTP/1.1\r\n\r\n").unwrap();
        assert!(!req.wants_metadata);
        assert_eq!(req.user_agent, None);
    }

    #[test]
    fn metadata_flag_must_be_one() {
        let req = parse_listener_request(b"GET /m HTTP/1.1\r\nIcy-MetaData: 0\r\n\r\n").unwrap();
        assert!(!req.wants_metadata);
    }

    #[test]
    fn non_get_is_rejected() {
        let err = parse_listener_request(b"POST /m HTTP/1.1\r\n\r\n").unwrap_err();
        assert!(matches!(err, IcyError::UnsupportedMethod(_)));
    }

    #[test]
    fn builds_response_with_metaint() {
        let meta = StationMeta {
            name: "Warren FM".into(),
            genre: "Ambient".into(),
            bitrate: Some(128),
            is_public: true,
            ..StationMeta::default()
        };
        let resp = build_listener_response(&meta, "audio/mpeg", Some(DEFAULT_METAINT));
        assert!(resp.starts_with("ICY 200 OK\r\n"));
        assert!(resp.contains("icy-name:Warren FM\r\n"));
        assert!(resp.contains("icy-br:128\r\n"));
        assert!(resp.contains("icy-pub:1\r\n"));
        assert!(resp.contains("content-type:audio/mpeg\r\n"));
        assert!(resp.contains("icy-metaint:8192\r\n"));
        assert!(resp.ends_with("\r\n\r\n"));
    }

    #[test]
    fn builds_response_without_metaint() {
        let resp = build_listener_response(&StationMeta::default(), "audio/mpeg", None);
        assert!(!resp.contains("icy-metaint"));
        assert!(resp.contains("content-type:audio/mpeg\r\n"));
    }
}
