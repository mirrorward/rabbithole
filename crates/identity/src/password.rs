//! Argon2id password hashing.
//!
//! Profile: the OWASP "standard" recommendation — m=64 MiB, t=3, p=1 —
//! stored as a PHC string so parameters travel with the hash. When the
//! configured profile is raised, [`needs_rehash`] lets login paths upgrade
//! hashes transparently.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};

/// Memory cost in KiB (64 MiB).
pub const MEMORY_KIB: u32 = 64 * 1024;
/// Iterations.
pub const ITERATIONS: u32 = 3;
/// Parallelism.
pub const PARALLELISM: u32 = 1;
/// Output length in bytes.
pub const OUTPUT_LEN: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("hashing failed: {0}")]
    Hash(argon2::password_hash::Error),
    #[error("stored hash is malformed")]
    Malformed,
}

fn argon2() -> Argon2<'static> {
    let params = Params::new(MEMORY_KIB, ITERATIONS, PARALLELISM, Some(OUTPUT_LEN))
        .expect("static params are valid");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

/// Hash a password with the current profile. Returns a PHC string
/// (`$argon2id$v=19$m=65536,t=3,p=1$...`).
pub fn hash_password(password: &str) -> Result<String, PasswordError> {
    let salt = SaltString::generate(&mut rand::rngs::OsRng);
    argon2()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(PasswordError::Hash)
}

/// Constant-time verification against a PHC string.
pub fn verify_password(password: &str, phc: &str) -> Result<bool, PasswordError> {
    let parsed = PasswordHash::new(phc).map_err(|_| PasswordError::Malformed)?;
    match argon2().verify_password(password.as_bytes(), &parsed) {
        Ok(()) => Ok(true),
        Err(argon2::password_hash::Error::Password) => Ok(false),
        Err(e) => Err(PasswordError::Hash(e)),
    }
}

/// Does this stored hash use weaker parameters than the current profile?
/// (Call after a successful login; if true, rehash and store.)
pub fn needs_rehash(phc: &str) -> Result<bool, PasswordError> {
    let parsed = PasswordHash::new(phc).map_err(|_| PasswordError::Malformed)?;
    if parsed.algorithm != Algorithm::Argon2id.ident() {
        return Ok(true);
    }
    let m = param_u32(&parsed, "m");
    let t = param_u32(&parsed, "t");
    let p = param_u32(&parsed, "p");
    Ok(m.unwrap_or(0) < MEMORY_KIB || t.unwrap_or(0) < ITERATIONS || p.unwrap_or(0) < PARALLELISM)
}

fn param_u32(hash: &PasswordHash<'_>, key: &str) -> Option<u32> {
    hash.params
        .iter()
        .find(|(k, _)| k.as_str() == key)
        .and_then(|(_, v)| v.decimal().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: these hash at full cost (64 MiB, t=3) — a few hundred ms each.

    #[test]
    fn hash_and_verify() {
        let phc = hash_password("correct horse battery staple").unwrap();
        assert!(phc.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery staple", &phc).unwrap());
        assert!(!verify_password("wrong", &phc).unwrap());
        assert!(!needs_rehash(&phc).unwrap());
    }

    #[test]
    fn weaker_params_need_rehash() {
        // A structurally valid argon2id PHC with m below our profile.
        let weak = "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$K5d1Nl3Yg0jFm0kFmSMPCMHpcUqC0G0RTPfvVLxKvR0";
        assert!(needs_rehash(weak).unwrap());
    }

    #[test]
    fn malformed_hash_errors() {
        assert!(matches!(
            verify_password("x", "not-a-phc"),
            Err(PasswordError::Malformed)
        ));
    }
}
