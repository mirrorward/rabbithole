//! Wave 6 legacy-surface polish, end to end: per-listener minimum-role
//! gates (`telnet_min_role`, `finger_min_role`) and the telnet file browser
//! with its HTTP-link handoff (`files` / `get` — no bytes ever stream over
//! telnet; the printed link is served by the web slice later).

use std::time::Duration;

use burrow::Burrow;
use rabbithole_server_core::{Role, ServerConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn test_config(dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        name: "Polish Warren".into(),
        quic_addr: "127.0.0.1:0".parse().unwrap(),
        ws_addr: "127.0.0.1:0".parse().unwrap(),
        telnet_enabled: true,
        telnet_addr: "127.0.0.1:0".parse().unwrap(),
        data_dir: dir.to_path_buf(),
        ..ServerConfig::default()
    }
}

// ---------------------------------------------------------------------------
// A tiny deterministic telnet client: accumulate bytes, wait for markers
// (same discipline as e2e_w6_doors — no blind sleeps).

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

    /// Read until `needle` appears past the consumption point; advance past
    /// the match.
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

    /// Everything received so far, decoded lossily (for "must NOT contain"
    /// assertions once a later marker proved the listing completed).
    fn transcript(&self) -> String {
        String::from_utf8_lossy(&self.buf).into_owned()
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

// ---------------------------------------------------------------------------
// Slice B: per-listener minimum-role gates.

#[tokio::test]
async fn telnet_min_role_refuses_guest_accepts_member() {
    let work = tempfile::tempdir().unwrap();
    let mut cfg = test_config(&work.path().join("srv"));
    cfg.telnet_min_role = "user".into();
    let burrow = Burrow::start(cfg).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("visitor", "pw-pw-pw", Role::Guest)
        .await
        .unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    let addr = burrow.telnet_addr.expect("telnet enabled");

    // The guest account presents correct credentials but is below the
    // surface minimum: refused with the message, never reaching the menu.
    let mut g = Client::connect(addr).await;
    g.expect(b"login: ").await;
    g.send("visitor").await;
    g.expect(b"password: ").await;
    g.send("pw-pw-pw").await;
    g.expect(b"requires user access or better").await;
    assert!(
        !g.transcript().contains("Command: "),
        "guest must not reach the menu"
    );

    // A member sails through.
    let mut m = Client::connect(addr).await;
    m.login("alice", "pw-pw-pw").await;
    m.send("q").await;
    m.expect(b"Goodbye, alice!").await;

    burrow.shutdown().await;
}

#[tokio::test]
async fn finger_min_role_restricts_the_anonymous_surface() {
    let work = tempfile::tempdir().unwrap();
    let mut cfg = test_config(&work.path().join("srv"));
    cfg.finger_enabled = true;
    cfg.finger_addr = "127.0.0.1:0".parse().unwrap();
    cfg.finger_min_role = "user".into();
    let burrow = Burrow::start(cfg).await.unwrap();
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    let addr = burrow.finger_addr.expect("finger enabled");

    // Finger is anonymous, so min>guest refuses every query.
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(b"alice\r\n").await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf);
    assert!(text.contains("restricted"), "refused: {text:?}");
    assert!(!text.contains("No Plan."), "no profile leaked: {text:?}");

    // The gate applies live: back to guest, the same query answers.
    assert!(burrow
        .shared
        .config
        .set_key("finger_min_role", "guest")
        .unwrap());
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(b"alice\r\n").await.unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.unwrap();
    let text = String::from_utf8_lossy(&buf);
    assert!(text.contains("alice"), "profile served again: {text:?}");

    burrow.shutdown().await;
}

// ---------------------------------------------------------------------------
// Slice C: telnet file browser + HTTP-link handoff.

