//! End-to-end directory-index round-trips: register servers (classic UDP
//! heartbeat and signed descriptor), then read the `INDEX` and `HEALTH`
//! status verbs back over TCP — all on ephemeral `127.0.0.1` ports, with no
//! sleeps beyond the usual bounded readiness polls.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use looking_glass::descriptor::Descriptor;
use looking_glass::{htrk, service, Registry, DEFAULT_TTL};
use rabbithole_identity::IdentityKey;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

/// Spawns the registration + status listeners on ephemeral ports; returns
/// (registry, udp registration addr, tcp status addr).
async fn spawn_tracker() -> (Arc<Registry>, SocketAddr, SocketAddr) {
    let registry = Arc::new(Registry::new(DEFAULT_TTL));
    let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let status = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let udp_addr = udp.local_addr().unwrap();
    let status_addr = status.local_addr().unwrap();
    tokio::spawn(service::run_registration_udp(udp, registry.clone()));
    tokio::spawn(service::run_status_tcp(status, registry.clone()));
    (registry, udp_addr, status_addr)
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

#[tokio::test]
async fn index_and_health_round_trip_over_the_status_port() {
    let (registry, udp_addr, status_addr) = spawn_tracker().await;

    // One classic unsigned heartbeat over UDP…
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let reg = htrk::Registration {
        port: 4653,
        users_online: 2,
        pass_id: [0; 4],
        name: "Cheshire".into(),
        description: "We're all mad here".into(),
    };
    sock.send_to(&reg.encode(), udp_addr).await.unwrap();
    wait_for_len(&registry, 1).await;

    // …and one signed descriptor (registered directly; the gossip round
    // trip has its own integration test).
    let key = IdentityKey::from_seed(&[7u8; 32]);
    let signed = Descriptor::new("Wonderland", ([127, 0, 0, 1], 5500).into())
        .with_description("Down the rabbit hole")
        .with_category("chat")
        .with_users(12)
        .with_timestamp(1_700_000_000_000)
        .sign(&key)
        .unwrap();
    registry.register_descriptor(signed, None).unwrap();

    // INDEX: signed row first, nine tab-separated columns per line.
    let index = status_query(status_addr, "INDEX\n").await;
    let lines: Vec<&str> = index.lines().collect();
    assert_eq!(lines.len(), 2);
    let signed_cols: Vec<&str> = lines[0].split('\t').collect();
    assert_eq!(
        &signed_cols[..4],
        ["Wonderland", "127.0.0.1:5500", "12", "chat"]
    );
    assert_eq!(signed_cols[4], "100.0", "fresh server reads full uptime");
    assert!(signed_cols[5].parse::<u64>().unwrap() < 60);
    assert_eq!(signed_cols[6], "yes");
    assert_eq!(signed_cols[7].len(), 16, "8-byte key prefix in hex");
    assert!(signed_cols[7].chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(signed_cols[8], "1700000000000", "verifiable generation");
    let unsigned_cols: Vec<&str> = lines[1].split('\t').collect();
    assert_eq!(
        &unsigned_cols[..4],
        ["Cheshire", "127.0.0.1:4653", "2", "-"]
    );
    assert_eq!(&unsigned_cols[6..], ["no", "-", "-"]);

    // INDEX cat= filters like LIST cat=.
    let chat = status_query(status_addr, "INDEX cat=CHAT\n").await;
    assert_eq!(chat.lines().count(), 1);
    assert!(chat.starts_with("Wonderland\t"));
    assert_eq!(status_query(status_addr, "INDEX cat=nope\n").await, "");

    // HEALTH: detail line plus a sparkline (one fresh bucket → "#").
    let health = status_query(status_addr, "HEALTH 127.0.0.1:4653\n").await;
    let health_lines: Vec<&str> = health.lines().collect();
    assert_eq!(health_lines.len(), 2);
    assert!(health_lines[0].starts_with("127.0.0.1:4653\tlive=yes\tuptime_24h=100.0\t"));
    assert!(health_lines[0].ends_with("flaps=0"));
    assert_eq!(health_lines[1], "#");

    // Malformed or unknown queries get one-line errors, never a hang.
    assert_eq!(
        status_query(status_addr, "HEALTH mad.hatter\n").await,
        "ERR bad address\n"
    );
    assert_eq!(
        status_query(status_addr, "HEALTH 127.0.0.1:9\n").await,
        "ERR unknown server\n"
    );
    assert_eq!(
        status_query(status_addr, "HEALTH\n").await,
        "ERR unknown command\n"
    );
}
