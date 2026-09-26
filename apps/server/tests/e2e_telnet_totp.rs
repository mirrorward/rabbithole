//! RH-30: real Telnet prompts use shared password/TOTP verification before
//! admitting a session. Enrollment is fixture setup through the store seam;
//! login, echo suppression, limits and role checks run over the TCP listener.

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use burrow::Burrow;
use rabbithole_identity::hash_password;
use rabbithole_identity::totp::{generate_recovery_codes, TotpEnrollment};
use rabbithole_server_core::ratelimit::{class as rl, Scope};
use rabbithole_server_core::{Role, ServerConfig};
use rabbithole_store_server::repo::{AccountsRepo, SessionsRepo};
use rabbithole_store_server::repo2::TotpRepo;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const PASSWORD: &str = "telnet-fixture-password";
const CODE_PROMPT: &str = "Authenticator or recovery code (empty to cancel): ";

struct Fixture {
    _dir: tempfile::TempDir,
    server: Burrow,
    account: i64,
    enrollment: TotpEnrollment,
    recovery: String,
}

impl Fixture {
    async fn new(role: Role, change: impl FnOnce(&mut ServerConfig)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = ServerConfig {
            data_dir: dir.path().into(),
            quic_addr: "127.0.0.1:0".parse().unwrap(),
            ws_addr: "127.0.0.1:0".parse().unwrap(),
            telnet_enabled: true,
            telnet_addr: "127.0.0.1:0".parse().unwrap(),
            ratelimit_auth_per_min: 1,
            ratelimit_auth_burst: 20,
            ..Default::default()
        };
        change(&mut cfg);
        let server = Burrow::start(cfg).await.unwrap();
        let account = server
            .shared
            .auth
            .create_account("alice", PASSWORD, role)
            .await
            .unwrap()
            .id;
        let enrollment = TotpEnrollment::generate("RabbitHole", "alice");
        let recovery = generate_recovery_codes(1).pop().unwrap();
        let repo = TotpRepo(&server.shared.pool);
        repo.begin(account, enrollment.secret()).await.unwrap();
        repo.confirm(account, &[recovery.1]).await.unwrap();
        Self {
            _dir: dir,
            server,
            account,
            enrollment,
            recovery: recovery.0,
        }
    }

    async fn connect(&self) -> Terminal {
        Terminal {
            socket: TcpStream::connect(self.server.telnet_addr.unwrap())
                .await
                .unwrap(),
            bytes: Vec::new(),
            consumed: 0,
        }
    }

    // The store's revocation count proves whether a token was issued without
    // exposing token material or introducing a test-only database dependency.
    // These fixtures do not exercise token resume.
    async fn take_session_count(&self) -> u64 {
        SessionsRepo(&self.server.shared.pool)
            .revoke_account(self.account)
            .await
            .unwrap()
    }

    async fn unauthenticated(&self) {
        assert_eq!(
            self.take_session_count().await,
            0,
            "no resumable token issued"
        );
        assert_eq!(self.server.shared.presence.count(), 0, "no presence entry");
    }

    fn ip_scope() -> Scope {
        Scope::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST))
    }
}

struct Terminal {
    socket: TcpStream,
    bytes: Vec<u8>,
    consumed: usize,
}

impl Terminal {
    async fn send(&mut self, line: &str) {
        self.socket
            .write_all(format!("{line}\r\n").as_bytes())
            .await
            .unwrap();
    }

    async fn expect(&mut self, text: &str) {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some(offset) = self.bytes[self.consumed..]
                    .windows(text.len())
                    .position(|part| part == text.as_bytes())
                {
                    self.consumed += offset + text.len();
                    return;
                }
                let mut buf = [0; 4096];
                let n = self.socket.read(&mut buf).await.unwrap();
                assert_ne!(n, 0, "EOF waiting for {text:?}: {:?}", self.transcript());
                self.bytes.extend_from_slice(&buf[..n]);
            }
        })
        .await
        .unwrap_or_else(|_| panic!("waiting for {text:?}: {:?}", self.transcript()));
    }

    async fn eof(&mut self) {
        tokio::time::timeout(
            Duration::from_secs(15),
            self.socket.read_to_end(&mut self.bytes),
        )
        .await
        .unwrap()
        .unwrap();
    }

    async fn password(&mut self, password: &str) {
        self.expect("login: ").await;
        self.send("alice").await;
        self.expect("password: ").await;
        self.send(password).await;
    }

    async fn challenge(&mut self) {
        self.password(PASSWORD).await;
        self.expect(CODE_PROMPT).await;
    }

    fn transcript(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }

    fn no_entry(&self) {
        assert!(!self.transcript().contains("Welcome, alice!"));
        assert!(!self.transcript().contains("Command: "));
    }
}

#[tokio::test]
async fn correct_totp_is_hidden_and_enters_only_after_full_verification() {
    let f = Fixture::new(Role::User, |c| c.ratelimit_auth_burst = 1).await;
    let mut terminal = f.connect().await;
    terminal.challenge().await;
    f.unauthenticated().await;
    let code = f.enrollment.current_code().unwrap();
    terminal.send(&format!("  {code}  ")).await;
    terminal.expect("Command: ").await;
    assert_eq!(f.take_session_count().await, 1);
    assert!(f
        .server
        .shared
        .presence
        .is_screen_name_online("alice")
        .is_some());
    assert!(!terminal.transcript().contains(PASSWORD));
    assert!(!terminal.transcript().contains(&code));
    assert!(
        f.server.shared.rate_allow(Fixture::ip_scope(), rl::AUTH),
        "success keeps the failure token"
    );
    assert!(!f.server.shared.rate_allow(Fixture::ip_scope(), rl::AUTH));
    terminal.send("q").await;
    terminal.expect("Goodbye, alice!").await;
    f.server.shutdown().await;
}

