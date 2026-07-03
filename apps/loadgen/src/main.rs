//! warren-stampede: thin CLI over the `warren_stampede` scenario driver.

#![forbid(unsafe_code)]

use std::time::Duration;

use clap::Parser;
use tokio::sync::watch;
use warren_stampede::{run, AuthMode, RunConfig, Scenario};

/// Drive many concurrent RHP sessions against a burrow and report latency
/// percentiles and error counts.
#[derive(Debug, Parser)]
#[command(name = "warren-stampede", version, about)]
struct Args {
    /// Target endpoint: ws://host:port (WebSocket) or host:port (QUIC,
    /// requires --fingerprint).
    #[arg(long)]
    url: String,

    /// Pinned server certificate fingerprint (hex) for QUIC endpoints.
    #[arg(long)]
    fingerprint: Option<String>,

    /// TLS server name for QUIC (defaults to the host part of --url).
    #[arg(long)]
    server_name: Option<String>,

    /// Number of concurrent sessions to ramp up to.
    #[arg(long, default_value_t = 10)]
    sessions: usize,

    /// Sessions started per second during ramp-up.
    #[arg(long, default_value_t = 25.0)]
    ramp_per_sec: f64,

    /// Run length in seconds, measured from the start of the ramp.
    #[arg(long, default_value_t = 30)]
    duration: u64,

    /// What each session does after login: idle | chat | mixed.
    #[arg(long, default_value = "chat")]
    scenario: Scenario,

    /// Log in as guests (the default when no --user-prefix is given).
    #[arg(long, conflicts_with = "user_prefix")]
    guests: bool,

    /// Log in with pre-created accounts named "{prefix}{index}" (0-based).
    #[arg(long, requires = "password")]
    user_prefix: Option<String>,

    /// Password shared by the pre-created accounts.
    #[arg(long, requires = "user_prefix")]
    password: Option<String>,

    /// Abort the run (exit 2) once the error count exceeds this.
    #[arg(long)]
    max_errors: Option<u64>,

    /// Bounded reconnect attempts per session after a drop.
    #[arg(long, default_value_t = 3)]
    max_reconnects: u32,

    /// Print the final report as JSON instead of plain text.
    #[arg(long)]
    json: bool,

    /// Suppress the 5-second progress lines on stderr.
    #[arg(long)]
    quiet: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let mut cfg = RunConfig::new(&args.url);
    cfg.server_name = args.server_name.clone();
    cfg.fingerprint = args.fingerprint.clone();
    cfg.sessions = args.sessions;
    cfg.ramp_per_sec = args.ramp_per_sec;
    cfg.duration = Duration::from_secs(args.duration);
    cfg.scenario = args.scenario;
    cfg.auth = match (&args.user_prefix, &args.password) {
        (Some(prefix), Some(password)) => AuthMode::Password {
            user_prefix: prefix.clone(),
            password: password.clone(),
        },
        _ => AuthMode::Guest,
    };
    cfg.max_errors = args.max_errors;
    cfg.max_reconnects = args.max_reconnects;
    cfg.progress = !args.quiet;

    // Ctrl-C flips the shutdown signal; sessions drain cleanly and the
    // report still covers what ran.
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("[stampede] ctrl-c: draining sessions...");
            let _ = shutdown_tx.send(true);
        }
    });

    let outcome = run(cfg, shutdown_rx).await;
    let report = &outcome.report;
    if args.json {
        println!("{}", report.to_json());
    } else {
        println!("{}", report.render_text());
    }

    // 0 clean, 1 finished with errors, 2 circuit breaker abort.
    let code = if report.aborted.is_some() {
        2
    } else if report.errors > 0 {
        1
    } else {
        0
    };
    std::process::exit(code);
}
