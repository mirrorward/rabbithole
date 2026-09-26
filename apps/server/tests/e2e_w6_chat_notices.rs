//! RH-22: real telnet sessions receive operator notices and kicks while
//! idle or editing a line. Native requests and ordered output markers keep
//! the assertions independent of arbitrary sleeps.

use std::time::Duration;

use burrow::Burrow;
use rabbithole_core::Client;
use rabbithole_proto::admin::{Broadcast, Kick};
use rabbithole_proto::chat::{RoomCreate, RoomInfoReply, RoomInvite, RoomJoin, RoomKick};
use rabbithole_server_core::{Role, ServerConfig, ServerEvent, LOBBY};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

async fn start(dir: &std::path::Path) -> Burrow {
    let burrow = Burrow::start(ServerConfig {
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        telnet_enabled: true,
        telnet_addr: "127.0.0.1:0".parse().unwrap(),
        keywords: [("den".into(), "room:den".into())].into(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    })
    .await
    .unwrap();
    for (name, role) in [
        ("operator", Role::Superuser),
        ("alice", Role::User),
        ("bob", Role::User),
    ] {
        burrow
            .shared
            .auth
            .create_account(name, "pw-pw-pw", role)
            .await
            .unwrap();
    }
    burrow
}

async fn native(burrow: &Burrow, name: &str) -> Client {
    let mut client = Client::connect(
        &format!("ws://127.0.0.1:{}", burrow.ws_addr.port()),
        None,
        None,
        "e2e",
        "0",
    )
    .await
    .unwrap();
    client.auth_password(name, "pw-pw-pw").await.unwrap();
    client.expect_welcome().await.unwrap();
    client
}

struct Telnet {
    socket: TcpStream,
    received: Vec<u8>,
    consumed: usize,
}

impl Telnet {
    async fn login(burrow: &Burrow, name: &str) -> Self {
        let mut client = Self {
            socket: TcpStream::connect(burrow.telnet_addr.unwrap())
                .await
                .unwrap(),
            received: Vec::new(),
            consumed: 0,
        };
        client.expect("login: ").await;
        client.line(name).await;
        client.expect("password: ").await;
        client.line("pw-pw-pw").await;
        client.expect("Command: ").await;
        client
    }

    async fn bytes(&mut self, text: &str) {
        self.socket.write_all(text.as_bytes()).await.unwrap();
    }

    async fn line(&mut self, text: &str) {
        self.bytes(&format!("{text}\r\n")).await;
    }

    async fn expect(&mut self, text: &str) {
        let needle = text.as_bytes();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(index) = self.received[self.consumed..]
                    .windows(needle.len())
                    .position(|bytes| bytes == needle)
                {
                    self.consumed += index + needle.len();
                    return;
                }
                let mut chunk = [0; 4096];
                let count = self.socket.read(&mut chunk).await.unwrap();
                assert!(count > 0, "EOF waiting for {text:?}");
                self.received.extend_from_slice(&chunk[..count]);
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timeout waiting for {text:?}; unread: {:?}",
                String::from_utf8_lossy(&self.received[self.consumed..])
            )
        });
    }

    fn assert_absent_since(&self, start: usize, text: &str) {
        assert!(
            !self.received[start..self.consumed]
                .windows(text.len())
                .any(|bytes| bytes == text.as_bytes()),
            "unexpected {text:?}: {:?}",
            String::from_utf8_lossy(&self.received[start..self.consumed])
        );
    }

    async fn lobby(&mut self) {
        self.line("c").await;
        self.expect("--- Chat: lobby ---").await;
    }

    async fn closed(&mut self) {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut tail = Vec::new();
            self.socket.read_to_end(&mut tail).await.unwrap();
            self.received.extend_from_slice(&tail);
        })
        .await
        .expect("kicked session closes");
    }
}

#[tokio::test]
async fn native_broadcast_arrives_idle_and_preserves_a_partial_chat_line() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut operator = native(&burrow, "operator").await;
    let mut alice = Telnet::login(&burrow, "alice").await;
    alice.lobby().await;
    alice.expect("(no recent chat)\r\n").await;

    operator
        .request_ack(&Broadcast::new("Tea in five."))
        .await
        .unwrap();
    alice
        .expect("\r\n(Notice from operator: Tea in five.)\r\n")
        .await;

    alice.bytes("unfinished").await;
    alice.expect("unfinished").await; // the server has accepted partial input
    let before_notice = alice.consumed;
    operator
        .request_ack(&Broadcast::new("Plain\r\ntext\x1b[31m\x07"))
        .await
        .unwrap();
    alice
        .expect("\r\n(Notice from operator: Plaintext[31m)\r\nunfinished")
        .await;
    alice.assert_absent_since(before_notice, "\x1b");
    alice.assert_absent_since(before_notice, "\x07");
    alice.line(" message").await;
    alice.expect("<alice> unfinished message\r\n").await;
    alice.line("/q").await;
    alice.expect("Command: ").await;
    alice.line("q").await;
    alice.expect("Goodbye, alice!\r\n").await;
    burrow.shutdown().await;
}

