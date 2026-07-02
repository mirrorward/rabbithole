//! End-to-end HTRK round-trip: register a server over UDP, then read it back
//! over the TCP listing session and the plain-text status query, all on
//! ephemeral `127.0.0.1` ports.

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use looking_glass::{htrk, service, Registry, DEFAULT_TTL};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

/// Spawns all three listeners on ephemeral ports; returns
/// (registry, udp registration addr, tcp listing addr, tcp status addr).
async fn spawn_tracker() -> (
    Arc<Registry>,
    std::net::SocketAddr,
    std::net::SocketAddr,
    std::net::SocketAddr,
) {
    let registry = Arc::new(Registry::new(DEFAULT_TTL));
    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let listing = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let status = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let udp_addr = udp.local_addr().unwrap();
    let listing_addr = listing.local_addr().unwrap();
    let status_addr = status.local_addr().unwrap();
    tokio::spawn(service::run_registration_udp(udp, registry.clone()));
    tokio::spawn(service::run_listing_tcp(listing, registry.clone()));
    tokio::spawn(service::run_status_tcp(status, registry.clone()));
    (registry, udp_addr, listing_addr, status_addr)
}

/// Sends one registration datagram from an ephemeral UDP socket.
async fn send_registration(to: std::net::SocketAddr, reg: &htrk::Registration) {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.send_to(&reg.encode(), to).await.unwrap();
}

/// Waits (bounded) for the registry to reach `len` live entries.
async fn wait_for_len(registry: &Registry, len: usize) {
    for _ in 0..200 {
        if registry.len() == len {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!(
        "registry never reached {len} entries (has {})",
        registry.len()
    );
}

/// Performs a full HTRK listing session and returns the decoded servers.
async fn list_via_tcp(addr: std::net::SocketAddr) -> Vec<htrk::ListedServer> {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(&htrk::encode_hello()).await.unwrap();
    let mut hello = [0u8; htrk::HELLO_LEN];
    stream.read_exact(&mut hello).await.unwrap();
    htrk::decode_hello(&hello).unwrap();
    let mut rest = Vec::new();
    stream.read_to_end(&mut rest).await.unwrap();
    htrk::decode_listing(&rest).unwrap()
}

#[tokio::test]
async fn register_via_udp_then_list_via_tcp() {
    let (registry, udp_addr, listing_addr, _) = spawn_tracker().await;

    let reg = htrk::Registration {
        port: 5500,
        users_online: 4,
        pass_id: [0; 4],
        name: "Wonderland".into(),
        description: "Down the rabbit hole".into(),
    };
    send_registration(udp_addr, &reg).await;
    wait_for_len(&registry, 1).await;

    let servers = list_via_tcp(listing_addr).await;
    assert_eq!(servers.len(), 1);
    let server = &servers[0];
    assert_eq!(server.ip, Ipv4Addr::LOCALHOST);
    assert_eq!(server.port, 5500);
    assert_eq!(server.users_online, 4);
    assert_eq!(server.name, "Wonderland");
    assert_eq!(server.description, "Down the rabbit hole");
}

#[tokio::test]
async fn malformed_datagrams_are_ignored_and_heartbeats_refresh() {
    let (registry, udp_addr, listing_addr, _) = spawn_tracker().await;

    // Garbage first: must be dropped without killing the listener.
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.send_to(&[], udp_addr).await.unwrap();
    sock.send_to(&[0xFF; 3], udp_addr).await.unwrap();
    sock.send_to(&[0xFF; 500], udp_addr).await.unwrap();

    let mut reg = htrk::Registration {
        port: 5500,
        users_online: 1,
        pass_id: [0; 4],
        name: "March Hare".into(),
        description: String::new(),
    };
    send_registration(udp_addr, &reg).await;
    wait_for_len(&registry, 1).await;

    // A fresh heartbeat for the same (ip, port) replaces, not duplicates.
    reg.users_online = 6;
    send_registration(udp_addr, &reg).await;
    for _ in 0..200 {
        if list_via_tcp(listing_addr).await[0].users_online == 6 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let servers = list_via_tcp(listing_addr).await;
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].users_online, 6);
    assert_eq!(servers[0].name, "March Hare");
}

#[tokio::test]
async fn listing_rejects_bad_hello() {
    let (_registry, _udp, listing_addr, _) = spawn_tracker().await;
    let mut stream = TcpStream::connect(listing_addr).await.unwrap();
    stream.write_all(b"NOPE\x00\x01").await.unwrap();
    // The tracker closes without sending a listing.
    let mut out = Vec::new();
    stream.read_to_end(&mut out).await.unwrap();
    assert!(out.is_empty());
}

#[tokio::test]
async fn status_query_lists_servers_line_per_entry() {
    let (registry, udp_addr, _, status_addr) = spawn_tracker().await;
    send_registration(
        udp_addr,
        &htrk::Registration {
            port: 4653,
            users_online: 2,
            pass_id: [0; 4],
            name: "Cheshire".into(),
            description: "We're all mad here".into(),
        },
    )
    .await;
    wait_for_len(&registry, 1).await;

    let mut stream = TcpStream::connect(status_addr).await.unwrap();
    stream.write_all(b"LIST\n").await.unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).await.unwrap();
    assert_eq!(out, "Cheshire\t127.0.0.1:4653\t2\tWe're all mad here\t-\n");

    // Unknown commands get a one-line error.
    let mut stream = TcpStream::connect(status_addr).await.unwrap();
    stream.write_all(b"FROLIC\n").await.unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).await.unwrap();
    assert_eq!(out, "ERR unknown command\n");
}
