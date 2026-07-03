//! The scenario driver: ramps up N sessions, runs them until the deadline
//! (or shutdown / circuit breaker), and aggregates metrics.
//!
//! Each session is the *real* client (`rabbithole_core::Client`) — the same
//! transport selection (ws:// URL or QUIC host:port + pinned fingerprint),
//! hello negotiation, auth, and push handling every frontend uses.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use rabbithole_core::{Client, ClientError};
use rabbithole_proto::chat::ChatMessage;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::metrics::{Metrics, Report};

/// What each session does after login.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// Keepalive pings only.
    Idle,
    /// A lobby line every jittered interval; measure the own-echo RTT.
    Chat,
    /// 80% idle ticks / 20% chat ticks.
    Mixed,
}

impl Scenario {
    pub fn as_str(&self) -> &'static str {
        match self {
            Scenario::Idle => "idle",
            Scenario::Chat => "chat",
            Scenario::Mixed => "mixed",
        }
    }
}

impl std::str::FromStr for Scenario {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "idle" => Ok(Scenario::Idle),
            "chat" => Ok(Scenario::Chat),
            "mixed" => Ok(Scenario::Mixed),
            other => Err(format!("unknown scenario `{other}` (idle|chat|mixed)")),
        }
    }
}

impl std::fmt::Display for Scenario {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How sessions authenticate.
#[derive(Debug, Clone)]
pub enum AuthMode {
    /// Guest login with a per-session desired name (`stampede-<idx>`).
    Guest,
    /// Pre-created accounts `"{user_prefix}{idx}"` (0-based), one password.
    Password {
        user_prefix: String,
        password: String,
    },
}

/// Everything one load run needs.
#[derive(Debug, Clone)]
pub struct RunConfig {
    /// `ws://host:port` / `wss://…` for WebSocket, or `host:port` for QUIC
    /// (QUIC also needs `fingerprint`).
    pub url: String,
    /// TLS server name for QUIC (defaults to the host part).
    pub server_name: Option<String>,
    /// Pinned certificate fingerprint (hex) — required for QUIC endpoints.
    pub fingerprint: Option<String>,
    /// Target number of concurrent sessions.
    pub sessions: usize,
    /// Sessions started per second during ramp-up.
    pub ramp_per_sec: f64,
    /// Run length, measured from the start of the ramp.
    pub duration: Duration,
    pub scenario: Scenario,
    pub auth: AuthMode,
    /// Abort the whole run once the error count *exceeds* this (None = off).
    pub max_errors: Option<u64>,
    /// Bounded reconnect attempts per session after a drop.
    pub max_reconnects: u32,
    /// Jitter range between chat/idle ticks (min, max).
    pub chat_interval: (Duration, Duration),
    /// How long to wait for a sent line's own echo before counting an error.
    pub echo_timeout: Duration,
    /// Transport connect + auth guard.
    pub connect_timeout: Duration,
    /// Print a progress line to stderr every 5s.
    pub progress: bool,
}

impl RunConfig {
    /// Defaults matching the CLI's: 10 guest chat sessions for 30s.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            server_name: None,
            fingerprint: None,
            sessions: 10,
            ramp_per_sec: 25.0,
            duration: Duration::from_secs(30),
            scenario: Scenario::Chat,
            auth: AuthMode::Guest,
            max_errors: None,
            max_reconnects: 3,
            chat_interval: (Duration::from_secs(5), Duration::from_secs(15)),
            echo_timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(15),
            progress: false,
        }
    }
}

/// The result of a run: the final report (which also carries the abort
/// reason, if the circuit breaker fired).
#[derive(Debug)]
pub struct RunOutcome {
    pub report: Report,
}