#[tokio::test]
async fn room_kicks_restore_the_menu_and_exclude_later_private_chat() {
    for partial in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let burrow = start(dir.path()).await;
        let mut operator = native(&burrow, "operator").await;
        let mut bob = native(&burrow, "bob").await;
        let _: RoomInfoReply = operator
            .request(&RoomCreate::new("den", true))
            .await
            .unwrap();
        let _: RoomInfoReply = operator
            .request(&RoomCreate::new("other", false))
            .await
            .unwrap();
        let mut alice = Telnet::login(&burrow, "alice").await;
        for name in ["alice", "bob"] {
            operator
                .request_ack(&RoomInvite::new("den", name))
                .await
                .unwrap();
        }
        let _: RoomInfoReply = bob.request(&RoomJoin::new("den")).await.unwrap();
        alice.line("/go den").await;
        alice.expect("--- Chat: den ---").await;
        alice.expect("(no recent chat)\r\n").await;

        // Neither another account's room kick nor a kick from another room
        // should evict the person from the chat screen they are using.
        operator
            .request_ack(&RoomKick::new("den", "bob", false))
            .await
            .unwrap();
        operator
            .request_ack(&RoomKick::new("other", "alice", false))
            .await
            .unwrap();
        operator.chat_send("den", "still in the den").await.unwrap();
        alice.expect("<operator> still in the den\r\n").await;
        if partial {
            alice.bytes("abandoned chat").await;
            alice.expect("abandoned chat").await;
        }
        let before_kick = alice.consumed;
        operator
            .request_ack(&RoomKick::new("den", "alice", partial))
            .await
            .unwrap();
        let action = if partial { "banned" } else { "removed" };
        alice
            .expect(&format!("\r\n(You were {action} from den.)\r\n"))
            .await;
        alice.expect("Command: ").await;

        // A new menu command works immediately: the unfinished chat line
        // must not prefix it. Later room chat must not reach this session.
        alice.lobby().await;
        operator
            .chat_send("den", "private after kick")
            .await
            .unwrap();
        operator.chat_send(LOBBY, "lobby barrier").await.unwrap();
        alice.expect("<operator> lobby barrier\r\n").await;
        alice.assert_absent_since(before_kick, "private after kick");
        alice.line("/q").await;
        alice.expect("Command: ").await;
        alice.line("q").await;
        alice.expect("Goodbye, alice!\r\n").await;
        burrow.shutdown().await;
    }
}

#[tokio::test]
async fn lobby_room_kick_allows_reentry_but_a_ban_keeps_the_menu_usable() {
    let dir = tempfile::tempdir().unwrap();
    let burrow = start(dir.path()).await;
    let mut operator = native(&burrow, "operator").await;
    let mut alice = Telnet::login(&burrow, "alice").await;
    alice.lobby().await;
    alice.expect("(no recent chat)\r\n").await;
    operator
        .request_ack(&RoomKick::new(LOBBY, "alice", false))
        .await
        .unwrap();
    alice.expect("(You were removed from lobby.)\r\n").await;
    alice.expect("Command: ").await;
    alice.lobby().await;
    alice.line("back in the lobby").await;
    alice.expect("<alice> back in the lobby\r\n").await;

    operator
        .request_ack(&RoomKick::new(LOBBY, "alice", true))
        .await
        .unwrap();
    alice.expect("(You were banned from lobby.)\r\n").await;
    alice.expect("Command: ").await;
    let before_reentry = alice.consumed;
    alice.line("c").await;
    alice.expect("Cannot join lobby: ").await;
    alice.expect("Command: ").await;
    alice.assert_absent_since(before_reentry, "--- Chat: lobby ---");
    alice.line("q").await;
    alice.expect("Goodbye, alice!\r\n").await;
    burrow.shutdown().await;
}

#[tokio::test]
async fn session_kicks_close_only_the_target_and_show_the_reason() {
    for partial in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let burrow = start(dir.path()).await;
        let mut operator = native(&burrow, "operator").await;
        let mut alice = Telnet::login(&burrow, "alice").await;
        alice.lobby().await;
        alice.expect("(no recent chat)\r\n").await;
        let mut bob = Telnet::login(&burrow, "bob").await;
        bob.lobby().await;
        bob.expect("(no recent chat)\r\n").await;
        let who = operator.who().await.unwrap();
        let id = |name: &str| {
            who.iter()
                .find(|person| person.screen_name == name)
                .unwrap()
                .session_id
        };

        operator.request_ack(&Kick::new(id("bob"))).await.unwrap();
        bob.expect("\r\nDisconnected by operator: kicked\r\n").await;
        bob.closed().await;
        operator
            .chat_send(LOBBY, "other session remains")
            .await
            .unwrap();
        alice.expect("<operator> other session remains\r\n").await;

        if partial {
            alice.bytes("unfinished").await;
            alice.expect("unfinished").await;
            // The native Kick request uses the fixed reason "kicked".
            // Other producers supply reasons on the same targeted bus event.
            burrow.shared.bus.publish(ServerEvent::Kick {
                session_id: id("alice"),
                reason: "Policy\r\nreason\x1b[0m\x07".into(),
            });
            let before_notice = alice.consumed;
            alice
                .expect("\r\nDisconnected by operator: Policyreason[0m\r\n")
                .await;
            alice.assert_absent_since(before_notice, "\x1b");
            alice.assert_absent_since(before_notice, "\x07");
        } else {
            operator.request_ack(&Kick::new(id("alice"))).await.unwrap();
            alice
                .expect("\r\nDisconnected by operator: kicked\r\n")
                .await;
        }
        alice.closed().await;
        assert!(!burrow
            .shared
            .presence
            .snapshot()
            .iter()
            .any(|person| { person.screen_name == "alice" || person.screen_name == "bob" }));
        burrow.shutdown().await;
    }
}
