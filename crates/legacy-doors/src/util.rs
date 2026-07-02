//! Small shared helpers for the drop-file codecs.
//!
//! All classic drop files are line-oriented ASCII with `CRLF` terminators,
//! so the writers build a list of field strings and join them, and the readers
//! split on newlines while tolerating either `CRLF` or bare `LF`.

/// Line terminator used by every classic drop file.
pub(crate) const CRLF: &str = "\r\n";

/// Join field lines with `CRLF`, terminating the final line too (as DOS tools
/// and real BBS software do).
pub(crate) fn join_crlf(lines: &[String]) -> String {
    let mut out = String::new();
    for line in lines {
        out.push_str(line);
        out.push_str(CRLF);
    }
    out
}

/// Split text into logical lines, tolerating `CRLF` or bare `LF` and stripping
/// a trailing `\r`. A trailing terminator yields a final empty element, which
/// callers simply never index.
pub(crate) fn split_lines(text: &str) -> Vec<&str> {
    text.split('\n').map(|l| l.trim_end_matches('\r')).collect()
}

/// Split a full name into `(first, rest)` at the first space. Names without a
/// space become `(name, "")`.
pub(crate) fn split_name(name: &str) -> (String, String) {
    match name.trim().split_once(' ') {
        Some((first, rest)) => (first.to_string(), rest.to_string()),
        None => (name.trim().to_string(), String::new()),
    }
}

/// Parse a comm-port field such as `"COM0:"`, `"COM3"` or `"3"` into its number
/// (`0` == local). Anything unparseable becomes `0`.
pub(crate) fn parse_com(field: &str) -> u16 {
    let digits = field
        .trim()
        .trim_end_matches(':')
        .trim_start_matches(|c: char| !c.is_ascii_digit());
    digits.parse().unwrap_or(0)
}
