//! RH-167: persona labels are plain text on Telnet, while the original
//! identity still selects the conversation and receives replies.

use std::{collections::HashMap, time::Duration};

use burrow::Burrow;
use rabbithole_core::Client;
use rabbithole_legacy_telnet::{
    encoding::encode_into,
    proto::{escape_iac, opt, Event, Parser, DO, IAC, SB, SE, WILL},
    Encoding,
};
use rabbithole_proto::dm::DmSend;
use rabbithole_server_core::{Role, ServerConfig};
use rabbithole_store_server::{repo::AccountsRepo, repo2::PersonasRepo};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

const PASS: &str = "persona-label-password";
const RAW_NAME: &str = "bad\x1b[2J\r\ncafé";
const DISPLAY_NAME: &str = "bad[2Jcafé";

async fn start(path: &std::path::Path) -> Burrow {
    let b = Burrow::start(ServerConfig {
        data_dir: path.to_owned(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        telnet_enabled: true,
        telnet_addr: "127.0.0.1:0".parse().unwrap(),
        ratelimit_enabled: false,
        keywords: HashMap::from([("newmail".into(), format!("user:{RAW_NAME}"))]),
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
    let mut client = Client::connect(&format!("ws://{}", b.ws_addr), None, None, "labels", "0")
        .await
        .unwrap();
    client.auth_password(name, PASS).await.unwrap();
    client.expect_welcome().await.unwrap();
    client
}

struct Telnet {
    socket: TcpStream,
    parser: Parser,
    encoding: Encoding,
    data: Vec<u8>,
    pos: usize,
}

impl Telnet {
    async fn connect(b: &Burrow, encoding: Encoding) -> Self {
        let mut c = Self {
            socket: TcpStream::connect(b.telnet_addr.unwrap()).await.unwrap(),
            parser: Parser::new(),
            encoding,
            data: Vec::new(),
            pos: 0,
        };
        let terminal = match encoding {
            Encoding::Utf8 => "XTERM",
            Encoding::Cp437 => "ANSI",
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

    async fn expect(&mut self, text: &str) {
        let mut needle = Vec::new();
        encode_into(self.encoding, text, &mut needle);
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(at) = self.data[self.pos..]
                    .windows(needle.len())
                    .position(|window| window == needle)
                {
                    self.pos += at + needle.len();
                    return;
                }
                let mut buf = [0; 4096];
                let n = self.socket.read(&mut buf).await.unwrap();
                assert!(n > 0, "unexpected EOF looking for {text:?}");
                let mut events = Vec::new();
                self.parser.feed(&buf[..n], &mut events);
                for event in events {
                    if let Event::Data(data) = event {
                        self.data.extend(data);
                    }
                }
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "missing {text:?}: {:?}",
                String::from_utf8_lossy(&self.data[self.pos..])
            )
        });
    }

    async fn line(&mut self, line: &str) {
        let mut bytes = Vec::new();
        encode_into(self.encoding, &format!("{line}\r\n"), &mut bytes);
        self.socket.write_all(&escape_iac(&bytes)).await.unwrap();
    }

    async fn login(&mut self, login: &str) -> usize {
        self.expect("login: ").await;
        self.line(login).await;
        self.expect("password: ").await;
        let start = self.pos;
        self.line(PASS).await;
        start
    }

    fn assert_plain_since(&self, start: usize) {
        // Text screens have their own CRLFs; no other ASCII controls are
        // expected in these fixtures. Exact labels below also prove embedded
        // CR/LF was removed rather than rendered as an extra line.
        assert!(
            self.data[start..self.pos]
                .iter()
                .all(|b| !b.is_ascii_control() || matches!(b, b'\r' | b'\n')),
            "terminal control in persona labels: {:?}",
            &self.data[start..self.pos]
        );
    }
}

async fn native_persona_labels(encoding: Encoding) {
    let dir = tempfile::tempdir().unwrap();
    let b = start(dir.path()).await;
    let mut bob = native(&b, "bob").await;
    let persona = bob.persona_create(RAW_NAME).await.unwrap();
    assert_eq!(persona.persona.screen_name, RAW_NAME);
    bob.persona_switch(persona.persona.id).await.unwrap();

    let mut alice = Telnet::connect(&b, encoding).await;
    let start = alice.login("alice").await;
    alice.expect("Welcome, alice!").await;
    alice.expect("Online now (").await;
    alice.expect(DISPLAY_NAME).await;
    alice.expect("Command: ").await;
    alice.assert_plain_since(start);

    // The mapped keyword carries the original identity into the existing
    // empty-conversation path; its display is sanitized separately.
    alice.line("/go newmail").await;
    alice
        .expect(&format!("No messages with {DISPLAY_NAME} yet"))
        .await;
    alice.expect(&format!("dm {DISPLAY_NAME}> ")).await;
    alice.assert_plain_since(start);
    alice.line("q").await;
    alice.expect("Command: ").await;

    bob.dm_send(&DmSend::new("alice", "native-message-body"))
        .await
        .unwrap();
    alice
        .expect(&format!("New direct mail from {DISPLAY_NAME}."))
        .await;
    alice.expect("Command: ").await;
    alice.line("d").await;
    alice.expect("WITH").await;
    alice.expect(DISPLAY_NAME).await;
    alice.expect("native-message-body").await;
    alice.expect("mail> ").await;
    alice.assert_plain_since(start);

    // Selection must retain the raw persona: the sanitized name does not
    // exist in the store. Then reply to verify the resolved account too.
    alice.line("1").await;
    alice
        .expect(&format!("{DISPLAY_NAME}: native-message-body\r\n"))
        .await;
    alice.expect(&format!("dm {DISPLAY_NAME}> ")).await;
    alice.assert_plain_since(start);
    alice.line("r").await;
    alice.expect("Message: ").await;
    alice.line("reply-to-original-persona").await;
    alice.expect("Sent.").await;
    alice.expect(&format!("dm {DISPLAY_NAME}> ")).await;
    let history = bob.dm_history("alice", 0, 20).await.unwrap();
    assert!(history.iter().any(|m| {
        m.from == "alice" && m.to == RAW_NAME && m.text == "reply-to-original-persona"
    }));
    alice.line("q").await;
    alice.expect("mail> ").await;
    alice.line("q").await;
    alice.expect("Command: ").await;

    // Ordinary direct-name selection and labels retain their existing form.
    alice.line("d carol").await;
    alice.expect("No messages with carol yet").await;
    alice.expect("dm carol> ").await;
    alice.line("q").await;
    alice.expect("Command: ").await;
    alice.line("q").await;
    alice.expect("Goodbye, alice!").await;
    alice.assert_plain_since(start);
    b.shutdown().await;
}

#[tokio::test]
async fn native_persona_labels_are_plain_utf8_and_numbered_selection_keeps_identity() {
    native_persona_labels(Encoding::Utf8).await;
}

#[tokio::test]
async fn native_persona_labels_are_plain_cp437_and_numbered_selection_keeps_identity() {
    native_persona_labels(Encoding::Cp437).await;
}

async fn own_persona_labels(encoding: Encoding) {
    let dir = tempfile::tempdir().unwrap();
    let b = start(dir.path()).await;
    // PersonaCreate creates an additional persona, not a login default.
    // Provision this separate existing-account fixture through repository
    // APIs so its safe login can exercise a default persona containing controls.
    let accounts = AccountsRepo(&b.shared.pool);
    let template = accounts.by_login("alice").await.unwrap().unwrap();
    let account = accounts
        .create(
            "own",
            template.phc.as_deref(),
            RAW_NAME,
            template.role,
            template.class_id,
        )
        .await
        .unwrap();
    PersonasRepo(&b.shared.pool)
        .create(account.id, RAW_NAME, true)
        .await
        .unwrap();
    let mut own = Telnet::connect(&b, encoding).await;
    let start = own.login("own").await;
    own.expect(&format!("Welcome, {DISPLAY_NAME}!")).await;
    own.expect("Command: ").await;
    own.line("q").await;
    own.expect(&format!("Goodbye, {DISPLAY_NAME}!\r\n")).await;
    own.assert_plain_since(start);
    assert_eq!(
        PersonasRepo(&b.shared.pool)
            .default_for_account(account.id)
            .await
            .unwrap()
            .unwrap()
            .screen_name,
        RAW_NAME
    );
    b.shutdown().await;
}

#[tokio::test]
async fn own_greeting_and_goodbye_use_plain_utf8_labels() {
    own_persona_labels(Encoding::Utf8).await;
}

#[tokio::test]
async fn own_greeting_and_goodbye_use_plain_cp437_labels() {
    own_persona_labels(Encoding::Cp437).await;
}
