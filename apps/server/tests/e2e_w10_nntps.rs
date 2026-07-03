//! Wave 10 end-to-end tests: TLS on the NNTP surfaces — the dedicated NNTPS
//! listener (implicit TLS, RFC 8143), the `STARTTLS` upgrade on the plaintext
//! listener (RFC 4642), and the RFC 4643 `AUTHINFO` gate
//! (`nntp_auth_require_tls`). The TLS client pins the burrow's certificate
//! fingerprint, exactly as the native QUIC clients do.

use std::sync::Arc;
use std::time::Duration;

use burrow::Burrow;
use rabbithole_net::tls::{CertFingerprint, PinnedCertVerifier};
use rabbithole_server_core::{Role, ServerConfig};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// A rustls client connector that authenticates the server by its pinned
/// blake3 certificate fingerprint — the self-signed trust model every other
/// burrow transport uses.
fn pinned_connector(fingerprint: CertFingerprint) -> TlsConnector {
    // `PinnedCertVerifier::new` installs the ring provider (idempotent), so
    // the builder below resolves it.
    let verifier = Arc::new(PinnedCertVerifier::new(fingerprint));
    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    TlsConnector::from(Arc::new(config))
}

/// A minimal scripted NNTP client generic over the stream — plain TCP or a
/// client-side TLS stream (same shape as the client in `e2e_w102`).
struct Client<S> {
    reader: BufReader<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Client<S> {
    fn new(stream: S) -> Client<S> {
        Client {
            reader: BufReader::new(stream),
        }
    }

    /// Read one status line (CRLF stripped).
    async fn read_line(&mut self) -> String {
        let mut line = String::new();
        let n = tokio::time::timeout(Duration::from_secs(5), self.reader.read_line(&mut line))
            .await
            .expect("server responded")
            .unwrap();
        assert!(n > 0, "server closed unexpectedly");
        line.trim_end_matches(['\r', '\n']).to_string()
    }

    /// Read a multi-line data block terminated by a "." line (un-dot-stuffed).
    async fn read_block(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        loop {
            let line = self.read_line().await;
            if line == "." {
                break;
            }
            out.push(line.strip_prefix('.').unwrap_or(&line).to_string());
        }
        out
    }

    async fn send(&mut self, cmd: &str) {
        self.reader
            .get_mut()
            .write_all(format!("{cmd}\r\n").as_bytes())
            .await
            .unwrap();
        self.reader.get_mut().flush().await.unwrap();
    }

    /// Hand the raw stream back (for a STARTTLS handshake). The server sends
    /// nothing between its `382` and the handshake, so the read buffer is
    /// empty by construction.
    fn into_inner(self) -> S {
        self.reader.into_inner()
    }
}

/// Connect a pinned TLS client to `addr` (implicit TLS).
async fn tls_connect(
    addr: std::net::SocketAddr,
    fingerprint: CertFingerprint,
) -> Client<tokio_rustls::client::TlsStream<TcpStream>> {
    let tcp = TcpStream::connect(addr).await.unwrap();
    let connector = pinned_connector(fingerprint);
    let server_name = rustls_pki_types::ServerName::try_from("localhost").unwrap();
    let tls = tokio::time::timeout(Duration::from_secs(5), connector.connect(server_name, tcp))
        .await
        .expect("handshake completed")
        .unwrap();
    Client::new(tls)
}

/// Boot config with the given TLS/plaintext reader toggles. The AUTHINFO
/// gate keeps its secure-by-default `true` unless a test flips it.
fn reader_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Secure News Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

/// Seed an account and a postable board with one article.
async fn seed(burrow: &Burrow) {
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    burrow
        .shared
        .boards
        .create_board("rabbit.secure", "Secure", "TLS chatter", 2, None, 0)
        .await
        .unwrap();
    burrow
        .shared
        .boards
        .post(
            "rabbit.secure",
            None,
            "alice@secure-news-warren",
            &[7u8; 32],
            "First light",
            "The warren wakes, encrypted.",
            "text/plain",
            1_700_000_000_000,
        )
        .await
        .unwrap();
}

/// Implicit TLS (NNTPS): the full reader session — greeting, capabilities,
/// group/article reads, AUTHINFO (honoured because the transport is secure,
/// with the gate at its default `true`), and a POST that lands as a board
/// post. STARTTLS is refused: TLS is already up.
#[tokio::test]
async fn nntps_implicit_tls_reader_session() {
    let work = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        nntp_tls_enabled: true,
        nntp_tls_addr: "127.0.0.1:0".parse().unwrap(),
        ..reader_config(&work.path().join("srv"))
    };
    let burrow = Burrow::start(cfg).await.unwrap();
    seed(&burrow).await;

