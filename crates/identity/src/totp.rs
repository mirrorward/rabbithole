//! TOTP two-factor auth (RFC 6238) and recovery codes.
//!
//! 30-second step, 6 digits, SHA-1 (the authenticator-app compatible
//! default), ±1 step of clock skew. Recovery codes are single-use random
//! strings; only their blake3 hashes are meant to be persisted (the
//! store layer additionally wraps them at rest).

use data_encoding::BASE32_NOPAD;
use rand::RngCore;
use totp_rs::{Algorithm, TOTP};

#[derive(Debug, thiserror::Error)]
pub enum TotpError {
    #[error("invalid TOTP secret")]
    InvalidSecret,
    #[error("system clock error")]
    Clock,
}

/// A TOTP enrollment for one account.
pub struct TotpEnrollment {
    totp: TOTP,
}

impl TotpEnrollment {
    /// Generate a fresh 160-bit secret.
    pub fn generate(issuer: &str, account_label: &str) -> Self {
        let mut secret = vec![0u8; 20];
        rand::rngs::OsRng.fill_bytes(&mut secret);
        Self::from_secret(&secret, issuer, account_label).expect("fresh secret is valid")
    }

    /// Rebuild from a stored raw secret.
    pub fn from_secret(
        secret: &[u8],
        issuer: &str,
        account_label: &str,
    ) -> Result<Self, TotpError> {
        let totp = TOTP::new(
            Algorithm::SHA1,
            6,
            1, // accepted skew, in steps
            30,
            secret.to_vec(),
            Some(issuer.to_string()),
            account_label.to_string(),
        )
        .map_err(|_| TotpError::InvalidSecret)?;
        Ok(Self { totp })
    }

    /// Raw secret bytes (persist encrypted at rest).
    pub fn secret(&self) -> &[u8] {
        &self.totp.secret
    }

    /// Base32 form for manual entry in authenticator apps.
    pub fn secret_base32(&self) -> String {
        BASE32_NOPAD.encode(&self.totp.secret)
    }

    /// `otpauth://` URL for QR-code provisioning.
    pub fn provisioning_url(&self) -> String {
        self.totp.get_url()
    }

    /// Check a user-supplied code against the current time (±1 step).
    pub fn verify(&self, code: &str) -> Result<bool, TotpError> {
        self.totp.check_current(code).map_err(|_| TotpError::Clock)
    }

    /// Current code (for tests and enrollment confirmation display).
    pub fn current_code(&self) -> Result<String, TotpError> {
        self.totp.generate_current().map_err(|_| TotpError::Clock)
    }
}

/// Generate `count` single-use recovery codes (returned in plain text for
/// one-time display) alongside their blake3 hashes (what gets stored).
pub fn generate_recovery_codes(count: usize) -> Vec<(String, [u8; 32])> {
    (0..count)
        .map(|_| {
            let mut raw = [0u8; 10];
            rand::rngs::OsRng.fill_bytes(&mut raw);
            // Format as xxxxx-xxxxx-xxxx from base32 for easy human entry.
            let code = BASE32_NOPAD.encode(&raw).to_lowercase();
            let code = format!("{}-{}-{}", &code[0..5], &code[5..10], &code[10..16]);
            let hash = *blake3::hash(code.as_bytes()).as_bytes();
            (code, hash)
        })
        .collect()
}

/// Constant-time-ish check of a presented recovery code against stored
/// hashes; returns the index of the matching (now spent) code.
pub fn check_recovery_code(presented: &str, stored: &[[u8; 32]]) -> Option<usize> {
    use subtle::ConstantTimeEq;
    let hash = *blake3::hash(presented.trim().to_lowercase().as_bytes()).as_bytes();
    let mut found = None;
    for (i, s) in stored.iter().enumerate() {
        if bool::from(hash.ct_eq(s)) {
            found = Some(i);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrollment_roundtrip_and_verify() {
        let e = TotpEnrollment::generate("RabbitHole", "alice");
        let code = e.current_code().unwrap();
        assert!(e.verify(&code).unwrap());
        assert!(!e.verify("000000").unwrap() || code == "000000");

        // Same secret elsewhere generates the same code.
        let e2 = TotpEnrollment::from_secret(e.secret(), "RabbitHole", "alice").unwrap();
        assert!(e2.verify(&code).unwrap());
    }

    #[test]
    fn provisioning_url_contains_issuer() {
        let e = TotpEnrollment::generate("RabbitHole", "alice");
        assert!(e.provisioning_url().starts_with("otpauth://totp/"));
        assert!(e.provisioning_url().contains("RabbitHole"));
    }

    #[test]
    fn recovery_codes_verify_once_each() {
        let codes = generate_recovery_codes(8);
        assert_eq!(codes.len(), 8);
        let hashes: Vec<[u8; 32]> = codes.iter().map(|(_, h)| *h).collect();
        let idx = check_recovery_code(&codes[3].0, &hashes).unwrap();
        assert_eq!(idx, 3);
        assert!(check_recovery_code("not-a-code", &hashes).is_none());
    }
}
