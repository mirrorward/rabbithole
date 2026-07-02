//! Minimal, panic-free parsing of a request head (request line + headers).
//!
//! ICY sources and listeners both open with an HTTP-shaped request head. This
//! module extracts the method/target/version tokens and the raw header lines
//! without allocating a full HTTP stack — just enough for the source and
//! listener parsers to work over. It is deliberately lenient: bytes are read
//! lossily as UTF-8, lines split on `\n` with a trailing `\r` trimmed, and the
//! header block ends at the first blank line (or end of input).

use crate::IcyError;

/// A parsed request head.
#[derive(Clone, Debug)]
pub(crate) struct RequestHead {
    /// Request method token, e.g. `SOURCE`, `PUT`, `GET` (verbatim casing).
    pub method: String,
    /// Request target token (usually the mount, e.g. `/live`).
    pub target: String,
    /// Version token, e.g. `HTTP/1.1` or `ICE/1.0` (empty if absent).
    pub version: String,
    /// Header lines following the request line, up to the blank-line
    /// terminator. Each is trimmed of its line ending but otherwise verbatim.
    pub header_lines: Vec<String>,
}

impl RequestHead {
    /// Parses a request head from raw bytes.
    ///
    /// Errors with [`IcyError::EmptyRequest`] when there is no request line and
    /// [`IcyError::MalformedRequestLine`] when the request line lacks a method
    /// and target.
    pub fn parse(raw: &[u8]) -> Result<Self, IcyError> {
        let text = String::from_utf8_lossy(raw);
        let mut lines = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));

        // First non-empty line is the request line.
        let request_line = lines
            .by_ref()
            .find(|l| !l.trim().is_empty())
            .ok_or(IcyError::EmptyRequest)?;

        let mut tokens = request_line.split_whitespace();
        let method = tokens
            .next()
            .ok_or(IcyError::MalformedRequestLine)?
            .to_string();
        let target = tokens
            .next()
            .ok_or(IcyError::MalformedRequestLine)?
            .to_string();
        let version = tokens.next().unwrap_or("").to_string();

        let mut header_lines = Vec::new();
        for line in lines {
            if line.trim().is_empty() {
                break; // end of header block
            }
            header_lines.push(line.to_string());
        }

        Ok(Self {
            method,
            target,
            version,
            header_lines,
        })
    }

    /// Finds the first header whose name matches `name` (case-insensitive) and
    /// returns its trimmed value.
    pub fn header(&self, name: &str) -> Option<String> {
        self.header_lines.iter().find_map(|line| {
            let (n, v) = split_header(line)?;
            (n == name.to_ascii_lowercase()).then_some(v)
        })
    }
}

/// Splits a header line into `(lowercased-name, trimmed-value)` at the first
/// colon. Returns `None` for lines without a colon.
pub(crate) fn split_header(line: &str) -> Option<(String, String)> {
    let (name, value) = line.split_once(':')?;
    Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_request_line_and_headers() {
        let head =
            RequestHead::parse(b"GET /live HTTP/1.1\r\nHost: x\r\nIcy-MetaData:1\r\n\r\n").unwrap();
        assert_eq!(head.method, "GET");
        assert_eq!(head.target, "/live");
        assert_eq!(head.version, "HTTP/1.1");
        assert_eq!(head.header_lines.len(), 2);
        assert_eq!(head.header("icy-metadata"), Some("1".to_string()));
        assert_eq!(head.header("HOST"), Some("x".to_string()));
        assert_eq!(head.header("missing"), None);
    }

    #[test]
    fn tolerates_lf_only_and_leading_blanks() {
        let head = RequestHead::parse(b"\n\nSOURCE /m ICE/1.0\nice-name:x\n").unwrap();
        assert_eq!(head.method, "SOURCE");
        assert_eq!(head.target, "/m");
        assert_eq!(head.header("ice-name"), Some("x".to_string()));
    }

    #[test]
    fn empty_input_errors() {
        assert_eq!(RequestHead::parse(b"").unwrap_err(), IcyError::EmptyRequest);
    }

    #[test]
    fn request_line_without_target_errors() {
        assert_eq!(
            RequestHead::parse(b"GET\r\n\r\n").unwrap_err(),
            IcyError::MalformedRequestLine
        );
    }

    #[test]
    fn split_header_basics() {
        assert_eq!(
            split_header("Content-Type:  audio/mpeg "),
            Some(("content-type".to_string(), "audio/mpeg".to_string()))
        );
        assert_eq!(split_header("no-colon-here"), None);
    }
}
