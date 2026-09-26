//! RH-87: strict feed authorities and real certificate identity verification.

use super::*;

#[test]
fn ipv6_authorities_keep_host_header_socket_address_and_query_separate() {
    for (url, host, port, header, path) in [
        ("http://[::1]", "::1", 80, "[::1]", "/"),
        (
            "http://[::1]:80?fresh=1#top",
            "::1",
            80,
            "[::1]",
            "/?fresh=1",
        ),
        ("https://[::1]:443/feed#top", "::1", 443, "[::1]", "/feed"),
        (
            "https://[2001:db8::1]:8443/feed?q=1",
            "2001:db8::1",
            8443,
            "[2001:db8::1]:8443",
            "/feed?q=1",
        ),
        (
            "http://[::ffff:192.0.2.1]:8080/",
            "::ffff:192.0.2.1",
            8080,
            "[::ffff:192.0.2.1]:8080",
            "/",
        ),
        (
            "http://localhost:8080?next=/elsewhere#top",
            "localhost",
            8080,
            "localhost:8080",
            "/?next=/elsewhere",
        ),
        (
            "https://feeds.example#top",
            "feeds.example",
            443,
            "feeds.example",
            "/",
        ),
        (
            "http://127.0.0.1/feed",
            "127.0.0.1",
            80,
            "127.0.0.1",
            "/feed",
        ),
    ] {
        let parsed = FeedUrl::parse(url).unwrap();
        assert_eq!(parsed.host, host, "{url}");
        assert_eq!(parsed.port, port, "{url}");
        assert_eq!(parsed.host_header(), header, "{url}");
        assert_eq!(parsed.path, path, "{url}");
        let request = build_request(&parsed, None, None);
        assert!(request.starts_with(&format!("GET {path} HTTP/1.1\r\n")));
        assert!(request.contains(&format!("\r\nHost: {header}\r\n")));
        assert!(!request.contains('#'));
    }
}

#[test]
fn malformed_authorities_fail_without_exposing_the_url() {
    for authority in [
        "[localhost]",
        "[127.0.0.1]",
        "[]",
        "[::gg]",
        "[::1",
        "::1",
        "2001:db8::1:80",
        "[::1]]",
        "[::1]oops",
        "[::1]:",
        "[::1]:+80",
        "[::1]:-1",
        "[::1]:65536",
        "[::1]:80:90",
        "host:",
        "host:+80",
        "host:abc",
        "host:65536",
        ":80",
        "host]",
        "[fe80::1%25en0]",
        "[v1.example]",
        "host\\other",
        "user:secret@[::1]",
    ] {
        let url = format!("http://{authority}/feed?private-token#secret-fragment");
        let error = FeedUrl::parse(&url).unwrap_err().to_string();
        assert!(!error.contains(&url));
        assert!(!error.contains("private-token"));
        assert!(!error.contains("secret"));
    }
    for url in [
        "http://[::1]/a b",
        "http://[::1]/a\r\nInjected:yes",
        "http:// host/feed",
    ] {
        assert!(FeedUrl::parse(url).is_err());
    }
    assert!(FeedUrl::parse("http://[::1]/a%20b?encoded=%0d%0a").is_ok());
}

#[test]
fn ipv6_redirects_preserve_authority_and_replace_queries_without_fragments() {
    let current = FeedUrl::parse("https://[::1]:8443/dir/feed?next=/elsewhere").unwrap();
    for (location, host, port, path) in [
        ("https://[2001:db8::2]/next", "2001:db8::2", 443, "/next"),
        ("//[::2]:9443?fresh=1#part", "::2", 9443, "/?fresh=1"),
        ("/next?fresh=1#part", "::1", 8443, "/next?fresh=1"),
        ("next.xml#part", "::1", 8443, "/dir/next.xml"),
        ("?fresh=1#part", "::1", 8443, "/dir/feed?fresh=1"),
        ("#part", "::1", 8443, "/dir/feed?next=/elsewhere"),
    ] {
        let next = resolve_location(&current, location).unwrap();
        assert_eq!(
            (next.host.as_str(), next.port, next.path.as_str()),
            (host, port, path)
        );
        assert!(next.tls);
    }
    for location in [
        "http://[dns]/",
        "//::1/",
        "//[::1]:abc/",
        "/x\r\nInjected:yes",
    ] {
        assert!(resolve_location(&current, location).is_err());
    }
}

async fn verified_tls_fetch(
    cert_name: &str,
    host: &str,
) -> (Result<Vec<u8>>, Option<(String, Option<String>)>) {
    use rabbithole_net::tls::TlsIdentity;
    use tokio::net::TcpListener;

    let identity = TlsIdentity::self_signed(&[cert_name.to_string()]).unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(identity.server_config().unwrap());
    let mut roots = rustls::RootCertStore::empty();
    roots.add(identity.cert_der.clone()).unwrap();
    let config = Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    );
    let listener = TcpListener::bind("[::1]:0")
        .await
        .expect("IPv6 loopback listener");
    let addr = listener.local_addr().unwrap();
    let served = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let Ok(mut tls) = acceptor.accept(socket).await else {
            return None;
        };
        let sni = tls.get_ref().1.server_name().map(str::to_string);
        let mut request = Vec::new();
        while !request.windows(4).any(|part| part == b"\r\n\r\n") {
            let mut bytes = [0; 1024];
            let n = tls.read(&mut bytes).await.unwrap();
            assert!(n > 0);
            request.extend_from_slice(&bytes[..n]);
            assert!(request.len() < 8192);
        }
        tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nrss")
            .await
            .unwrap();
        tls.shutdown().await.unwrap();
        Some((String::from_utf8(request).unwrap(), sni))
    });
    let authority = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let target = FeedUrl::parse(&format!("https://{authority}:{}/feed", addr.port())).unwrap();
    let request = build_request(&target, None, None);
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let socket = TcpStream::connect(addr).await.unwrap();
        tls_exchange_with_config(socket, &target.host, request.as_bytes(), config).await
    })
    .await
    .expect("TLS fixture completes");
    let observed = tokio::time::timeout(Duration::from_secs(5), served)
        .await
        .unwrap()
        .unwrap();
    (result, observed)
}

#[tokio::test]
async fn ipv6_tls_verifies_ip_san_and_does_not_send_dns_sni() {
    let (valid, observed) = verified_tls_fetch("::1", "::1").await;
    assert_eq!(parse_http_response(&valid.unwrap()).unwrap().body, b"rss");
    let (request, sni) = observed.unwrap();
    assert!(request.contains("\r\nHost: [::1]:"));
    assert_eq!(sni, None, "IP identities do not become DNS SNI");
    for wrong_san in ["::2", "localhost"] {
        let (invalid, _) = verified_tls_fetch(wrong_san, "::1").await;
        let error = invalid.unwrap_err().to_string();
        assert!(
            error.contains("certificate"),
            "wrong SAN must fail verification: {error}"
        );
    }
}

#[tokio::test]
async fn dns_tls_still_verifies_dns_san_and_sends_sni() {
    let (valid, observed) = verified_tls_fetch("localhost", "localhost").await;
    assert_eq!(parse_http_response(&valid.unwrap()).unwrap().body, b"rss");
    let (request, sni) = observed.unwrap();
    assert!(request.contains("\r\nHost: localhost:"));
    assert_eq!(sni.as_deref(), Some("localhost"));
}