    let addr = burrow.nntp_tls_addr.expect("nntps enabled");
    assert!(burrow.nntp_addr.is_none(), "plaintext reader not requested");
    let mut client = tls_connect(addr, burrow.fingerprint).await;

    // Greeting arrives over TLS.
    assert!(client.read_line().await.starts_with("200"));

    // Secure capabilities: AUTHINFO is offered, STARTTLS is not.
    client.send("CAPABILITIES").await;
    assert!(client.read_line().await.starts_with("101"));
    let caps = client.read_block().await;
    assert!(
        caps.iter().any(|l| l == "AUTHINFO USER"),
        "secure transport advertises AUTHINFO: {caps:?}"
    );
    assert!(
        !caps.iter().any(|l| l == "STARTTLS"),
        "no STARTTLS once TLS is up: {caps:?}"
    );

    // STARTTLS on an already-secure connection is refused (RFC 4642 §2.1).
    client.send("STARTTLS").await;
    assert!(client.read_line().await.starts_with("502"));

    // Ordinary reading works over the encrypted stream.
    client.send("GROUP rabbit.secure").await;
    let sel = client.read_line().await;
    assert!(sel.starts_with("211"), "group selected: {sel:?}");
    client.send("ARTICLE 1").await;
    assert!(client.read_line().await.starts_with("220"));
    let art = client.read_block().await;
    assert!(art.iter().any(|l| l == "The warren wakes, encrypted."));

    // AUTHINFO is honoured with the gate at its default: TLS satisfies it.
    client.send("AUTHINFO USER alice").await;
    assert!(client.read_line().await.starts_with("381"));
    client.send("AUTHINFO PASS pw-pw-pw").await;
    assert!(client.read_line().await.starts_with("281"));

    // POST over TLS lands as a real board post.
    client.send("POST").await;
    assert!(client.read_line().await.starts_with("340"));
    client
        .send("Newsgroups: rabbit.secure\r\nSubject: Via NNTPS\r\n\r\nCiphertext outside, plaintext inside.\r\n.")
        .await;
    assert!(client.read_line().await.starts_with("240"));
    client.send("QUIT").await;
    assert!(client.read_line().await.starts_with("205"));

    let threads = burrow
        .shared
        .boards
        .threads("rabbit.secure", 100)
        .await
        .unwrap();
    assert!(
        threads
            .iter()
            .any(|(root, _, _)| root.subject == "Via NNTPS"),
        "NNTPS post is a board post"
    );

    burrow.shutdown().await;
}

