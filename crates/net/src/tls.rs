//! Server TLS identity and client-side fingerprint pinning.
//!
//! A Burrow without an ACME certificate serves a self-signed cert; clients
//! authenticate it by pinning the cert's blake3 fingerprint, which travels
//! out of band (rabbit links, Looking Glass listings, `.well-known`).

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls_pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime};

use crate::NetError;

/// Install the ring crypto provider as the process default (idempotent).
pub fn ensure_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// A server's TLS identity: certificate + private key (+ fingerprint).
pub struct TlsIdentity {
    pub cert_der: CertificateDer<'static>,
    pub key_der: PrivatePkcs8KeyDer<'static>,
}

impl TlsIdentity {
    /// Generate a fresh self-signed identity for the given hostnames.
    pub fn self_signed(hostnames: &[String]) -> Result<Self, NetError> {
        let certified = rcgen::generate_simple_self_signed(hostnames.to_vec())
            .map_err(|e| NetError::Tls(e.to_string()))?;
        Ok(Self {
            cert_der: certified.cert.der().clone(),
            key_der: PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der()),
        })
    }

    /// blake3 fingerprint of the certificate DER — what clients pin.
    pub fn fingerprint(&self) -> CertFingerprint {
        CertFingerprint::of_der(&self.cert_der)
    }

    /// A plain rustls server config presenting this identity, for TLS-over-TCP
    /// listeners (NNTPS, STARTTLS upgrades). No ALPN — protocols that need it
    /// (QUIC) build their own config.
    pub fn server_config(&self) -> Result<Arc<rustls::ServerConfig>, NetError> {
        ensure_crypto_provider();
        let cfg = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![self.cert_der.clone()], self.clone_key().into())
            .map_err(|e| NetError::Tls(e.to_string()))?;
        Ok(Arc::new(cfg))
    }

    pub fn clone_key(&self) -> PrivatePkcs8KeyDer<'static> {
        self.key_der.clone_key()
    }
}

/// A pinned certificate fingerprint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CertFingerprint(pub [u8; 32]);

impl CertFingerprint {
    pub fn of_der(der: &CertificateDer<'_>) -> Self {
        CertFingerprint(*blake3::hash(der.as_ref()).as_bytes())
    }

    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        let raw = hex::decode(s).ok()?;
        Some(CertFingerprint(raw.try_into().ok()?))
    }
}

/// How a client authenticates the server certificate.
#[derive(Clone)]
pub enum ServerAuth {
    /// Standard WebPKI validation against the platform/Mozilla roots
    /// (used once ACME certs land in Wave 1).
    WebPki,
    /// Pin an exact certificate fingerprint (self-signed deployments).
    Pinned(CertFingerprint),
}

/// rustls verifier that accepts exactly one pinned certificate.
#[derive(Debug)]
pub struct PinnedCertVerifier {
    fingerprint: CertFingerprint,
    provider: Arc<CryptoProvider>,
}

impl PinnedCertVerifier {
    pub fn new(fingerprint: CertFingerprint) -> Self {
        ensure_crypto_provider();
        Self {
            fingerprint,
            provider: CryptoProvider::get_default()
                .expect("provider installed")
                .clone(),
        }
    }
}

impl ServerCertVerifier for PinnedCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if CertFingerprint::of_der(end_entity) == self.fingerprint {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_config_presents_identity_without_alpn() {
        let id = TlsIdentity::self_signed(&["localhost".into()]).unwrap();
        let cfg = id.server_config().unwrap();
        assert!(cfg.alpn_protocols.is_empty(), "no ALPN on the TCP config");
    }

    #[test]
    fn self_signed_identity_and_fingerprint() {
        let id = TlsIdentity::self_signed(&["localhost".into()]).unwrap();
        let fp = id.fingerprint();
        assert_eq!(CertFingerprint::from_hex(&fp.to_hex()), Some(fp));
        // A second identity has a different fingerprint.
        let other = TlsIdentity::self_signed(&["localhost".into()]).unwrap();
        assert_ne!(fp, other.fingerprint());
    }
}
