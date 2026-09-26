//! RH-21: live mail on real telnet screens, driven by native senders and
//! protocol markers. Notifications never count as reading the conversation.

use std::time::Duration;

use burrow::Burrow;
use rabbithole_core::{Client, ClientError};
use rabbithole_legacy_telnet::proto::{escape_iac, opt, Event, Parser, DO, IAC, SB, SE, WILL};
use rabbithole_proto::dm::{DmSend, EncryptedPayload};
use rabbithole_proto::presence::PresenceState;
use rabbithole_proto::ErrorCode;
use rabbithole_server_core::{Role, ServerConfig};
use rabbithole_store_server::repo3::DmsRepo;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const PASS: &str = "mail-notice-password";

async fn start(path: &std::path::Path) -> Burrow {
    let b = Burrow::start(ServerConfig {
        data_dir: path.to_owned(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        telnet_enabled: true,
        telnet_addr: "127.0.0.1:0".parse().unwrap(),
        ratelimit_enabled: false,
        ..ServerConfig::default()
    })
    .await
    .unwrap();
    for name in ["alice", "bob", "carol"] {
        b.shared
            .auth
            .create_account(name, PASS, Role::User)
            .await
            .unwrap();
    }
    b
}

async fn native(b: &Burrow, name: &str) -> Client {
    let mut c = Client::connect(&format!("ws://{}", b.ws_addr), None, None, "dm-test", "0")
        .await
        .unwrap();
    c.auth_password(name, PASS).await.unwrap();
    c.expect_welcome().await.unwrap();
    c
}

struct Telnet {
    socket: TcpStream,
    parser: Parser,
    data: Vec<u8>,
    wire: Vec<u8>,
    pos: usize,
}

impl Telnet {
    async fn connect(b: &Burrow, terminal: &str) -> Self {
        let mut c = Self {
            socket: TcpStream::connect(b.telnet_addr.unwrap()).await.unwrap(),
            parser: Parser::new(),
            data: Vec::new(),
            wire: Vec::new(),
            pos: 0,
        };
        let mut options = vec![
            IAC,
            DO,
            opt::ECHO,
            IAC,
            WILL,
            opt::TTYPE,
            IAC,
            SB,
            opt::TTYPE,
            0,
        ];
        options.extend(terminal.as_bytes());
        options.extend([IAC, SE]);
        c.socket.write_all(&options).await.unwrap();
        c
    }

    async fn more(&mut self) {
        let mut buf = [0; 4096];
        let n = tokio::time::timeout(Duration::from_secs(10), self.socket.read(&mut buf))
            .await
            .expect("telnet output bounded")
            .unwrap();
        assert!(
            n > 0,
            "unexpected EOF: {:?}",
            String::from_utf8_lossy(&self.data[self.pos..])
        );
        self.wire.extend_from_slice(&buf[..n]);
        let mut events = Vec::new();
        self.parser.feed(&buf[..n], &mut events);
        for event in events {
            if let Event::Data(data) = event {
                self.data.extend(data);
            }
        }
    }

    async fn expect(&mut self, needle: &[u8]) {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(at) = self.data[self.pos..]
                    .windows(needle.len())
                    .position(|w| w == needle)
                {
                    self.pos += at + needle.len();
                    break;
                }
                self.more().await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "missing {:?}: {:?}",
                String::from_utf8_lossy(needle),
                String::from_utf8_lossy(&self.data[self.pos..])
            )
        });
    }

    async fn raw(&mut self, bytes: &[u8]) {
        self.socket.write_all(&escape_iac(bytes)).await.unwrap();
    }

    async fn line(&mut self, line: &str) {
        self.raw(format!("{line}\r\n").as_bytes()).await;
    }

    async fn login(&mut self) {
        self.expect(b"login: ").await;
        self.line("alice").await;
        self.expect(b"password: ").await;
        self.line(PASS).await;
        self.expect(b"Command: ").await;
    }

    async fn notice(&mut self, name: &str, prompt: &[u8]) {
        self.expect(
            format!("(New direct mail from {name}. At the main menu, type D {name}.)\r\n")
                .as_bytes(),
        )
        .await;
        if !prompt.is_empty() {
            self.expect(prompt).await;
        }
    }
}

