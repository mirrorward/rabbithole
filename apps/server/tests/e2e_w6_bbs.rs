//! Wave 6 end-to-end tests: the telnet shell as a real BBS — welcome screen
//! on login, message boards (read + post), live lobby chat bridged over the
//! shared bus, direct mail against the durable DM store, and `/go` keyword
//! teleports. All scripted sessions are marker-driven (no blind sleeps) and
//! close gracefully.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use burrow::Burrow;
use rabbithole_server_core::{Role, ServerConfig, LOBBY};
use rabbithole_store_server::repo3::DmsRepo;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn test_config(dir: &Path) -> ServerConfig {
    ServerConfig {
        name: "BBS Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        telnet_enabled: true,
        telnet_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

// ---------------------------------------------------------------------------
// A tiny deterministic telnet client: accumulate bytes, wait for markers.

struct Client {
    sock: TcpStream,
    buf: Vec<u8>,
    /// Bytes before `pos` were already consumed by an earlier `expect`.
    pos: usize,
}

impl Client {
    async fn connect(addr: std::net::SocketAddr) -> Client {
        Client {
            sock: TcpStream::connect(addr).await.unwrap(),
            buf: Vec::new(),
            pos: 0,
        }
    }

    async fn send(&mut self, line: &str) {
        self.sock
            .write_all(format!("{line}\r\n").as_bytes())
            .await
            .unwrap();
    }

    /// Read until `needle` appears past the consumption point, advancing
    /// the point past the match — no blind sleeps, no double-matching
    /// stale output.
    async fn expect(&mut self, needle: &[u8]) {
        let deadline = Duration::from_secs(30);
        tokio::time::timeout(deadline, async {
            loop {
                if let Some(at) = find(&self.buf[self.pos..], needle) {
                    self.pos += at + needle.len();
                    return;
                }
                let mut chunk = [0u8; 4096];
                let n = self.sock.read(&mut chunk).await.expect("telnet read");
                assert!(
                    n > 0,
                    "EOF while waiting for {:?}; unconsumed: {:?}",
                    String::from_utf8_lossy(needle),
                    String::from_utf8_lossy(&self.buf[self.pos..])
                );
                self.buf.extend_from_slice(&chunk[..n]);
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "timed out waiting for {:?}; unconsumed: {:?}",
                String::from_utf8_lossy(needle),
                String::from_utf8_lossy(&self.buf[self.pos..])
            )
        })
    }

    /// Log in through the shell prompts and wait for the main-menu prompt.
    async fn login(&mut self, user: &str, pass: &str) {
        self.expect(b"login: ").await;
        self.send(user).await;
        self.expect(b"password: ").await;
        self.send(pass).await;
        self.expect(b"Command: ").await;
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

// ---------------------------------------------------------------------------
// The tests.

#[tokio::test]
async fn login_shows_composed_welcome_screen() {
    let work = tempfile::tempdir().unwrap();
    let mut cfg = test_config(&work.path().join("srv"));
    cfg.motd = "Curiouser and curiouser: welcome down here.".into();
    cfg.welcome_ticker = "Tea party at six.".into();
    // ANSI logo art: a raw client reports no TTYPE, so it must degrade to
    // plain glyphs (no escape bytes on the wire).
    cfg.theme_logo_ansi = "\x1b[1;31mRABBIT-LOGO\x1b[0m".into();
    let burrow = Burrow::start(cfg).await.unwrap();
    let alice = burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    let bob = burrow
        .shared
        .auth
        .create_account("bob", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    // An unread DM makes the welcome screen count it.
    DmsRepo(&burrow.shared.pool)
        .insert(
            bob.id,
            "bob",
            alice.id,
            "alice",
            "hi!",
            None,
            &[],
            1000,
            false,
        )
        .await
        .unwrap();
    let addr = burrow.telnet_addr.expect("telnet enabled");

    let mut c = Client::connect(addr).await;
    c.expect(b"login: ").await;
    c.send("alice").await;
    c.expect(b"password: ").await;
    c.send("pw-pw-pw").await;
    // Widgets render in composer order: logo, MOTD, unread DMs, who's on,
    // ticker — then the menu prompt.
    c.expect(b"RABBIT-LOGO").await;
    c.expect(b"Curiouser and curiouser: welcome down here.")
        .await;
    c.expect(b"1 unread direct message").await;
    c.expect(b"Online now (1): alice").await;
    c.expect(b"News: Tea party at six.").await;
    c.expect(b"Command: ").await;
    assert!(
        !c.buf.contains(&0x1b),
        "ANSI escapes must be stripped for a terminal that reported no TTYPE"
    );
    c.send("q").await;
    c.expect(b"Goodbye, alice!").await;

    burrow.shutdown().await;
}

#[tokio::test]
async fn boards_post_new_thread_and_reread() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .boards
        .create_board(
            "general",
            "General Discussion",
            "All things warren",
            2,
            None,
            0,
        )
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    let addr = burrow.telnet_addr.expect("telnet enabled");

    let mut c = Client::connect(addr).await;
    c.login("alice", "pw-pw-pw").await;

    // [B] lists the postable board.
    c.send("b").await;
    c.expect(b"Message Boards").await;
    c.expect(b"General Discussion").await;
    c.expect(b"boards> ").await;

    // Open it by slug: empty, then post a new thread with the line editor.
    c.send("general").await;
    c.expect(b"--- General Discussion (general) ---").await;
    c.expect(b"no threads yet").await;
    c.expect(b"board general> ").await;
    c.send("n").await;
    c.expect(b"Subject: ").await;
    c.send("Hello Wonderland").await;
    c.expect(b"End with a single `.`").await;
    c.send("Follow the white rabbit.").await;
    c.send("And bring tea.").await;
    c.send(".").await;
    c.expect(b"Posted.").await;

    // The re-listed threads show it; reading it shows both body lines and
    // the author name.
    c.expect(b"Hello Wonderland").await;
    c.expect(b"board general> ").await;
    c.send("1").await;
    c.expect(b"From: alice@bbs-warren").await;
    c.expect(b"Subj: Hello Wonderland").await;
    c.expect(b"Follow the white rabbit.").await;
    c.expect(b"And bring tea.").await;
    c.expect(b"thread> ").await;

    // Reply from the thread prompt; re-reading shows the reply.
    c.send("r").await;
    c.expect(b"Subject [Re: Hello Wonderland]: ").await;
    c.send("").await;
    c.expect(b"End with a single `.`").await;
    c.send("I brought scones instead.").await;
    c.send(".").await;
    c.expect(b"Posted.").await;
    c.send("ls").await;
    c.expect(b"Re: Hello Wonderland").await;
    c.expect(b"I brought scones instead.").await;
    c.expect(b"thread> ").await;

    // Unwind cleanly.
    c.send("q").await;
    c.expect(b"board general> ").await;
    c.send("q").await;
    c.expect(b"boards> ").await;
    c.send("q").await;
    c.expect(b"Command: ").await;
    c.send("q").await;
    c.expect(b"Goodbye, alice!").await;

    burrow.shutdown().await;
}

#[tokio::test]
async fn chat_bridges_telnet_clients_and_native_sessions() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    for login in ["alice", "bob"] {
        burrow
            .shared
            .auth
            .create_account(login, "pw-pw-pw", Role::User)
            .await
            .unwrap();
    }
    let addr = burrow.telnet_addr.expect("telnet enabled");

    let mut a = Client::connect(addr).await;
    a.login("alice", "pw-pw-pw").await;
    a.send("c").await;
    a.expect(b"--- Chat: lobby ---").await;
    a.expect(b"(no recent chat)").await;

    let mut b = Client::connect(addr).await;
    b.login("bob", "pw-pw-pw").await;
    b.send("c").await;
    b.expect(b"--- Chat: lobby ---").await;

    // Bob talks; both clients see the line via the shared bus (bob's own
    // copy comes back formatted, distinct from his local echo).
    b.send("hello from bob").await;
    b.expect(b"<bob> hello from bob").await;
    a.expect(b"<bob> hello from bob").await;

    // Alice answers; bob sees it live.
    a.send("hi bob!").await;
    b.expect(b"<alice> hi bob!").await;

    // A native (non-telnet) session speaks in the same lobby: both telnet
    // clients get the line.
    let native_session = burrow.shared.next_session_id();
    burrow.shared.chat.join_lobby(native_session, "carol");
    burrow
        .shared
        .chat
        .send(LOBBY, native_session, "carol", "greetings from native")
        .unwrap();
    a.expect(b"<carol> greetings from native").await;
    b.expect(b"<carol> greetings from native").await;

    // A late joiner gets the scrollback.
    b.send("/q").await;
    b.expect(b"Command: ").await;
    b.send("c").await;
    b.expect(b"<carol> greetings from native").await;
    b.send("/q").await;
    b.expect(b"Command: ").await;
    b.send("q").await;
    b.expect(b"Goodbye, bob!").await;

    a.send("/q").await;
    a.expect(b"Command: ").await;
    a.send("q").await;
    a.expect(b"Goodbye, alice!").await;

    burrow.shutdown().await;
}

#[tokio::test]
async fn dm_list_read_reply_roundtrip_with_native_user() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    let alice = burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    let bob = burrow
        .shared
        .auth
        .create_account("bob", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    // Bob (a native-side user) mails alice through the shared DM store.
    DmsRepo(&burrow.shared.pool)
        .insert(
            bob.id,
            "bob",
            alice.id,
            "alice",
            "hey alice, fancy a tea?",
            None,
            &[],
            1000,
            false,
        )
        .await
        .unwrap();
    let addr = burrow.telnet_addr.expect("telnet enabled");

    let mut c = Client::connect(addr).await;
    c.login("alice", "pw-pw-pw").await;

    // [D] lists the conversation with one unread; opening it pages the
    // message and marks it read.
    c.send("d").await;
    c.expect(b"--- Direct Mail ---").await;
    c.expect(b"bob").await;
    c.expect(b"mail> ").await;
    c.send("1").await;
    c.expect(b"bob: hey alice, fancy a tea?").await;
    c.expect(b"dm bob> ").await;

    // Reply; it lands in the durable store for the native side.
    c.send("r").await;
    c.expect(b"Message: ").await;
    c.send("on my way!").await;
    c.expect(b"Sent.").await;

    let thread = DmsRepo(&burrow.shared.pool)
        .thread(bob.id, alice.id, 0, 10)
        .await
        .unwrap();
    assert!(
        thread
            .iter()
            .any(|m| m.from_account == alice.id && m.text == "on my way!"),
        "the telnet reply reached the shared DM store"
    );
    assert!(
        DmsRepo(&burrow.shared.pool)
            .unread_for(alice.id)
            .await
            .unwrap()
            .is_empty(),
        "reading the thread marked it read"
    );

    // Re-reading shows both sides of the conversation.
    c.send("ls").await;
    c.expect(b"alice: on my way!").await;
    c.expect(b"dm bob> ").await;
    c.send("q").await;
    c.expect(b"mail> ").await;
    c.send("q").await;
    c.expect(b"Command: ").await;
    c.send("q").await;
    c.expect(b"Goodbye, alice!").await;

    burrow.shutdown().await;
}

#[tokio::test]
async fn go_keyword_jumps_to_a_board() {
    let work = tempfile::tempdir().unwrap();
    let mut cfg = test_config(&work.path().join("srv"));
    cfg.keywords = HashMap::from([("lounge".to_string(), "board:general".to_string())]);
    let burrow = Burrow::start(cfg).await.unwrap();
    burrow
        .shared
        .boards
        .create_board("general", "General Discussion", "", 2, None, 0)
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    let addr = burrow.telnet_addr.expect("telnet enabled");

    let mut c = Client::connect(addr).await;
    c.login("alice", "pw-pw-pw").await;

    // `/go` alone lists the operator keywords.
    c.send("/go").await;
    c.expect(b"--- Keywords ---").await;
    c.expect(b"lounge").await;
    c.expect(b"board:general").await;
    c.expect(b"Command: ").await;

    // The mapped keyword lands inside the board view.
    c.send("/go lounge").await;
    c.expect(b"--- General Discussion (general) ---").await;
    c.expect(b"board general> ").await;

    // `/go` works from a sub-shell prompt too: the bare board slug resolves
    // without any operator mapping and re-enters the board.
    c.send("/go general").await;
    c.expect(b"--- General Discussion (general) ---").await;
    c.expect(b"board general> ").await;
    c.send("q").await;
    c.expect(b"Command: ").await;

    // Unknown keywords answer helpfully.
    c.send("/go jabberwock").await;
    c.expect(b"Nothing answers to `jabberwock`").await;
    c.expect(b"Command: ").await;
    c.send("q").await;
    c.expect(b"Goodbye, alice!").await;

    burrow.shutdown().await;
}

#[tokio::test]
async fn guests_can_read_but_not_post_or_dm() {
    let work = tempfile::tempdir().unwrap();
    let burrow = Burrow::start(test_config(&work.path().join("srv")))
        .await
        .unwrap();
    burrow
        .shared
        .boards
        .create_board("general", "General Discussion", "", 2, None, 0)
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("visitor", "pw-pw-pw", Role::Guest)
        .await
        .unwrap();
    let addr = burrow.telnet_addr.expect("telnet enabled");

    let mut c = Client::connect(addr).await;
    c.login("visitor", "pw-pw-pw").await;

    // Guests hold BOARD_READ (they can browse) but not BOARD_POST.
    c.send("b").await;
    c.expect(b"General Discussion").await;
    c.expect(b"boards> ").await;
    c.send("general").await;
    c.expect(b"board general> ").await;
    c.send("n").await;
    c.expect(b"You do not have permission to post here.").await;
    c.expect(b"board general> ").await;
    c.send("q").await;
    c.expect(b"boards> ").await;
    c.send("q").await;
    c.expect(b"Command: ").await;

    // Direct mail is members-only.
    c.send("d").await;
    c.expect(b"Direct mail needs a member account").await;
    c.expect(b"Command: ").await;
    c.send("q").await;
    c.expect(b"Goodbye, visitor!").await;

    burrow.shutdown().await;
}
