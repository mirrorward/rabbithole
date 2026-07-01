//! Opaque session tokens.
//!
//! 32 bytes from the OS CSPRNG, presented as URL-safe base64. The server
//! stores only the blake3 hash of a token (so a database leak doesn't leak
//! live sessions); lookups hash the presented token and compare in
//! constant time.

use rand::RngCore;
use subtle::ConstantTimeEq;

/// A bearer session token (the secret the client holds).
#[derive(Clone)]
pub struct SessionToken {
    bytes: [u8; 32],
}

impl SessionToken {
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        Self { bytes }
    }

    /// Wire/user representation (URL-safe base64, no padding).
    pub fn encode(&self) -> String {
        data_encoding::BASE64URL_NOPAD.encode(&self.bytes)
    }

    pub fn decode(s: &str) -> Option<Self> {
        let raw = data_encoding::BASE64URL_NOPAD.decode(s.as_bytes()).ok()?;
        let bytes: [u8; 32] = raw.try_into().ok()?;
        Some(Self { bytes })
    }

    /// What the server persists: blake3 of the token bytes.
    pub fn storage_hash(&self) -> [u8; 32] {
        *blake3::hash(&self.bytes).as_bytes()
    }

    /// Constant-time check of this token against a stored hash.
    pub fn matches_hash(&self, stored: &[u8; 32]) -> bool {
        self.storage_hash().ct_eq(stored).into()
    }
}

impl std::fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never leak token material through Debug/logs.
        f.write_str("SessionToken(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let t = SessionToken::generate();
        let restored = SessionToken::decode(&t.encode()).unwrap();
        assert!(restored.matches_hash(&t.storage_hash()));
    }

    #[test]
    fn tokens_are_unique_and_hashes_differ() {
        let a = SessionToken::generate();
        let b = SessionToken::generate();
        assert_ne!(a.encode(), b.encode());
        assert!(!a.matches_hash(&b.storage_hash()));
    }

    #[test]
    fn debug_does_not_leak() {
        let t = SessionToken::generate();
        assert_eq!(format!("{t:?}"), "SessionToken(..)");
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(SessionToken::decode("short").is_none());
        assert!(SessionToken::decode("!!!not-base64!!!").is_none());
    }
}