#[tokio::test]
async fn mail_preserves_partial_command_and_only_opening_the_thread_marks_read() {
    let dir = tempfile::tempdir().unwrap();
    let b = start(dir.path()).await;
    let mut bob = native(&b, "bob").await;
    let mut alice = Telnet::connect(&b, "XTERM").await;
    alice.login().await;
    alice.raw(b"D b").await;
    alice.expect(b"D b").await; // echo proves the partial command was consumed
    let sent = bob
        .dm_send(&DmSend::new("alice", "private-body-only-after-open"))
        .await
        .unwrap();
    alice.notice("bob", b"Command: D b").await;
    assert!(
        !String::from_utf8_lossy(&alice.data[..alice.pos]).contains("private-body-only-after-open")
    );
    let account = b
        .shared
        .auth
        .login_password("alice", PASS, None)
        .await
        .unwrap()
        .account
        .id;
    assert!(DmsRepo(&b.shared.pool)
        .unread_for(account)
        .await
        .unwrap()
        .iter()
        .any(|m| m.id == sent.id));

    // Follow the exact route printed by the notification; typed input survived.
    alice.line("ob").await;
    alice.expect(b"private-body-only-after-open").await;
    alice.expect(b"dm bob> ").await;
    assert!(DmsRepo(&b.shared.pool)
        .unread_for(account)
        .await
        .unwrap()
        .is_empty());
    bob.dm_send(&DmSend::new("alice", "second-private-body"))
        .await
        .unwrap();
    alice.notice("bob", b"dm bob> ").await;
    assert!(!String::from_utf8_lossy(&alice.data[..alice.pos]).contains("second-private-body"));
    alice.line("ls").await;
    alice.expect(b"second-private-body").await;
    alice.expect(b"dm bob> ").await;
    alice.line("q").await;
    alice.expect(b"Command: ").await;
    alice.line("q").await;
    alice.expect(b"Goodbye, alice!").await;
    b.shutdown().await;
}

#[tokio::test]
async fn notices_reach_boards_compose_files_pager_and_chat_without_duplicate_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let b = start(dir.path()).await;
    b.shared
        .boards
        .create_board("general", "General", "", 2, None, 0)
        .await
        .unwrap();
    for n in 0..20 {
        b.shared
            .files
            .create_area(&format!("area{n:02}"), "Area", "")
            .await
            .unwrap();
    }
    let mut bob = native(&b, "bob").await;
    let mut alice = Telnet::connect(&b, "XTERM").await;
    alice.login().await;
    for (command, prompt) in [
        ("b", "boards> "),
        ("general", "board general> "),
        ("n", "Subject: "),
    ] {
        alice.line(command).await;
        alice.expect(prompt.as_bytes()).await;
        bob.dm_send(&DmSend::new("alice", "screen-private-body"))
            .await
            .unwrap();
        alice.notice("bob", prompt.as_bytes()).await;
    }
    alice.line("My subject").await;
    alice.expect(b"(`/abort` cancels).\r\n").await;
    alice.raw(b"an unfinished body").await;
    alice.expect(b"an unfinished body").await;
    bob.dm_send(&DmSend::new("alice", "compose-private-body"))
        .await
        .unwrap();
    alice.notice("bob", b"an unfinished body").await;
    alice.line(" survives").await;
    alice.line(".").await;
    alice.expect(b"Posted.").await;
    alice.expect(b"board general> ").await;
    alice.line("1").await;
    alice.expect(b"an unfinished body survives").await;
    alice.expect(b"thread> ").await;
    alice.line("q").await;
    alice.expect(b"board general> ").await;
    alice.line("q").await;
    alice.expect(b"boards> ").await;
    alice.line("q").await;
    alice.expect(b"Command: ").await;

    alice.line("f").await;
    alice.expect(b") [Enter continues, q stops] -- ").await;
    bob.dm_send(&DmSend::new("alice", "pager-private-body"))
        .await
        .unwrap();
    alice
        .notice("bob", b") [Enter continues, q stops] -- ")
        .await;
    alice.line("q").await;
    alice.expect(b"files /> ").await;
    bob.dm_send(&DmSend::new("alice", "files-private-body"))
        .await
        .unwrap();
    alice.notice("bob", b"files /> ").await;
    alice.line("q").await;
    alice.expect(b"Command: ").await;
    alice.line("c").await;
    alice.expect(b"Type to talk.").await;
    bob.dm_send(&DmSend::new("alice", "chat-private-body"))
        .await
        .unwrap();
    alice.notice("bob", b"").await;
    // A later room event proves that chat's separate fresh subscription did
    // not duplicate the DM or stop ordinary chat delivery.
    bob.chat_send("lobby", "after-mail-marker").await.unwrap();
    alice.expect(b"<bob> after-mail-marker").await;
    let shown = String::from_utf8_lossy(&alice.data[..alice.pos]);
    assert_eq!(shown.matches("New direct mail from bob.").count(), 7);
    assert!(!shown.contains("private-body"));
    alice.line("/q").await;
    alice.expect(b"Command: ").await;
    alice.line("q").await;
    alice.expect(b"Goodbye, alice!").await;
    b.shutdown().await;
}

