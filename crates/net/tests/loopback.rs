//! Loopback integration tests: a client and server exchanging Hello over
//! each transport.

use rabbithole_net::quic::{QuicListener, QuicTransport};
use rabbithole_net::tls::{ServerAuth, TlsIdentity};
use rabbithole_net::ws::{WsListener, WsTransport};
use rabbithole_net::{Listener, Transport, TransportKind};
use rabbithole_proto::{
    Capability, CapabilitySet, Frame, Hello, HelloAck, RequestId, PROTOCOL_VERSION,
};

fn hello() -> Hello {
    Hello::new(
        "loopback-test",
        env!("CARGO_PKG_VERSION"),
        CapabilitySet(vec![Capability::new("session-resume")]),
    )
}

fn hello_ack() -> HelloAck {
    HelloAck::new(
        PROTOCOL_VERSION,
        CapabilitySet::default(),
        "test burrow",
        env!("CARGO_PKG_VERSION"),
        [7u8; 32],
    )
}

/// Drive one request/reply exchange over any listener/transport pair.
async fn exchange(
    mut listener: Box<dyn Listener>,
    transport: Box<dyn Transport>,
    endpoint: String,
    expected_kind: TransportKind,
) {
    let server = tokio::spawn(async move {
        let mut conn = listener.accept().await.expect("accept");
        assert_eq!(conn.peer().transport, expected_kind);
        let frame = conn.recv().await.expect("recv").expect("open");
        let decoded = frame.decode::<Hello>().expect("is hello").expect("decodes");
        assert_eq!(decoded.client_name, "loopback-test");
        let reply = Frame::reply_to(&frame, &hello_ack()).unwrap();
        conn.send(reply).await.expect("send reply");
        conn.close().await;
    });

    let mut conn = transport.connect(&endpoint).await.expect("connect");
    let req = Frame::request(RequestId(1), &hello()).unwrap();
    conn.send(req).await.expect("send");
    let reply = conn.recv().await.expect("recv").expect("reply");
    assert_eq!(reply.id, RequestId(1));
    let ack = reply
        .decode::<HelloAck>()
        .expect("is ack")
        .expect("decodes");
    assert_eq!(ack.server_name, "test burrow");
    conn.close().await;

    server.await.unwrap();
}

#[tokio::test]
async fn quic_loopback_hello() {
    let identity = TlsIdentity::self_signed(&["localhost".into()]).unwrap();
    let fingerprint = identity.fingerprint();
    let listener = QuicListener::bind("127.0.0.1:0".parse().unwrap(), &identity).unwrap();
    let endpoint = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let transport = QuicTransport::new("localhost", ServerAuth::Pinned(fingerprint));
    exchange(
        Box::new(listener),
        Box::new(transport),
        endpoint,
        TransportKind::Quic,
    )
    .await;
}

#[tokio::test]
async fn quic_rejects_wrong_fingerprint() {
    let identity = TlsIdentity::self_signed(&["localhost".into()]).unwrap();
    let listener = QuicListener::bind("127.0.0.1:0".parse().unwrap(), &identity).unwrap();
    let endpoint = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());

    // Pin a fingerprint that doesn't match the server's cert.
    let wrong = TlsIdentity::self_signed(&["localhost".into()])
        .unwrap()
        .fingerprint();
    let transport = QuicTransport::new("localhost", ServerAuth::Pinned(wrong));
    // Keep the listener alive while the client attempts the handshake.
    let mut listener = listener;
    let accept = tokio::spawn(async move {
        let _ = listener.accept().await;
    });
    let result = transport.connect(&endpoint).await;
    assert!(
        result.is_err(),
        "handshake must fail on fingerprint mismatch"
    );
    accept.abort();
}

#[tokio::test]
async fn quic_bulk_stream_transfers_bytes() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let identity = TlsIdentity::self_signed(&["localhost".into()]).unwrap();
    let fingerprint = identity.fingerprint();
    let mut listener = QuicListener::bind("127.0.0.1:0".parse().unwrap(), &identity).unwrap();
    let endpoint = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());

    // A oneshot lets the client tell the server it has finished reading, so
    // the server holds the QUIC connection open until then (dropping it early
    // would discard the in-flight echo — the same race close() guards).
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

    // Server: accept the control connection, then a bulk stream, and echo
    // a large payload back over it (proving it's independent of control).
    let server = tokio::spawn(async move {
        let conn = listener.accept().await.expect("accept");
        let bulk = conn.bulk().expect("quic offers bulk streams");
        let (mut send, mut recv) = bulk.accept().await.expect("accept bulk");
        let mut buf = Vec::new();
        recv.read_to_end(&mut buf).await.expect("read bulk");
        send.write_all(&buf).await.expect("echo");
        send.shutdown().await.ok(); // FIN so the client's read_to_end returns
        done_rx.await.ok(); // keep the connection alive until the client is done
        buf.len()
    });

    let transport = QuicTransport::new("localhost", ServerAuth::Pinned(fingerprint));
    let conn = transport.connect(&endpoint).await.expect("connect");
    let bulk = conn.bulk().expect("client bulk");
    let (mut send, mut recv) = bulk.open().await.expect("open bulk");
    let payload = vec![0xABu8; 256 * 1024];
    send.write_all(&payload).await.expect("write");
    send.shutdown().await.expect("shutdown"); // FIN so the server sees EOF
    let mut got = Vec::new();
    recv.read_to_end(&mut got).await.expect("read echo");
    assert_eq!(got, payload, "bulk stream round-trips the payload");
    done_tx.send(()).ok();
    server.await.unwrap();
}

#[tokio::test]
async fn ws_reports_no_bulk_streams() {
    let listener = WsListener::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let endpoint = format!("ws://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let mut listener = listener;
    let accept = tokio::spawn(async move {
        let conn = listener.accept().await.expect("accept");
        assert!(conn.bulk().is_none(), "ws has no bulk streams");
    });
    let conn = WsTransport.connect(&endpoint).await.expect("connect");
    assert!(conn.bulk().is_none(), "ws client has no bulk streams");
    accept.await.unwrap();
}

#[tokio::test]
async fn ws_loopback_hello() {
    let listener = WsListener::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let endpoint = format!("ws://127.0.0.1:{}", listener.local_addr().unwrap().port());
    exchange(
        Box::new(listener),
        Box::new(WsTransport),
        endpoint,
        TransportKind::WebSocket,
    )
    .await;
}
