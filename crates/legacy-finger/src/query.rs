//! RFC 1288 query-line parsing.
//!
//! A finger request is a single line: `{Q1} ::= [/W] [username] <CRLF>`.
//! An empty query asks who's online; a bare name asks for that user's
//! profile; the `/W` verbose token is accepted and ignored (we always answer
//! at one verbosity); and anything containing `@` is a forwarding request
//! (`{Q2}`), which this server refuses per RFC 1288 §3.2.1's advice that
//! hosts should feel free to.

/// A parsed finger query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Query {
    /// Empty query: list who's currently online.
    Who,
    /// Look up a single user by name.
    User(String),
    /// A `user@host` forwarding request. Always refused.
    Forward,
}

/// Parse one query line (CRLF already stripped).
///
/// Leading/trailing whitespace is tolerated, as is a leading `/W` token in
/// any case. Any `@` anywhere in the remaining query marks it as a
/// forwarding request — conservative on purpose, so `@host`, `user@host`,
/// and `user@host@host` chains are all refused.
pub fn parse_query(line: &str) -> Query {
    let mut rest = line.trim();

    // Tolerate (and ignore) the verbose flag, with or without a following
    // name: "/W", "/W user", and the glued "/Wuser" form some clients emit.
    //
    // Compare the first two *bytes*, not `rest[..2]`: the query reaches us via
    // the server's `String::from_utf8_lossy`, so a line starting with an
    // invalid byte becomes a leading multi-byte U+FFFD and `rest[..2]` would
    // slice inside it and panic. `/W` is pure ASCII, so byte-level comparison
    // is exact; and once bytes 0–1 are the ASCII `/w`, byte 2 is guaranteed a
    // char boundary, so `rest[2..]` is safe.
    let head = rest.as_bytes();
    if head.len() >= 2 && head[..2].eq_ignore_ascii_case(b"/w") {
        rest = rest[2..].trim_start();
    }

    if rest.is_empty() {
        Query::Who
    } else if rest.contains('@') {
        Query::Forward
    } else {
        Query::User(rest.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_line_is_who() {
        assert_eq!(parse_query(""), Query::Who);
        assert_eq!(parse_query("   "), Query::Who);
    }

    #[test]
    fn bare_name_is_user_lookup() {
        assert_eq!(parse_query("alice"), Query::User("alice".into()));
        assert_eq!(parse_query("  alice  "), Query::User("alice".into()));
    }

    #[test]
    fn verbose_flag_is_tolerated_and_ignored() {
        assert_eq!(parse_query("/W"), Query::Who);
        assert_eq!(parse_query("/w  "), Query::Who);
        assert_eq!(parse_query("/W alice"), Query::User("alice".into()));
        assert_eq!(parse_query("/w alice"), Query::User("alice".into()));
        assert_eq!(parse_query("/Walice"), Query::User("alice".into()));
    }

    #[test]
    fn anything_with_at_sign_is_forwarding() {
        assert_eq!(parse_query("alice@example.com"), Query::Forward);
        assert_eq!(parse_query("@example.com"), Query::Forward);
        assert_eq!(parse_query("a@b@c"), Query::Forward);
        assert_eq!(parse_query("/W alice@example.com"), Query::Forward);
    }

    #[test]
    fn slash_w_prefix_only_strips_the_flag_once() {
        // "/W/W" leaves a residual "/W" that is treated as a (weird) name.
        assert_eq!(parse_query("/W/W"), Query::User("/W".into()));
    }
}
