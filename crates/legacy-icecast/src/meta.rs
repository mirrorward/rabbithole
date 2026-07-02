//! Station metadata model and ICY response-header rendering.
//!
//! [`StationMeta`] is the shared description of a broadcast — the values a
//! source advertises via `ice-*`/`icy-*` headers when it connects, and the
//! values a listener is told via `icy-*` headers on the stream response. The
//! same struct rides both directions so the source parser and the listener
//! responder speak one vocabulary.

use std::fmt::Write as _;

/// Description of a radio station / mount.
///
/// Populated from a source's `ice-*`/`icy-*` headers on connect
/// ([`crate::source::SourceRequest`]) and rendered back out to listeners as
/// `icy-*` response headers ([`StationMeta::render_icy_headers`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StationMeta {
    /// Human-readable station name (`ice-name` / `icy-name`).
    pub name: String,
    /// Free-text genre (`ice-genre` / `icy-genre`).
    pub genre: String,
    /// Station homepage URL (`ice-url` / `icy-url`).
    pub url: String,
    /// Nominal stream bitrate in kbps (`ice-bitrate` / `icy-br`), if known.
    pub bitrate: Option<u32>,
    /// Whether the station opts into public directory listing
    /// (`ice-public` / `icy-pub`).
    pub is_public: bool,
    /// The currently-playing track title, if any. Sources leave this empty at
    /// connect; it is filled from in-band metadata updates thereafter.
    pub now_playing: String,
}

impl StationMeta {
    /// Renders the station's `icy-*` response headers, each terminated with
    /// CRLF. Empty string fields are omitted so listeners are not told
    /// `icy-name:` with no value; `icy-pub` is always emitted (0 or 1).
    ///
    /// The returned block does *not* include a status line, `content-type`,
    /// `icy-metaint`, or the terminating blank line — see
    /// [`crate::listener::build_listener_response`] for the full response.
    pub fn render_icy_headers(&self) -> String {
        let mut out = String::new();
        if !self.name.is_empty() {
            let _ = write!(out, "icy-name:{}\r\n", header_value(&self.name));
        }
        if !self.genre.is_empty() {
            let _ = write!(out, "icy-genre:{}\r\n", header_value(&self.genre));
        }
        if !self.url.is_empty() {
            let _ = write!(out, "icy-url:{}\r\n", header_value(&self.url));
        }
        if let Some(br) = self.bitrate {
            let _ = write!(out, "icy-br:{br}\r\n");
        }
        let _ = write!(out, "icy-pub:{}\r\n", u8::from(self.is_public));
        out
    }
}

/// Strips CR/LF from a header value so it cannot inject extra header lines
/// (defensive: source-supplied metadata is untrusted). Other bytes pass
/// through verbatim.
fn header_value(raw: &str) -> String {
    raw.chars().filter(|&c| c != '\r' && c != '\n').collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_populated_headers() {
        let meta = StationMeta {
            name: "Warren FM".into(),
            genre: "Ambient".into(),
            url: "https://example.invalid".into(),
            bitrate: Some(128),
            is_public: true,
            now_playing: String::new(),
        };
        let headers = meta.render_icy_headers();
        assert!(headers.contains("icy-name:Warren FM\r\n"));
        assert!(headers.contains("icy-genre:Ambient\r\n"));
        assert!(headers.contains("icy-url:https://example.invalid\r\n"));
        assert!(headers.contains("icy-br:128\r\n"));
        assert!(headers.contains("icy-pub:1\r\n"));
    }

    #[test]
    fn omits_empty_and_defaults_pub_zero() {
        let meta = StationMeta::default();
        let headers = meta.render_icy_headers();
        assert!(!headers.contains("icy-name"));
        assert!(!headers.contains("icy-genre"));
        assert!(!headers.contains("icy-url"));
        assert!(!headers.contains("icy-br"));
        assert_eq!(headers, "icy-pub:0\r\n");
    }

    #[test]
    fn header_value_strips_crlf_injection() {
        let meta = StationMeta {
            name: "evil\r\nicy-pub:9".into(),
            ..StationMeta::default()
        };
        let headers = meta.render_icy_headers();
        assert!(headers.contains("icy-name:evilicy-pub:9\r\n"));
        // The injected value must not create a real second header line.
        assert_eq!(headers.matches("\r\n").count(), 2);
    }
}