#[tokio::test]
async fn recovery_code_works_once_and_is_not_echoed() {
    let f = Fixture::new(Role::User, |_| {}).await;
    let mut terminal = f.connect().await;
    terminal.challenge().await;
    terminal.send(&f.recovery).await;
    terminal.expect("Command: ").await;
    assert!(!terminal.transcript().contains(&f.recovery));
    terminal.send("q").await;
    terminal.eof().await;
    assert!(TotpRepo(&f.server.shared.pool)
        .get(f.account)
        .await
        .unwrap()
        .unwrap()
        .recovery_hashes
        .is_empty());
    let mut reuse = f.connect().await;
    reuse.challenge().await;
    reuse.send(&f.recovery).await;
    reuse.expect("Login incorrect.").await;
    reuse.expect("login: ").await;
    reuse.no_entry();
    assert_eq!(
        f.take_session_count().await,
        1,
        "failed reuse issued no new token"
    );
    assert_eq!(f.server.shared.presence.count(), 0);
    f.server.shutdown().await;
}

#[tokio::test]
async fn wrong_password_never_prompts_for_code_and_wrong_codes_keep_attempt_limit() {
    let f = Fixture::new(Role::User, |_| {}).await;
    let mut terminal = f.connect().await;
    terminal.password("incorrect-password").await;
    terminal.expect("Login incorrect.").await;
    assert!(!terminal.transcript().contains(CODE_PROMPT));
    for _ in 0..2 {
        terminal.challenge().await;
        terminal.send("123").await; // invalid length, never a valid current code
        terminal.expect("Login incorrect.").await;
    }
    terminal.expect("Too many failures. Goodbye.").await;
    terminal.eof().await;
    terminal.no_entry();
    f.unauthenticated().await;
    f.server.shutdown().await;
}

#[tokio::test]
async fn empty_code_and_disconnect_cancel_without_tokens_or_budget_bypass() {
    let f = Fixture::new(Role::User, |c| c.ratelimit_auth_burst = 2).await;
    let mut empty = f.connect().await;
    empty.challenge().await;
    empty.send("   ").await;
    empty.expect("Login cancelled.").await;
    empty.eof().await;
    empty.no_entry();
    f.unauthenticated().await;

    let mut disconnected = f.connect().await;
    disconnected.challenge().await;
    disconnected
        .socket
        .write_all(b"partial-code")
        .await
        .unwrap();
    disconnected.socket.shutdown().await.unwrap();
    disconnected.eof().await;
    disconnected.no_entry();
    f.unauthenticated().await;
    let mut fresh = f.connect().await;
    fresh
        .expect("Too many failed logins. Try again later.")
        .await;
    fresh.eof().await;
    assert!(!fresh.transcript().contains("login: "));
    f.server.shutdown().await;
}

#[tokio::test]
async fn wrong_codes_exhaust_the_shared_ip_budget_across_connections() {
    let f = Fixture::new(Role::User, |c| c.ratelimit_auth_burst = 1).await;
    let mut terminal = f.connect().await;
    terminal.challenge().await;
    terminal.send("123").await;
    terminal.expect("Login incorrect.").await;
    terminal
        .expect("Too many failed logins. Try again later.")
        .await;
    terminal.eof().await;
    let mut fresh = f.connect().await;
    fresh
        .expect("Too many failed logins. Try again later.")
        .await;
    fresh.eof().await;
    f.unauthenticated().await;
    f.server.shutdown().await;
}

#[tokio::test]
async fn waiting_for_a_code_does_not_bypass_newly_exhausted_auth_budget() {
    let f = Fixture::new(Role::User, |c| c.ratelimit_auth_burst = 1).await;
    let mut waiting = f.connect().await;
    waiting.challenge().await;
    let mut other = f.connect().await;
    other.password("incorrect-password").await;
    other
        .expect("Too many failed logins. Try again later.")
        .await;
    waiting.send(&f.enrollment.current_code().unwrap()).await;
    waiting
        .expect("Too many failed logins. Try again later.")
        .await;
    waiting.eof().await;
    waiting.no_entry();
    f.unauthenticated().await;
    f.server.shutdown().await;
}

#[tokio::test]
async fn account_password_and_disabled_state_are_rechecked_after_the_prompt() {
    for disable in [false, true] {
        let f = Fixture::new(Role::User, |_| {}).await;
        let mut terminal = f.connect().await;
        terminal.challenge().await;
        if disable {
            AccountsRepo(&f.server.shared.pool)
                .admin_set("alice", None, None, Some(true))
                .await
                .unwrap();
        } else {
            AccountsRepo(&f.server.shared.pool)
                .update_phc(f.account, &hash_password("changed-password").unwrap())
                .await
                .unwrap();
        }
        terminal.send(&f.enrollment.current_code().unwrap()).await;
        terminal.expect("Login incorrect.").await;
        terminal.expect("login: ").await;
        terminal.no_entry();
        f.unauthenticated().await;
        f.server.shutdown().await;
    }
}

#[tokio::test]
async fn a_valid_second_factor_still_observes_the_live_telnet_role_gate() {
    let f = Fixture::new(Role::User, |_| {}).await;
    let mut terminal = f.connect().await;
    terminal.challenge().await;
    f.server
        .shared
        .config
        .set_key("telnet_min_role", "admin")
        .unwrap();
    terminal.send(&f.enrollment.current_code().unwrap()).await;
    terminal
        .expect("requires admin access or better on telnet")
        .await;
    terminal.eof().await;
    terminal.no_entry();
    assert_eq!(f.server.shared.presence.count(), 0);
    f.server.shutdown().await;
}
