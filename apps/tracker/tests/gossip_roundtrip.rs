//! End-to-end signed-descriptor + gossip round-trips: announce a signed
//! descriptor over UDP, read it back through the status port (with category
//! filtering), and let two trackers converge over gossip — all on ephemeral
//! `127.0.0.1` ports.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use looking_glass::descriptor::Descriptor;
use looking_glass::gossip::GossipMessage;
use looking_glass::{service, Registry, SignedDescriptor, DEFAULT_TTL};
use rabbithole_identity::IdentityKey;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

/// Spawns a gossip listener on an ephemeral port; returns its address.
async fn spawn_gossip(
    registry: Arc<Registry>,
    peers: Vec<SocketAddr>,
    interval: Duration,
) -> SocketAddr {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    tokio::spawn(service::run_gossip_udp(socket, registry, peers, interval));
    addr
}

/// Spawns a status listener on an ephemeral port; returns its address.
async fn spawn_status(registry: Arc<Registry>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(service::run_status_tcp(listener, registry));
    addr
}

/// Sends one gossip datagram from an ephemeral UDP socket.
async fn send_message(to: SocketAddr, message: &GossipMessage) {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.send_to(&message.encode(), to).await.unwrap();
}

/// One status-port command; returns the full response.
async fn status_query(addr: SocketAddr, command: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream.write_all(command.as_bytes()).await.unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).await.unwrap();
    out
}

/// Waits (bounded) for the registry to reach `len` live entries.
async fn wait_for_len(registry: &Registry, len: usize) {
    for _ in 0..400 {
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

fn signed(seed: u8, name: &str, port: u16, categories: &[&str]) -> SignedDescriptor {
    let mut descriptor = Descriptor::new(name, ([127, 0, 0, 1], port).into())
        .with_description(format!("{name} desc"))
        .with_users(3)
        .with_software("rabbithole-server 0.36")
        .with_timestamp(1_700_000_000_000);
    for category in categories {
        descriptor = descriptor.with_category(*category);
    }
    descriptor
        .sign(&IdentityKey::from_seed(&[seed; 32]))
        .unwrap()
}

#[tokio::test]
async fn signed_announce_then_filtered_listing_and_categories() {
    let registry = Arc::new(Registry::new(DEFAULT_TTL));
    let gossip_addr = spawn_gossip(registry.clone(), Vec::new(), Duration::from_secs(60)).await;
    let status_addr = spawn_status(registry.clone()).await;

    // Garbage first: the gossip listener must shrug it off.
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.send_to(&[], gossip_addr).await.unwrap();
    sock.send_to(&[0xFF; 3], gossip_addr).await.unwrap();
    sock.send_to(b"RHGS\x01\xff\xff\xff\xff", gossip_addr)
        .await
        .unwrap();

    // A tampered announce is verified and rejected…
    let mut tampered = signed(1, "Wonderland", 5500, &["chat", "retro"]);
    tampered.descriptor.users_online = 999;
    send_message(gossip_addr, &GossipMessage::Announce(Box::new(tampered))).await;

    // …while genuine announces register.
    let wonderland = signed(1, "Wonderland", 5500, &["chat", "retro"]);
    let teaparty = signed(2, "Tea Party", 5510, &["warez"]);
    send_message(
        gossip_addr,
        &GossipMessage::Announce(Box::new(wonderland.clone())),
    )
    .await;
    send_message(gossip_addr, &GossipMessage::Announce(Box::new(teaparty))).await;
    wait_for_len(&registry, 2).await;

    let snap = registry.snapshot();
    assert_eq!(snap[1].name, "Wonderland");
    assert_eq!(snap[1].users_online, 3, "tampered announce never landed");
    assert_eq!(snap[1].server_key(), Some(wonderland.descriptor.server_key));
    assert_eq!(snap[1].via, None, "direct announce carries no via marker");

    // LIST shows the category column; LIST cat= filters; CATEGORIES counts.
    let all = status_query(status_addr, "LIST\n").await;
    assert_eq!(
        all,
        "Tea Party\t127.0.0.1:5510\t3\tTea Party desc\twarez\n\
         Wonderland\t127.0.0.1:5500\t3\tWonderland desc\tchat,retro\n"
    );
    let chat = status_query(status_addr, "LIST cat=CHAT\n").await;
    assert_eq!(
        chat,
        "Wonderland\t127.0.0.1:5500\t3\tWonderland desc\tchat,retro\n"
    );
    assert_eq!(status_query(status_addr, "LIST cat=nope\n").await, "");
    assert_eq!(
        status_query(status_addr, "CATEGORIES\n").await,
        "chat\t1\nretro\t1\nwarez\t1\n"
    );
}

#[tokio::test]
async fn two_trackers_converge_via_gossip() {
    // Tracker A holds a directly-announced signed entry; tracker B peers
    // with A and must learn it (push–pull: B's empty digest makes A push).
    let reg_a = Arc::new(Registry::new(DEFAULT_TTL));
    let reg_b = Arc::new(Registry::new(DEFAULT_TTL));
    let addr_a = spawn_gossip(reg_a.clone(), Vec::new(), Duration::from_secs(60)).await;

    let wonderland = signed(1, "Wonderland", 5500, &["chat"]);
    send_message(
        addr_a,
        &GossipMessage::Announce(Box::new(wonderland.clone())),
    )
    .await;
    wait_for_len(&reg_a, 1).await;

    let _addr_b = spawn_gossip(reg_b.clone(), vec![addr_a], Duration::from_millis(25)).await;
    wait_for_len(&reg_b, 1).await;

    let snap = reg_b.snapshot();
    assert_eq!(snap[0].name, "Wonderland");
    assert_eq!(snap[0].signed, Some(wonderland), "relayed verbatim");
    assert_eq!(
        snap[0].via,
        Some(addr_a),
        "gossiped entry is marked with the peer it came from"
    );
    // A never learned anything back (B had nothing new to offer).
    assert_eq!(reg_a.len(), 1);
    assert_eq!(reg_a.snapshot()[0].via, None);
}