/// Drive a full load run. `shutdown` flipping to `true` (e.g. on Ctrl-C)
/// drains all sessions cleanly; the report still covers what ran.
pub async fn run(cfg: RunConfig, shutdown: watch::Receiver<bool>) -> RunOutcome {
    let cfg = Arc::new(cfg);
    let metrics = Arc::new(Metrics::default());
    let start = Instant::now();
    let deadline = start + cfg.duration;

    // Internal stop signal: external shutdown, circuit breaker, or deadline
    // (sessions also watch the deadline themselves).
    let (stop_tx, stop_rx) = watch::channel(false);
    let abort_reason: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    // Forward the caller's shutdown signal into our stop channel.
    let forwarder = tokio::spawn(forward_shutdown(shutdown, stop_tx.clone()));

    // Circuit breaker: abort once errors exceed the budget.
    let breaker = cfg.max_errors.map(|max| {
        tokio::spawn(circuit_breaker(
            max,
            metrics.clone(),
            stop_tx.clone(),
            stop_rx.clone(),
            abort_reason.clone(),
        ))
    });

    // Progress line every 5s.
    let progress = cfg
        .progress
        .then(|| tokio::spawn(progress_loop(cfg.clone(), metrics.clone(), stop_rx.clone())));

    // Ramp: spawn sessions at ramp_per_sec until target/stop/deadline.
    let mut set = tokio::task::JoinSet::new();
    let spawn_gap = Duration::from_secs_f64(1.0 / cfg.ramp_per_sec.max(0.001));
    let mut ramp_stop = stop_rx.clone();
    for idx in 0..cfg.sessions {
        if *ramp_stop.borrow() || Instant::now() >= deadline {
            break;
        }
        metrics.add(&metrics.sessions_started, 1);
        set.spawn(session_task(
            idx,
            cfg.clone(),
            metrics.clone(),
            stop_rx.clone(),
            deadline,
        ));
        if idx + 1 < cfg.sessions {
            tokio::select! {
                _ = tokio::time::sleep(spawn_gap) => {}
                _ = ramp_stop.wait_for(|s| *s) => break,
                _ = tokio::time::sleep_until(deadline) => break,
            }
        }
    }

    // Wait for every session to drain.
    while set.join_next().await.is_some() {}

    // Tear down the auxiliary tasks.
    let _ = stop_tx.send(true);
    forwarder.abort();
    if let Some(b) = breaker {
        b.abort();
    }
    if let Some(p) = progress {
        p.abort();
    }

    let aborted = abort_reason.lock().expect("not poisoned").clone();
    let report = metrics.report(
        cfg.scenario.as_str(),
        cfg.sessions as u64,
        start.elapsed(),
        aborted,
    );
    RunOutcome { report }
}

async fn forward_shutdown(mut shutdown: watch::Receiver<bool>, stop_tx: watch::Sender<bool>) {
    // Fires on the first `true`; a dropped sender ends the task quietly.
    if shutdown.wait_for(|s| *s).await.is_ok() {
        let _ = stop_tx.send(true);
    }
}

async fn circuit_breaker(
    max: u64,
    metrics: Arc<Metrics>,
    stop_tx: watch::Sender<bool>,
    mut stop_rx: watch::Receiver<bool>,
    abort_reason: Arc<Mutex<Option<String>>>,
) {
    loop {
        let errors = Metrics::get(&metrics.errors);
        if errors > max {
            *abort_reason.lock().expect("not poisoned") = Some(format!(
                "circuit breaker: {errors} errors > --max-errors {max}"
            ));
            let _ = stop_tx.send(true);
            return;
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
            _ = stop_rx.wait_for(|s| *s) => return,
        }
    }
}

