//! QUIC connection migration (mobile WiFi↔cellular resilience) and the
//! WebSocket fallback contract.
//!
//! The migration test proves the strong property: after the client moves to a
//! fresh local UDP socket via [`Connection::migrate`], the *same* QUIC session
//! keeps working — no reconnect, no re-handshake, no re-auth. The evidence:
//!
//! - the client's local socket address actually changes across `migrate`;
//! - the remote address is unchanged (we never re-dialed);
//! - a second request round-trips over the same connection object; and
//! - the server handled both requests on a single accepted connection (it
//!   never re-accepted), so the second request cannot have come from a new
//!   connection.
//!
//! The WS test proves the fallback signal: WebSocket reports
//! [`NetError::Unsupported`] from `migrate`, telling callers to reconnect +
//! `auth_resume` instead.

use std::time::Duration;

use rabbithole_net::quic::{QuicListener, QuicTransport};
use rabbithole_net::tls::{ServerAuth, TlsIdentity};
use rabbithole_net::ws::{WsListener, WsTransport};
use rabbithole_net::{Connection, Listener, NetError, Transport};
use rabbithole_proto::{
    Capability, CapabilitySet, Frame, Hello, HelloAck, RequestId, PROTOCOL_VERSION,
};

/// Bounded so a stalled migration fails the test instead of hanging CI.
const TIMEOUT: Duration = Duration::from_secs(10);

fn hello() -> Hello {
    Hello::new(
        "migration-test",
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

/// Await `conn.recv()` with a bound; distinguishes "timed out" from "closed".
async fn recv_reply(conn: &mut Box<dyn Connection>, expect: RequestId) -> Frame {
    let reply = tokio::time::timeout(TIMEOUT, conn.recv())
        .await
        .expect("recv timed out")
        .expect("recv errored")
        .expect("peer closed instead of replying");
    assert_eq!(reply.id, expect, "reply id");
    reply
}

#[tokio::test]
async fn quic_connection_migrates_without_reconnect() {
    let identity = TlsIdentity::self_signed(&["localhost".into()]).unwrap();
    let fingerprint = identity.fingerprint();
    let listener = QuicListener::bind("127.0.0.1:0".parse().unwrap(), &identity).unwrap();
    let endpoint = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());

    // Server: accept exactly once, then reply to every request that arrives on
    // that one connection until the client FINs the control stream. The count
    // it returns is how many requests reached a *single* accepted connection.
    let mut listener = listener;
    let server = tokio::spawn(async move {
        let mut conn = listener.accept().await.expect("accept");
        // A server-accepted connection shares the listener endpoint, so it
        // cannot migrate and exposes no per-connection local socket.
        assert!(
            matches!(conn.migrate(), Err(NetError::Unsupported(_))),
            "server side must report migration unsupported",
        );
        assert!(conn.local_addr().is_none(), "server has no client endpoint");

        let mut handled = 0usize;
        while let Some(frame) = conn.recv().await.expect("server recv") {
            let reply = Frame::reply_to(&frame, &hello_ack()).unwrap();
            conn.send(reply).await.expect("server send reply");
            handled += 1;
        }
        conn.close().await;
        handled
    });

    let transport = QuicTransport::new("localhost", ServerAuth::Pinned(fingerprint));
    let mut conn = transport.connect(&endpoint).await.expect("connect");
    let remote_before = conn.peer().remote_addr;
    let local_before = conn.local_addr().expect("quic client exposes a local addr");

    // Request 1 over the original socket.
    conn.send(Frame::request(RequestId(1), &hello()).unwrap())
        .await
        .expect("send 1");
    recv_reply(&mut conn, RequestId(1)).await;

    // Migrate: move to a fresh local UDP socket without touching the session.
    conn.migrate().expect("quic client migrates");
    let local_after = conn.local_addr().expect("local addr after migrate");
    assert_ne!(
        local_before, local_after,
        "migration must move the connection to a new local socket",
    );
    assert_eq!(
        conn.peer().remote_addr,
        remote_before,
        "remote address unchanged — this is the same connection, not a reconnect",
    );

    // Request 2 over the SAME connection object, now on the new socket.
    conn.send(Frame::request(RequestId(2), &hello()).unwrap())
        .await
        .expect("send 2 after migration");
    recv_reply(&mut conn, RequestId(2)).await;

    conn.close().await;

    let handled = tokio::time::timeout(TIMEOUT, server)
        .await
        .expect("server join timed out")
        .expect("server task panicked");
    assert_eq!(
        handled, 2,
        "both requests must land on the one accepted connection (proving the \
         session migrated in place rather than reconnecting)",
    );
}

#[tokio::test]
async fn ws_migrate_reports_unsupported() {
    let listener = WsListener::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let endpoint = format!("ws://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let mut listener = listener;
    let accept = tokio::spawn(async move {
        // Hold the accepted connection so the client handshake completes.
        let _conn = listener.accept().await.expect("accept");
    });

    let conn = WsTransport.connect(&endpoint).await.expect("connect");
    let err = conn
        .migrate()
        .expect_err("websocket cannot migrate at the transport layer");
    assert!(
        matches!(err, NetError::Unsupported(_)),
        "ws migrate must return the documented Unsupported signal, got {err:?}",
    );

    tokio::time::timeout(TIMEOUT, accept)
        .await
        .expect("accept join timed out")
        .expect("accept task panicked");
}
