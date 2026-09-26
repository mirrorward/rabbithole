//! RH-87: real IPv6 feed transport, Host/request targets, redirects and bounds.

use std::{net::SocketAddr, time::Duration};

use burrow::syndication::{http_get, MAX_BODY_BYTES, MAX_REDIRECTS};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

const BODY: &str = "<rss version=\"2.0\"><channel><title>IPv6 feed</title></channel></rss>";

async fn read_request(socket: &mut TcpStream) -> String {
    let mut request = Vec::new();
    while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
        let mut bytes = [0; 2048];
        let n = socket.read(&mut bytes).await.unwrap();
        assert!(n > 0, "request head arrives before EOF");
        request.extend_from_slice(&bytes[..n]);
        assert!(request.len() <= 8192);
    }
    String::from_utf8(request).unwrap()
}

fn response(body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"v6\"\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn redirect(location: &str) -> Vec<u8> {
    format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n").into_bytes()
}

async fn listener() -> TcpListener {
    TcpListener::bind("[::1]:0")
        .await
        .expect("IPv6 loopback listener")
}

fn serve(listener: TcpListener, responses: Vec<Vec<u8>>) -> JoinHandle<Vec<String>> {
    tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(5), async move {
            let mut requests = Vec::new();
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                requests.push(read_request(&mut socket).await);
                socket.write_all(&response).await.unwrap();
                socket.shutdown().await.unwrap();
            }
            requests
        })
        .await
        .expect("bounded feed fixture")
    })
}

fn assert_head(request: &str, addr: SocketAddr, path: &str) {
    assert!(
        request.starts_with(&format!("GET {path} HTTP/1.1\r\n")),
        "{request}"
    );
    assert!(
        request.contains(&format!("\r\nHost: {addr}\r\n")),
        "{request}"
    );
    assert!(
        !request.contains('#'),
        "fragments are not sent to HTTP peers"
    );
}

#[tokio::test]
async fn direct_ipv6_and_dns_feeds_preserve_queries_headers_and_validators() {
    let listener = listener().await;
    let addr = listener.local_addr().unwrap();
    let served = serve(listener, vec![response(BODY), response(BODY)]);
    let url = format!("http://{addr}?format=rss#not-on-wire");
    let fetched = http_get(
        &url,
        Some("\"previous\""),
        Some("Wed, 02 Jul 2003 05:00:00 GMT"),
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(fetched.status, 200);
    assert_eq!(fetched.body, BODY.as_bytes());
    assert_eq!(fetched.etag.as_deref(), Some("\"v6\""));
    // DNS remains a resolver input; localhost reaches the same IPv6 listener.
    let fetched = http_get(
        &format!("http://localhost:{}/feed", addr.port()),
        None,
        None,
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(fetched.body, BODY.as_bytes());
    let requests = served.await.unwrap();
    assert_head(&requests[0], addr, "/?format=rss");
    assert!(requests[0].contains("\r\nIf-None-Match: \"previous\"\r\n"));
    assert!(requests[0].contains("\r\nIf-Modified-Since: Wed, 02 Jul 2003 05:00:00 GMT\r\n"));
    assert!(requests[1].starts_with("GET /feed HTTP/1.1\r\n"));
    assert!(requests[1].contains(&format!("\r\nHost: localhost:{}\r\n", addr.port())));
}

#[tokio::test]
async fn ipv6_absolute_scheme_relative_and_path_redirects_reach_the_final_feed() {
    for form in [
        "absolute",
        "scheme-relative",
        "rooted",
        "relative",
        "query-only",
    ] {
        let listener = listener().await;
        let addr = listener.local_addr().unwrap();
        let (location, expected) = match form {
            "absolute" => (format!("http://{addr}?fresh=1#part"), "/?fresh=1"),
            "scheme-relative" => (format!("//{addr}/final#part"), "/final"),
            "rooted" => ("/final?fresh=1#part".into(), "/final?fresh=1"),
            "relative" => ("final#part".into(), "/dir/final"),
            _ => ("?fresh=1#part".into(), "/dir/start?fresh=1"),
        };
        let served = serve(listener, vec![redirect(&location), response(BODY)]);
        let fetched = http_get(
            &format!("http://{addr}/dir/start?next=/ignored"),
            None,
            None,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(fetched.body, BODY.as_bytes(), "{form}");
        let requests = served.await.unwrap();
        assert_head(&requests[0], addr, "/dir/start?next=/ignored");
        assert_head(&requests[1], addr, expected);
    }
}

#[tokio::test]
async fn ipv4_feed_can_redirect_to_a_bracketed_ipv6_authority() {
    let destination = listener().await;
    let destination_addr = destination.local_addr().unwrap();
    let destination = serve(destination, vec![response(BODY)]);
    let source = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let source_addr = source.local_addr().unwrap();
    let source = serve(
        source,
        vec![redirect(&format!("http://{destination_addr}/feed"))],
    );
    let fetched = http_get(
        &format!("http://{source_addr}/start"),
        None,
        None,
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(fetched.body, BODY.as_bytes());
    assert_head(&source.await.unwrap()[0], source_addr, "/start");
    assert_head(&destination.await.unwrap()[0], destination_addr, "/feed");
}

#[tokio::test]
async fn ipv6_fetches_retain_body_redirect_and_whole_fetch_limits() {
    let large = listener().await;
    let large_addr = large.local_addr().unwrap();
    let large = serve(large, vec![response(&"x".repeat(MAX_BODY_BYTES + 1))]);
    let error = http_get(
        &format!("http://{large_addr}/large"),
        None,
        None,
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("cap"), "{error}");
    assert_eq!(large.await.unwrap().len(), 1);

    let looping = listener().await;
    let looping_addr = looping.local_addr().unwrap();
    let looping = serve(looping, vec![redirect("/again"); MAX_REDIRECTS + 1]);
    let fetched = http_get(
        &format!("http://{looping_addr}/again"),
        None,
        None,
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(
        fetched.status, 302,
        "hop limit returns the final HTTP response"
    );
    assert_eq!(looping.await.unwrap().len(), MAX_REDIRECTS + 1);

    let stalled = listener().await;
    let stalled_addr = stalled.local_addr().unwrap();
    let (release, released) = tokio::sync::oneshot::channel();
    let stalled = tokio::spawn(async move {
        let (mut socket, _) = stalled.accept().await.unwrap();
        let request = read_request(&mut socket).await;
        let _ = released.await;
        request
    });
    let error = http_get(
        &format!("http://{stalled_addr}/stalled"),
        None,
        None,
        Duration::from_millis(500),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("timed out"), "{error}");
    release.send(()).unwrap();
    assert_head(&stalled.await.unwrap(), stalled_addr, "/stalled");
}