#[tokio::test]
async fn notices_are_authenticated_recipient_only_and_preserve_dm_privacy() {
    let dir = tempfile::tempdir().unwrap();
    let b = start(dir.path()).await;
    let mut bob = native(&b, "bob").await;
    let mut carol = native(&b, "carol").await;
    let mut alice = Telnet::connect(&b, "XTERM").await;
    alice.expect(b"login: ").await;
    bob.dm_send(&DmSend::new("alice", "before-login-secret"))
        .await
        .unwrap();
    alice.line("alice").await;
    alice.expect(b"password: ").await;
    alice.line(PASS).await;
    alice.expect(b"Command: ").await;
    bob.dm_send(&DmSend::new("carol", "different-recipient-secret"))
        .await
        .unwrap();
    carol
        .dm_send(&DmSend::new("alice", "ordered-sentinel-secret"))
        .await
        .unwrap();
    alice.notice("carol", b"Command: ").await;
    assert!(
        !String::from_utf8_lossy(&alice.data[..alice.pos]).contains("New direct mail from bob.")
    );

    bob.block_add("alice").await.unwrap();
    assert!(matches!(
        bob.dm_send(&DmSend::new("alice", "blocked-secret")).await,
        Err(ClientError::Refused(ErrorCode::Forbidden))
    ));
    carol
        .dm_send(&DmSend::new("alice", "after-block-sentinel"))
        .await
        .unwrap();
    alice.notice("carol", b"Command: ").await;
    assert!(
        !String::from_utf8_lossy(&alice.data[..alice.pos]).contains("New direct mail from bob.")
    );
    bob.block_remove("alice").await.unwrap();
    bob.presence_set(
        PresenceState::Invisible,
        Some("invisible-status-secret".into()),
    )
    .await
    .unwrap();
    bob.dm_send(&DmSend::new("alice", "intentional-invisible-secret"))
        .await
        .unwrap();
    alice.notice("bob", b"Command: ").await;
    bob.dm_send_encrypted(
        "alice",
        EncryptedPayload::new(None, vec![1], b"ciphertext-secret".to_vec()),
    )
    .await
    .unwrap();
    alice.notice("bob", b"Command: ").await;
    let shown = String::from_utf8_lossy(&alice.data[..alice.pos]);
    assert!(!shown.contains("secret"));
    assert!(!shown.contains("bob is online"));
    assert_eq!(shown.matches("New direct mail from bob.").count(), 2);
    alice.line("q").await;
    alice.expect(b"Goodbye, alice!").await;
    b.shutdown().await;
}