/// Seed an area with a root file, a subfolder with a file, and a drop box
/// hiding one.
async fn seed_library(burrow: &Burrow) {
    let files = &burrow.shared.files;
    files
        .create_area("pub", "Public", "the goods")
        .await
        .unwrap();
    files
        .add_file(
            "pub",
            None,
            "readme.txt",
            &[1u8; 32],
            1234,
            "text/plain",
            "",
            "",
            "sysop",
            1,
        )
        .await
        .unwrap();
    files.mkdir("pub", None, "docs", false).await.unwrap();
    files
        .add_file(
            "pub",
            Some("docs"),
            "guide (v2).txt",
            &[2u8; 32],
            99_999,
            "text/plain",
            "",
            "",
            "sysop",
            1,
        )
        .await
        .unwrap();
    files.mkdir("pub", None, "inbox", true).await.unwrap();
    files
        .add_file(
            "pub",
            Some("inbox"),
            "secret.zip",
            &[3u8; 32],
            7,
            "application/zip",
            "",
            "",
            "sysop",
            1,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn files_browser_lists_pages_and_hands_off_http_links() {
    let work = tempfile::tempdir().unwrap();
    let mut cfg = test_config(&work.path().join("srv"));
    cfg.files_http_base = "http://dl.example.org:8080/".into(); // trailing / is trimmed
    let burrow = Burrow::start(cfg).await.unwrap();
    seed_library(&burrow).await;
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    let addr = burrow.telnet_addr.expect("telnet enabled");

    let mut c = Client::connect(addr).await;
    c.login("alice", "pw-pw-pw").await;

    // Root: the area table.
    c.send("files").await;
    c.expect(b"File Library").await;
    c.expect(b"pub").await;
    c.expect(b"files /> ").await;

    // Area root: files and folders as an ASCII table (name, size, uploaded),
    // the drop box visible as a folder but its contents hidden.
    c.send("cd pub").await;
    c.expect(b"files /pub> ").await;
    c.send("ls").await;
    c.expect(b"NAME").await;
    c.expect(b"SIZE").await;
    c.expect(b"UPLOADED").await;
    c.expect(b"files /pub> ").await;
    let t = c.transcript();
    assert!(t.contains("readme.txt"), "root file listed: {t}");
    assert!(t.contains("1234B"), "size shown: {t}");
    assert!(t.contains("docs/"), "folder listed: {t}");
    assert!(t.contains("inbox/"), "drop box folder itself listed: {t}");
    assert!(
        !t.contains("secret.zip"),
        "drop-box contents must stay hidden: {t}"
    );

    // Inside the drop box: contents hidden for a member.
    c.send("cd inbox").await;
    c.expect(b"files /pub/inbox> ").await;
    c.send("ls").await;
    c.expect(b"contents are hidden").await;
    // The same drop-box rule Hotline's DownloadFile applies: known but not
    // downloadable without view/manage rights.
    c.send("get secret.zip").await;
    c.expect(b"You do not have permission to download that file")
        .await;
    assert!(
        !c.transcript().contains("secret.zip  "),
        "hidden file never tabulated"
    );

    // `get` prints the HTTP handoff link (never streams bytes).
    c.send("cd ..").await;
    c.expect(b"files /pub> ").await;
    c.send("get readme.txt").await;
    c.expect(b"http://dl.example.org:8080/files/pub/readme.txt")
        .await;

    // Nested path + characters needing escapes are percent-encoded.
    c.send("cd docs").await;
    c.expect(b"files /pub/docs> ").await;
    c.send("get guide (v2).txt").await;
    c.expect(b"http://dl.example.org:8080/files/pub/docs/guide%20%28v2%29.txt")
        .await;

    // Back out to the menu and quit cleanly.
    c.send("q").await;
    c.expect(b"Command: ").await;
    c.send("q").await;
    c.expect(b"Goodbye, alice!").await;

    burrow.shutdown().await;
}

#[tokio::test]
async fn files_get_without_base_explains_no_telnet_transfers() {
    let work = tempfile::tempdir().unwrap();
    let cfg = test_config(&work.path().join("srv")); // files_http_base = ""
    let burrow = Burrow::start(cfg).await.unwrap();
    seed_library(&burrow).await;
    burrow
        .shared
        .auth
        .create_account("alice", "pw-pw-pw", Role::User)
        .await
        .unwrap();
    let addr = burrow.telnet_addr.expect("telnet enabled");

    let mut c = Client::connect(addr).await;
    c.login("alice", "pw-pw-pw").await;
    c.send("files").await;
    c.expect(b"files /> ").await;
    c.send("cd pub").await;
    c.expect(b"files /pub> ").await;
    c.send("get readme.txt").await;
    c.expect(b"not available on telnet").await;
    assert!(
        !c.transcript().contains("http://"),
        "no link without a base"
    );
    c.send("q").await;
    c.expect(b"Command: ").await;
    c.send("q").await;
    c.expect(b"Goodbye").await;

    burrow.shutdown().await;
}
