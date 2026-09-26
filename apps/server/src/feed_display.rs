//! Redacted identifiers for routine feed monitoring. Fetching and statistics
//! continue to use the exact configured URL internally.

/// Omit userinfo and every query/fragment value, including on invalid input.
/// Paths remain visible so operators can recognize ordinary feed endpoints.
pub fn url(raw: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(raw) else {
        return "(invalid feed URL)".into();
    };
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return "(invalid feed URL)".into();
    }
    if parsed.set_username("").is_err() || parsed.set_password(None).is_err() {
        return "(invalid feed URL)".into();
    }
    parsed.set_query(None);
    parsed.set_fragment(None);
    parsed.to_string()
}

/// A stable row key that does not expose an unkeyed hash of credentials.
pub fn id(seed: &[u8; 32], raw: &str) -> [u8; 32] {
    let key = blake3::derive_key("RabbitHole feed monitor row identity v1", seed);
    *blake3::keyed_hash(&key, raw.as_bytes()).as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_queries_fragments_and_invalid_inputs_are_not_displayed() {
        assert_eq!(
            url("https://user:p%40ss@example.test/feed?token=private#secret"),
            "https://example.test/feed"
        );
        assert_eq!(
            url("http://user:pass@[::1]:8080/feed?q=secret"),
            "http://[::1]:8080/feed"
        );
        for invalid in [
            "not a URL password",
            "ftp://user:secret@example.test/a",
            "http://user:secret@[bad/feed",
            "https:///",
            "https://user:secret@",
        ] {
            assert_eq!(url(invalid), "(invalid feed URL)");
        }
    }

    #[test]
    fn redacted_collisions_keep_distinct_burrow_scoped_ids() {
        let a = "https://a:secret@example.test/feed?token=one";
        let b = "https://b:secret@example.test/feed?token=two";
        assert_eq!(url(a), url(b));
        assert_eq!(id(&[1; 32], a), id(&[1; 32], a));
        assert_ne!(id(&[1; 32], a), id(&[1; 32], b));
        assert_ne!(id(&[1; 32], a), id(&[2; 32], a));
    }
}
