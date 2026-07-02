//! Persistent server identity: the Ed25519 signing key and TLS material,
//! stored under `<data_dir>/identity/`.
//!
//! The Ed25519 key is the burrow's federation identity — it must survive
//! restarts. The self-signed TLS cert persists too so the pinned
//! fingerprint clients saved stays valid (replaced by ACME in later work).

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use rabbithole_identity::keys::IdentityKey;
use rabbithole_net::tls::TlsIdentity;
use rustls_pki_types::{CertificateDer, PrivatePkcs8KeyDer};

pub struct ServerIdentity {
    pub signing: IdentityKey,
    pub tls: TlsIdentity,
}

pub fn load_or_create(data_dir: &Path, hostnames: &[String]) -> Result<ServerIdentity> {
    let dir = data_dir.join("identity");
    fs::create_dir_all(&dir)?;

    let key_path = dir.join("server_ed25519.seed");
    let signing = if key_path.exists() {
        let hex = fs::read_to_string(&key_path)?;
        let raw = hex::decode(hex.trim()).context("corrupt server key file")?;
        let seed: [u8; 32] = raw
            .as_slice()
            .try_into()
            .context("server key wrong length")?;
        IdentityKey::from_seed(&seed)
    } else {
        let key = IdentityKey::generate();
        write_private(&key_path, hex::encode(key.seed()).as_bytes())?;
        key
    };

    let cert_path = dir.join("tls_cert.der");
    let tlskey_path = dir.join("tls_key.der");
    let tls = if cert_path.exists() && tlskey_path.exists() {
        TlsIdentity {
            cert_der: CertificateDer::from(fs::read(&cert_path)?),
            key_der: PrivatePkcs8KeyDer::from(fs::read(&tlskey_path)?),
        }
    } else {
        let tls = TlsIdentity::self_signed(hostnames)
            .map_err(|e| anyhow::anyhow!("tls identity: {e}"))?;
        fs::write(&cert_path, tls.cert_der.as_ref())?;
        write_private(&tlskey_path, tls.key_der.secret_pkcs8_der())?;
        tls
    };

    Ok(ServerIdentity { signing, tls })
}

/// Write a secret file with owner-only permissions.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_persists_across_loads() {
        let dir = tempfile::tempdir().unwrap();
        let a = load_or_create(dir.path(), &["localhost".into()]).unwrap();
        let b = load_or_create(dir.path(), &["localhost".into()]).unwrap();
        assert_eq!(a.signing.public(), b.signing.public());
        assert_eq!(a.tls.fingerprint(), b.tls.fingerprint());
    }
}
