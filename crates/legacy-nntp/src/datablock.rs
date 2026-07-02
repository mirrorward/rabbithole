//! Multi-line data-block framing with dot-stuffing (RFC 3977 §3.1.1).
//!
//! Article bodies, `LIST` output, overview blocks, and every other multi-line
//! NNTP response travel as a *data block*: each line is terminated by CRLF and
//! the whole block is terminated by a line containing a single `.`. To keep a
//! payload line that legitimately begins with `.` from being mistaken for the
//! terminator, the sender doubles a leading `.` and the receiver undoubles it.
//!
//! This module encodes and decodes that framing in both directions:
//!
//! * [`encode_lines`] / [`decode_lines`] operate on logical lines and are the
//!   exact, lossless pair (`decode_lines(&encode_lines(x)) == x`).
//! * [`encode_block`] / [`decode_block`] are string conveniences that split on
//!   and rejoin with `\n`.
//!
//! Decoding is defensive: any byte sequence is accepted without panicking, lone
//! `\n` line endings are tolerated as if they were CRLF, and a block that is
//! never terminated by a `.` line is reported as an error rather than silently
//! accepted.

use thiserror::Error;

/// The terminator that ends every data block on the wire.
pub const TERMINATOR: &str = ".\r\n";

/// Reasons a wire data block cannot be decoded.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DataBlockError {
    /// The input ran out before a `.` terminator line was seen.
    #[error("data block is not terminated by a \".\" line")]
    Unterminated,
}

/// Encode logical `lines` into a dot-stuffed, CRLF-framed wire block.
///
/// Each line is emitted with a doubled leading `.` when needed and a trailing
/// CRLF; the block ends with the `.` terminator line. Lines are expected to be
/// single logical lines (no embedded CR/LF); embedded newlines are written
/// verbatim and are the caller's responsibility.
#[must_use]
pub fn encode_lines<S: AsRef<str>>(lines: &[S]) -> String {
    let mut out = String::new();
    for line in lines {
        let line = line.as_ref();
        if line.starts_with('.') {
            out.push('.');
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out.push_str(TERMINATOR);
    out
}

/// Encode a body string into a wire block, splitting on line boundaries.
///
/// Lines are recognised with [`str::lines`], so both `\n` and `\r\n` separators
/// are accepted and a trailing newline does not produce a spurious empty line.
#[must_use]
pub fn encode_block(body: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    encode_lines(&lines)
}

/// Decode a dot-stuffed wire block into its logical lines.
///
/// Leading `.` doubling is undone and the `.` terminator is consumed. Lone `\n`
/// separators are tolerated. Never panics on arbitrary input.
///
/// # Errors
///
/// Returns [`DataBlockError::Unterminated`] if no `.` terminator line is found.
pub fn decode_lines(wire: &str) -> Result<Vec<String>, DataBlockError> {
    let mut out = Vec::new();
    for segment in wire.split('\n') {
        // Tolerate CRLF and bare LF: strip a single trailing CR.
        let line = segment.strip_suffix('\r').unwrap_or(segment);
        if line == "." {
            return Ok(out);
        }
        // The segment following the final `\n` is the empty tail of a
        // well-formed block; only reachable here when unterminated.
        if let Some(rest) = line.strip_prefix('.') {
            out.push(rest.to_string());
        } else {
            out.push(line.to_string());
        }
    }
    Err(DataBlockError::Unterminated)
}

/// Decode a wire block into a body string, joining logical lines with `\n`.
///
/// # Errors
///
/// Returns [`DataBlockError::Unterminated`] if the block has no terminator.
pub fn decode_block(wire: &str) -> Result<String, DataBlockError> {
    Ok(decode_lines(wire)?.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_plain_lines() {
        let wire = encode_lines(&["hello", "world"]);
        assert_eq!(wire, "hello\r\nworld\r\n.\r\n");
    }

    #[test]
    fn doubles_leading_dot_on_send() {
        let wire = encode_lines(&[".signature", "..dotdot", "normal"]);
        assert_eq!(wire, "..signature\r\n...dotdot\r\nnormal\r\n.\r\n");
    }

    #[test]
    fn lone_dot_line_is_doubled() {
        // A body line that is just "." must not look like the terminator.
        let wire = encode_lines(&["."]);
        assert_eq!(wire, "..\r\n.\r\n");
    }

    #[test]
    fn empty_block_is_just_terminator() {
        let empty: [&str; 0] = [];
        assert_eq!(encode_lines(&empty), ".\r\n");
        assert_eq!(decode_lines(".\r\n").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn undoubles_leading_dot_on_receive() {
        let lines = decode_lines("..signature\r\n...dotdot\r\nnormal\r\n.\r\n").unwrap();
        assert_eq!(lines, vec![".signature", "..dotdot", "normal"]);
    }

    #[test]
    fn round_trip_lines_with_dots() {
        let original = vec![
            ".hidden".to_string(),
            "..".to_string(),
            "normal text".to_string(),
            String::new(),
            ". with trailing".to_string(),
        ];
        let wire = encode_lines(&original);
        let back = decode_lines(&wire).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn round_trip_body_string() {
        let body = "From: me\n.leading dot line\n\nlast";
        let wire = encode_block(body);
        assert_eq!(decode_block(&wire).unwrap(), body);
    }

    #[test]
    fn preserves_empty_interior_lines() {
        let wire = "a\r\n\r\nb\r\n.\r\n";
        assert_eq!(decode_lines(wire).unwrap(), vec!["a", "", "b"]);
    }

    #[test]
    fn tolerates_bare_lf() {
        assert_eq!(decode_lines("a\nb\n.\n").unwrap(), vec!["a", "b"]);
    }

    #[test]
    fn unterminated_is_an_error() {
        assert_eq!(
            decode_lines("no terminator here\r\n"),
            Err(DataBlockError::Unterminated)
        );
        assert_eq!(decode_lines(""), Err(DataBlockError::Unterminated));
    }

    #[test]
    fn never_panics_on_arbitrary_input() {
        for probe in [
            "",
            ".",
            "\r",
            "\n",
            "\r\n",
            "..\r",
            "....",
            "\0\0\0",
            ".\r\n.\r\n",
        ] {
            let _ = decode_lines(probe);
            let _ = decode_block(probe);
        }
    }
}