#[tokio::test]
async fn utf8_prefix_and_cp437_partial_input_survive_notifications() {
    for terminal in ["XTERM", "ANSI"] {
        let dir = tempfile::tempdir().unwrap();
        let b = start(dir.path()).await;
        let mut bob = native(&b, "bob").await;
        let mut alice = Telnet::connect(&b, terminal).await;
        alice.login().await;
        if terminal == "XTERM" {
            alice.raw(b"caf\xc3").await;
            // An unsupported-option reply is an ordered protocol fence:
            // the preceding incomplete UTF-8 prefix has reached the reader.
            let start = alice.wire.len();
            alice.socket.write_all(&[IAC, DO, 42]).await.unwrap();
            while !alice.wire[start..].windows(3).any(|w| w == [IAC, 252, 42]) {
                alice.more().await;
            }
            bob.dm_send(&DmSend::new("alice", "split-secret"))
                .await
                .unwrap();
            alice.notice("bob", b"Command: caf").await;
            assert!(!String::from_utf8_lossy(&alice.data[..alice.pos]).contains('\u{fffd}'));
            alice.raw(b"\xa9\r\n").await;
            alice.expect("Unknown command: café".as_bytes()).await;
        } else {
            alice.raw(b"caf\x82").await;
            alice.expect(b"caf\x82").await;
            bob.dm_send(&DmSend::new("alice", "cp437-secret"))
                .await
                .unwrap();
            alice.notice("bob", b"Command: caf\x82").await;
            alice.raw(b"\x08e\r\n").await;
            alice.expect(b"Unknown command: cafe").await;
        }
        alice.expect(b"Command: ").await;
        alice.line("q").await;
        alice.expect(b"Goodbye, alice!").await;
        b.shutdown().await;
    }
}

#[tokio::test]
async fn control_names_and_lossy_cp437_names_offer_the_conversation_list_route() {
    for (terminal, name, displayed) in [
        ("XTERM", "bad\x1b[2Jname", "bad[2Jname"),
        ("ANSI", "snow☃", "snow?"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let b = start(dir.path()).await;
        let mut bob = native(&b, "bob").await;
        let persona = bob.persona_create(name).await.unwrap();
        bob.persona_switch(persona.persona.id).await.unwrap();
        let mut alice = Telnet::connect(&b, terminal).await;
        alice.login().await;
        let start = alice.pos;
        bob.dm_send(&DmSend::new("alice", "do-not-display-body"))
            .await
            .unwrap();
        alice.expect(format!("(New direct mail from {displayed}. At the main menu, type D and choose the conversation.)\r\nCommand: ").as_bytes()).await;
        let shown = &alice.data[start..alice.pos];
        assert!(!shown.contains(&0x1b));
        assert!(!String::from_utf8_lossy(shown).contains("do-not-display-body"));
        alice.line("q").await;
        alice.expect(b"Goodbye, alice!").await;
        b.shutdown().await;
    }
}

#[tokio::test]
async fn zmodem_defers_mail_until_text_mode_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let b = start(dir.path()).await;
    b.shared
        .files
        .create_area("stuff", "Stuff", "")
        .await
        .unwrap();
    let bytes = b"a file for a deliberately cancelled transfer";
    let blob = b.shared.blobs.put(bytes).unwrap().0;
    b.shared
        .files
        .add_file(
            "stuff",
            None,
            "sample.txt",
            &blob,
            bytes.len() as i64,
            "text/plain",
            "",
            "",
            "test",
            1,
        )
        .await
        .unwrap();
    let mut bob = native(&b, "bob").await;
    let mut alice = Telnet::connect(&b, "XTERM").await;
    alice.login().await;
    alice.line("f").await;
    alice.expect(b"files /> ").await;
    alice.line("cd stuff").await;
    alice.expect(b"files /stuff> ").await;
    alice.line("zget sample.txt").await;
    alice.expect(b"**\x18B").await; // actual ZRQINIT, now in binary receive wait
    let binary_start = alice.pos;
    bob.dm_send(&DmSend::new("alice", "transfer-secret"))
        .await
        .unwrap();
    alice.raw(&[0x18; 8]).await;
    alice.expect(b"Transfer cancelled.").await;
    assert!(
        !String::from_utf8_lossy(&alice.data[binary_start..alice.pos]).contains("New direct mail")
    );
    alice.expect(b"files /stuff> ").await;
    alice.notice("bob", b"files /stuff> ").await;
    alice.line("q").await;
    alice.expect(b"Command: ").await;
    alice.line("q").await;
    alice.expect(b"Goodbye, alice!").await;
    b.shutdown().await;
}
