//! End-to-end shell test over a real TCP socket: a scripted client
//! negotiates options, logs in against a stub authenticator, sees the main
//! menu, and quits. The client side reuses the crate's own [`Parser`] to
//! split server output into negotiation commands vs. text.

use std::collections::VecDeque;

use rabbithole_legacy_telnet::proto::{DO, IAC, SB, SE, TTYPE_IS, WILL};
use rabbithole_legacy_telnet::{opt, run_shell, Event, Parser, ShellOptions, TelnetAuth};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

struct StubAuth;

#[async_trait::async_trait]
impl TelnetAuth for StubAuth {
    async fn login(&self, username: &str, password: &str) -> Option<String> {
        (username == "kevin" && password == "carrots").then(|| "White Rabbit".to_string())
    }
}

/// The scripted client: parses server bytes, collecting text and commands.
struct Client {
    stream: TcpStream,
    parser: Parser,
    events: VecDeque<Event>,
    text: String,
}

impl Client {
    async fn connect(addr: std::net::SocketAddr) -> Client {
        Client {
            stream: TcpStream::connect(addr).await.unwrap(),
            parser: Parser::new(),
            events: VecDeque::new(),
            text: String::new(),
        }
    }

    async fn send(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).await.unwrap();
    }

    /// Read until `needle` appears in the accumulated text (data bytes
    /// only); returns all negotiation/command events seen on the way.
    async fn read_until(&mut self, needle: &str) -> Vec<Event> {
        let mut seen = Vec::new();
        loop {
            while let Some(ev) = self.events.pop_front() {
                match ev {
                    Event::Data(d) => self.text.push_str(&String::from_utf8_lossy(&d)),
                    other => seen.push(other),
                }
            }
            if self.text.contains(needle) {
                return seen;
            }
            let mut buf = [0u8; 4096];
            let n = self.stream.read(&mut buf).await.unwrap();
            assert!(n > 0, "server closed before {needle:?}; got: {}", self.text);
            let mut evs = Vec::new();
            self.parser.feed(&buf[..n], &mut evs);
            self.events.extend(evs);
        }
    }

    /// Read until an event matching `pred` arrives (data keeps accruing to
    /// `self.text` on the way).
    async fn read_until_event(&mut self, pred: impl Fn(&Event) -> bool) {
        loop {
            while let Some(ev) = self.events.pop_front() {
                match ev {
                    Event::Data(d) => self.text.push_str(&String::from_utf8_lossy(&d)),
                    other if pred(&other) => return,
                    _ => {}
                }
            }
            let mut buf = [0u8; 4096];
            let n = self.stream.read(&mut buf).await.unwrap();
            assert!(n > 0, "server closed while waiting for an event");
            let mut evs = Vec::new();
            self.parser.feed(&buf[..n], &mut evs);
            self.events.extend(evs);
        }
    }

    /// Read until the server closes the connection; returns remaining text.
    async fn read_to_eof(&mut self) -> String {
        loop {
            let mut buf = [0u8; 4096];
            let n = self.stream.read(&mut buf).await.unwrap();
            if n == 0 {
                while let Some(ev) = self.events.pop_front() {
                    if let Event::Data(d) = ev {
                        self.text.push_str(&String::from_utf8_lossy(&d));
                    }
                }
                return std::mem::take(&mut self.text);
            }
            let mut evs = Vec::new();
            self.parser.feed(&buf[..n], &mut evs);
            for ev in evs {
                match ev {
                    Event::Data(d) => self.text.push_str(&String::from_utf8_lossy(&d)),
                    _ => self.events.push_back(ev),
                }
            }
        }
    }
}

/// Bind a listener and serve exactly one shell session on it.
async fn spawn_server() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        // BrokenPipe/reset from an abruptly-closing client is fine here.
        let _ = run_shell(socket, &StubAuth, &ShellOptions::default()).await;
    });
    (addr, handle)
}

#[tokio::test]
async fn full_session_negotiate_login_menu_quit() {
    let (addr, server) = spawn_server().await;
    let mut c = Client::connect(addr).await;

    // Banner arrives alongside the server's option offers.
    let events = c.read_until("RabbitHole BBS").await;
    assert!(events.contains(&Event::Will(opt::ECHO)), "{events:?}");
    assert!(events.contains(&Event::Will(opt::SGA)), "{events:?}");
    assert!(events.contains(&Event::Do(opt::NAWS)), "{events:?}");
    assert!(events.contains(&Event::Do(opt::TTYPE)), "{events:?}");

    // Accept ECHO/SGA, agree to NAWS + TTYPE, and report 80x24.
    c.send(&[
        IAC,
        DO,
        opt::ECHO,
        IAC,
        DO,
        opt::SGA,
        IAC,
        WILL,
        opt::SGA,
        IAC,
        WILL,
        opt::NAWS,
        IAC,
        WILL,
        opt::TTYPE,
    ])
    .await;
    c.send(&[IAC, SB, opt::NAWS, 0, 80, 0, 24, IAC, SE]).await;

    // The server must ask for our terminal type; answer "ANSI".
    c.read_until("login: ").await;
    c.read_until_event(
        |e| matches!(e, Event::Subnegotiation(o, p) if *o == opt::TTYPE && p == &[1u8]),
    )
    .await;
    let mut is = vec![IAC, SB, opt::TTYPE, TTYPE_IS];
    is.extend(b"ANSI");
    is.extend([IAC, SE]);
    c.send(&is).await;

    // Log in (username echoed by the server, password not).
    c.send(b"kevin\r\n").await;
    c.read_until("password: ").await;
    c.send(b"carrots\r\n").await;

    // Greeting includes the negotiated terminal facts, then the menu.
    c.read_until("Welcome, White Rabbit!").await;
    c.read_until("[ANSI, 80x24]").await;
    c.read_until("MAIN MENU").await;
    c.read_until("Command: ").await;
    assert!(!c.text.contains("carrots"), "password must not be echoed");

    // An unknown command re-prompts; then quit ends the session.
    c.send(b"xyzzy\r\n").await;
    c.read_until("Unknown command: xyzzy").await;
    c.read_until("Command: ").await;
    c.send(b"q\r\n").await;
    let rest = c.read_to_eof().await;
    assert!(rest.contains("Goodbye, White Rabbit!"), "{rest}");

    server.await.unwrap();
}

#[tokio::test]
async fn bad_password_then_success() {
    let (addr, server) = spawn_server().await;
    let mut c = Client::connect(addr).await;

    c.read_until("login: ").await;
    c.send(b"kevin\r\nwrong\r\n").await;
    c.read_until("Login incorrect.").await;
    c.read_until("login: ").await;
    c.send(b"kevin\r\ncarrots\r\n").await;
    c.read_until("Welcome, White Rabbit!").await;
    c.send(b"quit\r\n").await;
    let rest = c.read_to_eof().await;
    assert!(rest.contains("Goodbye"), "{rest}");

    server.await.unwrap();
}

#[tokio::test]
async fn lockout_after_max_attempts() {
    let (addr, server) = spawn_server().await;
    let mut c = Client::connect(addr).await;

    for _ in 0..3 {
        c.read_until("login: ").await;
        c.send(b"mallory\r\nguess\r\n").await;
    }
    let rest = c.read_to_eof().await;
    assert!(rest.contains("Too many failures."), "{rest}");

    server.await.unwrap();
}