/// STARTTLS on the plaintext listener: AUTHINFO is refused with 483 before
/// the upgrade (gate at its default), the verb answers 382 and the handshake
/// completes, session state (selected group) is discarded per RFC 4642, and
/// AUTHINFO then succeeds over the upgraded stream.
#[tokio::test]
async fn starttls_upgrade_resets_state_and_unlocks_authinfo() {
    let work = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        nntp_enabled: true,
        nntp_addr: "127.0.0.1:0".parse().unwrap(),
        ..reader_config(&work.path().join("srv"))
    };
    let burrow = Burrow::start(cfg).await.unwrap();
    seed(&burrow).await;

    let addr = burrow.nntp_addr.expect("nntp enabled");
    let mut plain = Client::new(TcpStream::connect(addr).await.unwrap());
    assert!(plain.read_line().await.starts_with("200"));

    // Pre-TLS capabilities: STARTTLS offered, AUTHINFO withheld (RFC 4643).
    plain.send("CAPABILITIES").await;
    assert!(plain.read_line().await.starts_with("101"));
    let caps = plain.read_block().await;
    assert!(caps.iter().any(|l| l == "STARTTLS"), "caps: {caps:?}");
    assert!(
        !caps.iter().any(|l| l == "AUTHINFO USER"),
        "AUTHINFO not advertised on plaintext while the gate is up: {caps:?}"
    );

    // Credentials on plaintext: 483, encryption required — for both halves.
    plain.send("AUTHINFO USER alice").await;
    assert!(plain.read_line().await.starts_with("483"));
    plain.send("AUTHINFO PASS pw-pw-pw").await;
    assert!(plain.read_line().await.starts_with("483"));

    // Select a group so the upgrade has state to discard.
    plain.send("GROUP rabbit.secure").await;
    assert!(plain.read_line().await.starts_with("211"));

    // Upgrade: 382, then the TLS handshake on the same connection.
    plain.send("STARTTLS").await;
    assert!(plain.read_line().await.starts_with("382"));
    let tcp = plain.into_inner();
    let connector = pinned_connector(burrow.fingerprint);
    let server_name = rustls_pki_types::ServerName::try_from("localhost").unwrap();
    let tls = tokio::time::timeout(Duration::from_secs(5), connector.connect(server_name, tcp))
        .await
        .expect("handshake completed")
        .unwrap();
    let mut client = Client::new(tls);

    // No second greeting (RFC 4642 §2.2.2): the next response answers our
    // next command. The group selection did not survive the upgrade.
    client.send("ARTICLE").await;
    assert!(
        client.read_line().await.starts_with("412"),
        "group state discarded across the upgrade"
    );

    // Capabilities flipped: AUTHINFO in, STARTTLS out.
    client.send("CAPABILITIES").await;
    assert!(client.read_line().await.starts_with("101"));
    let caps = client.read_block().await;
    assert!(caps.iter().any(|l| l == "AUTHINFO USER"), "caps: {caps:?}");
    assert!(!caps.iter().any(|l| l == "STARTTLS"), "caps: {caps:?}");

    // AUTHINFO now succeeds; a second STARTTLS is refused.
    client.send("AUTHINFO USER alice").await;
    assert!(client.read_line().await.starts_with("381"));
    client.send("AUTHINFO PASS pw-pw-pw").await;
    assert!(client.read_line().await.starts_with("281"));
    client.send("STARTTLS").await;
    assert!(client.read_line().await.starts_with("502"));

    // And the session is fully usable.
    client.send("GROUP rabbit.secure").await;
    assert!(client.read_line().await.starts_with("211"));
    client.send("QUIT").await;
    assert!(client.read_line().await.starts_with("205"));

    burrow.shutdown().await;
}

/// The AUTHINFO gate follows `nntp_auth_require_tls` live: on (default) a
/// plaintext AUTHINFO answers 483; flipped off, the same session's next
/// attempt is honoured.
#[tokio::test]
async fn plaintext_authinfo_gate_follows_config() {
    let work = tempfile::tempdir().unwrap();
    let cfg = ServerConfig {
        nntp_enabled: true,
        nntp_addr: "127.0.0.1:0".parse().unwrap(),
        ..reader_config(&work.path().join("srv"))
    };
    let burrow = Burrow::start(cfg).await.unwrap();
    seed(&burrow).await;

    let addr = burrow.nntp_addr.expect("nntp enabled");
    let mut client = Client::new(TcpStream::connect(addr).await.unwrap());
    assert!(client.read_line().await.starts_with("200"));

    client.send("AUTHINFO USER alice").await;
    assert!(
        client.read_line().await.starts_with("483"),
        "secure-by-default: plaintext AUTHINFO refused"
    );

    // The knob applies live (checked per command).
    assert!(burrow
        .shared
        .config
        .set_key("nntp_auth_require_tls", "false")
        .unwrap());
    client.send("AUTHINFO USER alice").await;
    assert!(client.read_line().await.starts_with("381"));
    client.send("AUTHINFO PASS pw-pw-pw").await;
    assert!(client.read_line().await.starts_with("281"));

    client.send("QUIT").await;
    assert!(client.read_line().await.starts_with("205"));
    burrow.shutdown().await;
}