async fn progress_loop(
    cfg: Arc<RunConfig>,
    metrics: Arc<Metrics>,
    mut stop_rx: watch::Receiver<bool>,
) {
    let started = Instant::now();
    let mut last_msgs = 0u64;
    let mut last_at = started;
    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(5)) => {}
            _ = stop_rx.wait_for(|s| *s) => return,
        }
        let msgs = Metrics::get(&metrics.msgs_sent);
        let rate = (msgs - last_msgs) as f64 / last_at.elapsed().as_secs_f64().max(0.001);
        eprintln!(
            "[stampede +{:>4.0}s] sessions {}/{} up ({} logged in) | {:.1} msg/s | errors {} | reconnects {}",
            started.elapsed().as_secs_f64(),
            Metrics::get(&metrics.sessions_active),
            cfg.sessions,
            Metrics::get(&metrics.sessions_logged_in),
            rate,
            Metrics::get(&metrics.errors),
            Metrics::get(&metrics.reconnects),
        );
        last_msgs = msgs;
        last_at = Instant::now();
    }
}

/// Decrements the active-sessions gauge on drop, whatever the exit path.
struct ActiveGuard(Arc<Metrics>);

impl ActiveGuard {
    fn new(m: &Arc<Metrics>) -> Self {
        m.add(&m.sessions_active, 1);
        Self(m.clone())
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.sessions_active.fetch_sub(1, Ordering::Relaxed);
    }
}

/// One session's whole life, including bounded reconnects.
async fn session_task(
    idx: usize,
    cfg: Arc<RunConfig>,
    metrics: Arc<Metrics>,
    mut stop: watch::Receiver<bool>,
    deadline: Instant,
) {
    let mut reconnects_left = cfg.max_reconnects;
    loop {
        if *stop.borrow() || Instant::now() >= deadline {
            break;
        }
        match drive_session(idx, &cfg, &metrics, &mut stop, deadline).await {
            Ok(()) => break, // drained cleanly
            Err(e) => {
                metrics.add(&metrics.errors, 1);
                metrics.add(&metrics.disconnects, 1);
                if reconnects_left == 0 || *stop.borrow() || Instant::now() >= deadline {
                    if cfg.progress {
                        eprintln!("[stampede] session {idx} gave up: {e}");
                    }
                    break;
                }
                reconnects_left -= 1;
                metrics.add(&metrics.reconnects, 1);
                // Short jittered backoff before redialing.
                let mut rng = StdRng::seed_from_u64(idx as u64 ^ 0x5eed);
                let backoff = Duration::from_millis(250 + rng.gen_range(0..250));
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = stop.wait_for(|s| *s) => break,
                }
            }
        }
    }
    metrics.add(&metrics.sessions_completed, 1);
}

async fn with_timeout<T>(
    d: Duration,
    what: &'static str,
    fut: impl std::future::Future<Output = Result<T, ClientError>>,
) -> Result<T, ClientError> {
    match tokio::time::timeout(d, fut).await {
        Ok(r) => r,
        Err(_) => Err(ClientError::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            what,
        ))),
    }
}