/// The peer feed shares the TLS plumbing: implicit TLS on its own listener,
/// STARTTLS + the AUTHINFO gate on the plaintext one.
#[tokio::test]
async fn feed_tls_implicit_and_starttls() {
    let work = tempfile::tempdir().unwrap();
    let mut peers = std::collections::HashMap::new();
    peers.insert("hub".to_string(), "feed-pw".to_string());
    let cfg = ServerConfig {
        nntp_feed_enabled: true,
        nntp_feed_addr: "127.0.0.1:0".parse().unwrap(),
        nntp_feed_tls_enabled: true,
        nntp_feed_tls_addr: "127.0.0.1:0".parse().unwrap(),
        nntp_feed_peers: peers,
        ..reader_config(&work.path().join("srv"))
    };
    let burrow = Burrow::start(cfg).await.unwrap();

    // Implicit TLS: AUTHINFO honoured with the gate at its default.
    let tls_addr = burrow.nntp_feed_tls_addr.expect("feed tls enabled");
    let mut peer = tls_connect(tls_addr, burrow.fingerprint).await;
    assert!(peer.read_line().await.starts_with("200"));
    peer.send("AUTHINFO USER hub").await;
    assert!(peer.read_line().await.starts_with("381"));
    peer.send("AUTHINFO PASS feed-pw").await;
    assert!(peer.read_line().await.starts_with("281"));
    peer.send("MODE STREAM").await;
    assert!(peer.read_line().await.starts_with("203"));
    peer.send("QUIT").await;
    assert!(peer.read_line().await.starts_with("205"));

    // Plaintext feed: 483 before STARTTLS, 281 after the upgrade.
    let plain_addr = burrow.nntp_feed_addr.expect("feed enabled");
    let mut plain = Client::new(TcpStream::connect(plain_addr).await.unwrap());
    assert!(plain.read_line().await.starts_with("200"));
    plain.send("AUTHINFO USER hub").await;
    assert!(plain.read_line().await.starts_with("483"));
    plain.send("STARTTLS").await;
    assert!(plain.read_line().await.starts_with("382"));
    let tcp = plain.into_inner();
    let connector = pinned_connector(burrow.fingerprint);
    let server_name = rustls_pki_types::ServerName::try_from("localhost").unwrap();
    let tls = tokio::time::timeout(Duration::from_secs(5), connector.connect(server_name, tcp))
        .await
        .expect("handshake completed")
        .unwrap();
    let mut peer = Client::new(tls);
    peer.send("AUTHINFO USER hub").await;
    assert!(peer.read_line().await.starts_with("381"));
    peer.send("AUTHINFO PASS feed-pw").await;
    assert!(peer.read_line().await.starts_with("281"));
    peer.send("STARTTLS").await;
    assert!(peer.read_line().await.starts_with("502"));
    peer.send("QUIT").await;
    assert!(peer.read_line().await.starts_with("205"));

    burrow.shutdown().await;
}

/// Both TLS listeners are off by default.
#[tokio::test]
async fn nntps_off_by_default() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(reader_config(&work.path().join("srv")))
        .await
        .unwrap();
    assert!(burrow.nntp_tls_addr.is_none(), "nntps off by default");
    assert!(
        burrow.nntp_feed_tls_addr.is_none(),
        "feed tls off by default"
    );
    burrow.shutdown().await;
}