/// One connection: connect → login → scenario loop → graceful close.
async fn drive_session(
    idx: usize,
    cfg: &RunConfig,
    metrics: &Arc<Metrics>,
    stop: &mut watch::Receiver<bool>,
    deadline: Instant,
) -> Result<(), ClientError> {
    // Connect (measured).
    let t0 = Instant::now();
    let mut client = with_timeout(
        cfg.connect_timeout,
        "connect timed out",
        Client::connect(
            &cfg.url,
            cfg.server_name.as_deref(),
            cfg.fingerprint.as_deref(),
            "warren-stampede",
            env!("CARGO_PKG_VERSION"),
        ),
    )
    .await?;
    metrics.connect_latency.record(t0.elapsed());
    metrics.add(&metrics.sessions_connected, 1);

    // Login (auth + welcome push, measured together).
    let t1 = Instant::now();
    let ok = with_timeout(cfg.connect_timeout, "auth timed out", async {
        match &cfg.auth {
            AuthMode::Guest => client.auth_guest(Some(format!("stampede-{idx}"))).await,
            AuthMode::Password {
                user_prefix,
                password,
            } => {
                client
                    .auth_password(&format!("{user_prefix}{idx}"), password)
                    .await
            }
        }
    })
    .await?;
    with_timeout(
        cfg.connect_timeout,
        "welcome timed out",
        client.expect_welcome(),
    )
    .await?;
    metrics.login_latency.record(t1.elapsed());
    metrics.add(&metrics.sessions_logged_in, 1);
    let me = ok.screen_name;

    let _active = ActiveGuard::new(metrics);
    let mut rng = StdRng::seed_from_u64(((idx as u64) << 17) | 0xace);
    let mut seq = 0u64;
    let mut echoed_once = false;
    let (min_gap, max_gap) = cfg.chat_interval;

    loop {
        // First tick fires almost immediately (small stagger) so short runs
        // still exercise every session's send path; later ticks jitter
        // across [min_gap, max_gap].
        let wait = if seq == 0 {
            Duration::from_millis(rng.gen_range(0..500))
        } else {
            let (lo, hi) = (min_gap.as_millis() as u64, max_gap.as_millis() as u64);
            Duration::from_millis(rng.gen_range(lo..=hi.max(lo)))
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = stop.wait_for(|s| *s) => break,
            _ = tokio::time::sleep_until(deadline) => break,
        }
        seq += 1;

        // Drain any pushes buffered since the last tick (chat from other
        // sessions, presence, ...). Zero-ish timeout: buffered frames pop
        // immediately; both transports' recv paths are cancel-safe.
        loop {
            match tokio::time::timeout(Duration::from_millis(5), client.next_push()).await {
                Err(_) => break,                                 // nothing pending
                Ok(Ok(None)) => return Err(ClientError::Closed), // server hung up
                Ok(Err(e)) => return Err(e),
                Ok(Ok(Some(_frame))) => metrics.add(&metrics.pushes_seen, 1),
            }
        }

        let do_chat = match cfg.scenario {
            Scenario::Idle => false,
            Scenario::Chat => true,
            Scenario::Mixed => rng.gen_bool(0.2),
        };
        if do_chat {
            chat_tick(idx, seq, &me, cfg, metrics, &mut client, &mut echoed_once).await?;
        } else {
            // Keepalive.
            with_timeout(cfg.echo_timeout, "ping timed out", client.ping()).await?;
        }
    }

    client.close().await;
    Ok(())
}

/// Send one lobby line and wait (bounded) for our own echo push.
async fn chat_tick(
    idx: usize,
    seq: u64,
    me: &str,
    cfg: &RunConfig,
    metrics: &Arc<Metrics>,
    client: &mut Client,
    echoed_once: &mut bool,
) -> Result<(), ClientError> {
    let marker = format!("stampede s{idx} #{seq}");
    let sent_at = Instant::now();
    client.chat_send("lobby", &marker).await?;
    metrics.add(&metrics.msgs_sent, 1);

    let echo_deadline = sent_at + cfg.echo_timeout;
    let mut got = false;
    while Instant::now() < echo_deadline {
        let remain = echo_deadline - Instant::now();
        match tokio::time::timeout(remain, client.next_push()).await {
            Err(_) => break, // echo timeout
            Ok(Ok(None)) => return Err(ClientError::Closed),
            Ok(Err(e)) => return Err(e),
            Ok(Ok(Some(frame))) => {
                metrics.add(&metrics.pushes_seen, 1);
                if let Some(Ok(msg)) = frame.decode::<ChatMessage>() {
                    if msg.from == me && msg.text == marker {
                        metrics.chat_rtt.record(sent_at.elapsed());
                        metrics.add(&metrics.echoes_seen, 1);
                        if !*echoed_once {
                            *echoed_once = true;
                            metrics.add(&metrics.sessions_echoed, 1);
                        }
                        got = true;
                        break;
                    }
                }
            }
        }
    }
    if !got {
        metrics.add(&metrics.echo_timeouts, 1);
        metrics.add(&metrics.errors, 1);
    }
    Ok(())
}
