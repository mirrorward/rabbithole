//! Door-game hosting (Wave 6): the tokio driving slice over the pure
//! `rabbithole-legacy-doors` session-runner model.
//!
//! [`DoorService`] is assembled once at boot from config (`doors_enabled`,
//! `doors_dir`, `doors_max_nodes`, `doors_session_max_secs`, the `[[doors]]`
//! list) and lives on [`Shared`]. [`run_door`] drives one caller through one
//! door from the telnet shell:
//!
//! 1. RBAC-gate on [`Caps::DOOR_RUN`] over the `doors/<id>` resource
//!    (member+ by default; per-class/account/ACL overridable).
//! 2. Allocate a node from the shared [`NodePool`] — a single-node door's
//!    range naturally serializes its callers; a full pool refuses politely.
//! 3. Write the drop file rendered by [`prepare_dropfile`] into the
//!    per-node drop directory `<doors_dir>/node<N>/`.
//! 4. Spawn the door's argv (`tokio::process`) after `%`-token substitution.
//! 5. Pump bytes both ways between the telnet connection and the child's
//!    stdio through a [`BridgeBuffer`] — 8-bit clean, telnet-IAC safe.
//! 6. Enforce the [`DoorSession`] FSM and the per-door time budget: on
//!    expiry the session moves to `TimedOut` and the child is killed.
//! 7. Release the node (the RAII lease drops) and audit-log the run.
//!
//! Daily limits accumulate per account and door in UTC calendar days. Before
//! spawn, a durable reservation subtracts completed and concurrently reserved
//! time. Normal completion refunds unused milliseconds; a crash conservatively
//! retains the prepaid reservation. Daily-limited runs end at UTC midnight so
//! the next run uses the new day's allowance. The global session cap still
//! applies to every run; doors without a daily limit do not stop at midnight.
//!
//! ## `%`-token substitution
//!
//! Every element of a door's `command` argv (the program included) may use:
//!
//! | token | expands to                                                     |
//! |-------|----------------------------------------------------------------|
//! | `%D`  | absolute drop-file **directory** for this session              |
//! | `%F`  | absolute path of the drop **file** itself                      |
//! | `%N`  | the allocated node number                                      |
//! | `%H`  | the comm/socket handle — always `0` today: both `io_mode`s     |
//! |       | bridge the child's stdio; socket-handle inheritance is deferred |
//! | `%%`  | a literal `%`                                                  |
//!
//! Unknown `%x` pairs pass through verbatim. The same facts are exported to
//! the child's environment as `RABBITHOLE_DOOR_ID`, `RABBITHOLE_DOOR_NODE`
//! and `RABBITHOLE_DOOR_DROPDIR`.
//!
//! ## The bridge and IAC
//!
//! The remote leg here is *always* a telnet stream, so the bridge is built
//! in socket mode regardless of the door's `io_mode`: door output has its
//! `0xFF` bytes doubled before hitting the wire ([`TelnetStream::write_raw`]
//! deliberately does not escape). Inbound, the telnet layer has already
//! collapsed doubled IACs and absorbed option negotiation, so the payload is
//! re-escaped to wire form before entering the bridge — the round trip keeps
//! the bridge's byte accounting exact while feeding it the wire shapes its
//! decoder is specified against.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::anyhow;
use rabbithole_legacy_doors::{
    prepare_dropfile, BridgeBuffer, DoorContext, DoorDef, DoorRegistry, DoorSession, DoorUser,
    Emulation, IoMode, NodePool,
};
use rabbithole_legacy_telnet::proto::escape_iac;
use rabbithole_legacy_telnet::{Input, TelnetStream};
use rabbithole_server_core::{security_level, AuthedUser, Caps, ServerConfig};
use rabbithole_store_server::{doors::DoorUsageRepo, repo::AuditRepo, SqlitePool};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::{Child, ChildStdout, Command};

use crate::Shared;

/// The boot-assembled door host: validated registry, shared node pool, and
/// the working root the per-node drop directories live under.
pub struct DoorService {
    enabled: bool,
    registry: DoorRegistry,
    nodes: Arc<NodePool>,
    root: PathBuf,
    session_max_secs: u64,
    pool: SqlitePool,
}

impl DoorService {
    /// Build from config. When `doors_enabled`, every `[[doors]]` entry is
    /// validated (and duplicate ids rejected) — a misconfigured door list
    /// fails boot loudly rather than surfacing at first launch. When
    /// disabled, the list is ignored entirely.
    pub fn from_config(
        cfg: &ServerConfig,
        data_dir: &Path,
        pool: SqlitePool,
    ) -> anyhow::Result<DoorService> {
        let mut registry = DoorRegistry::new();
        if cfg.doors_enabled {
            for def in &cfg.doors {
                registry
                    .add(def.clone())
                    .map_err(|e| anyhow!("doors config: {e}"))?;
            }
        }
        Ok(DoorService {
            enabled: cfg.doors_enabled,
            registry,
            nodes: Arc::new(NodePool::new(cfg.doors_max_nodes)),
            root: crate::resolve_dir(data_dir, &cfg.doors_dir),
            session_max_secs: cfg.doors_session_max_secs,
            pool,
        })
    }

    /// Whether door hosting is switched on (`doors_enabled`).
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Installed doors in menu order (empty when disabled).
    pub fn list(&self) -> &[DoorDef] {
        self.registry.list()
    }

    /// Look up one door by id.
    pub fn get(&self, id: &str) -> Option<&DoorDef> {
        self.registry.get(id)
    }

    async fn reserve(
        &self,
        def: &DoorDef,
        account: i64,
        now: SystemTime,
    ) -> anyhow::Result<Option<UsageLease>> {
        let global =
            (self.session_max_secs > 0).then(|| Duration::from_secs(self.session_max_secs));
        let Some(minutes) = def.daily_limit_mins else {
            return Ok(Some(UsageLease::unmetered(global)));
        };
        let window = DailyWindow::at(now)?;
        let requested = global.map_or(window.remaining, |cap| cap.min(window.remaining));
        let reservation = DoorUsageRepo(&self.pool)
            .reserve(
                account,
                &def.id,
                window.day,
                u64::from(minutes) * 60_000,
                duration_ms(requested),
            )
            .await?;
        Ok(reservation.map(|reservation| UsageLease {
            reservation: Some((self.pool.clone(), reservation.id)),
            limit: Some(Duration::from_millis(reservation.granted_ms)),
            day: Some(window.day),
            started: None,
        }))
    }
}

const DAY_MILLIS: u64 = 86_400_000;

struct DailyWindow {
    day: i64,
    remaining: Duration,
}

impl DailyWindow {
    fn at(now: SystemTime) -> anyhow::Result<Self> {
        let since_epoch = now.duration_since(SystemTime::UNIX_EPOCH)?;
        let millis = u64::try_from(since_epoch.as_millis())?;
        Ok(Self {
            day: i64::try_from(millis / DAY_MILLIS)?,
            remaining: Duration::from_millis(DAY_MILLIS - millis % DAY_MILLIS),
        })
    }
}

fn duration_ms(duration: Duration) -> u64 {
    // Round up: quick exits/reconnects cannot accumulate free fractions.
    u64::try_from(duration.as_nanos().div_ceil(1_000_000)).unwrap_or(u64::MAX)
}

/// Monotonic elapsed time controls refunds; wall time is used only to choose
/// the UTC accounting window. Cancellation may attempt a refund, but a hard
/// crash or database failure never erases the durable prepaid charge.
struct UsageLease {
    reservation: Option<(SqlitePool, i64)>,
    limit: Option<Duration>,
    day: Option<i64>,
    started: Option<tokio::time::Instant>,
}

impl UsageLease {
    fn unmetered(limit: Option<Duration>) -> Self {
        Self {
            reservation: None,
            limit,
            day: None,
            started: None,
        }
    }

    /// Recheck midnight after preparing the drop file, before starting a
    /// process. Slow filesystem preparation cannot shift a prepaid run into
    /// a new accounting day. Returns false when that window has already ended.
    fn start(&mut self, now: SystemTime) -> anyhow::Result<bool> {
        if let Some(day) = self.day {
            let window = DailyWindow::at(now)?;
            if window.day != day {
                return Ok(false);
            }
            self.limit = self.limit.map(|limit| limit.min(window.remaining));
        }
        self.started = Some(tokio::time::Instant::now());
        Ok(true)
    }

    fn deadline(&self) -> Option<tokio::time::Instant> {
        self.started
            .zip(self.limit)
            .map(|(started, limit)| started + limit)
    }

    fn elapsed_ms(&self) -> u64 {
        self.started.map_or(0, |start| duration_ms(start.elapsed()))
    }

    async fn finish(&mut self) {
        if let Some((pool, id)) = self.reservation.take() {
            if let Err(error) = DoorUsageRepo(&pool).settle(id, self.elapsed_ms()).await {
                tracing::warn!(%error, "door usage settlement failed; prepaid time retained");
            }
        }
    }
}

impl Drop for UsageLease {
    fn drop(&mut self) {
        let Some((pool, id)) = self.reservation.take() else {
            return;
        };
        let elapsed = self.elapsed_ms();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Err(error) = DoorUsageRepo(&pool).settle(id, elapsed).await {
                    tracing::warn!(%error, "cancelled door usage settlement failed; prepaid time retained");
                }
            });
        }
    }
}

/// How one bridged door session came to an end.
enum Outcome {
    /// The door exited on its own with this code.
    Ended(i32),
    /// The time budget expired; the child was killed.
    TimedOut,
    /// The caller hung up (or the connection failed); the child was killed.
    Hangup,
}

/// Run door `id` for the authenticated caller, bridging its stdio onto the
/// telnet stream. Refusals (disabled, unknown, denied, pool exhausted) are
/// reported to the caller and return `Ok`; only transport failures err.
pub async fn run_door<S>(
    t: &mut TelnetStream<S>,
    shared: &Arc<Shared>,
    authed: &AuthedUser,
    id: &str,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let doors = &shared.doors;
    if !doors.enabled() {
        return t
            .write_str("\nDoors are not enabled on this system.\n")
            .await;
    }
    let Some(def) = doors.get(id).cloned() else {
        return t
            .write_str(&format!("\nNo such door: {id} (try `doors`).\n"))
            .await;
    };
    if !shared.perms.allows(
        &authed.subject,
        &format!("doors/{}", def.id),
        Caps::DOOR_RUN,
    ) {
        audit(
            shared,
            &authed.account.login,
            "door-denied",
            format!("{} via=telnet", def.id),
        );
        return t
            .write_str("\nYou do not have access to that door.\n")
            .await;
    }

    // A node from the shared pool, clamped to the door's own range. The
    // lease is RAII: every exit path below releases it on drop.
    let Ok(lease) = doors.nodes.allocate_in(def.nodes) else {
        return t
            .write_str("\nAll door nodes are busy right now. Try again later.\n")
            .await;
    };
    let node = lease.node();
    let drop_dir = doors.root.join(format!("node{node}"));
    let mut usage = match doors
        .reserve(&def, authed.account.id, SystemTime::now())
        .await
    {
        Ok(Some(usage)) => usage,
        Ok(None) => {
            return t.write_str("\nNo daily time remains for this door (including time reserved by active runs). Try again later or after midnight UTC.\n").await;
        }
        Err(error) => {
            tracing::warn!(%error, "door daily allowance could not be reserved");
            return t
                .write_str(
                    "\nThe door's daily allowance could not be checked. Please try again later.\n",
                )
                .await;
        }
    };

    let mut session = DoorSession::new(&def.id, node, &drop_dir, SystemTime::now());
    let (filename, contents) =
        prepare_dropfile(&def, &door_context(shared, t, authed, usage.limit), node);
    let dropfile = drop_dir.join(filename);
    let prepared = async {
        tokio::fs::create_dir_all(&drop_dir).await?;
        tokio::fs::write(&dropfile, contents.as_bytes()).await
    }
    .await;
    if let Err(e) = prepared {
        let _ = session.abort();
        usage.finish().await;
        audit(
            shared,
            &authed.account.login,
            "door-run",
            format!(
                "{} node={node} outcome=aborted(dropfile: {e}) via=telnet",
                def.id
            ),
        );
        return t.write_str("\nThe door failed to start.\n").await;
    }

    match usage.start(SystemTime::now()) {
        Ok(true) => {}
        _ => {
            usage.finish().await;
            return t
                .write_str(
                    "\nThe daily time window changed while preparing the door. Please try again.\n",
                )
                .await;
        }
    }
    let mut child = match spawn_door(&def, &drop_dir, &dropfile, node) {
        Ok(c) => c,
        Err(e) => {
            let _ = session.abort();
            usage.started = None; // no process ran: refund the whole reservation
            usage.finish().await;
            audit(
                shared,
                &authed.account.login,
                "door-run",
                format!(
                    "{} node={node} outcome=aborted(spawn: {e}) via=telnet",
                    def.id
                ),
            );
            return t.write_str("\nThe door failed to start.\n").await;
        }
    };
    session
        .start(SystemTime::now())
        .map_err(std::io::Error::other)?;

    // Always socket-mode: the remote leg is telnet (see the module docs).
    let mut bridge = BridgeBuffer::new(IoMode::Socket);
    let outcome = drive(
        t,
        &mut child,
        &mut bridge,
        usage.deadline(),
        &format!("\nEntering {} (node {node})...\n\n", def.title),
    )
    .await;
    usage.finish().await;

    let (label, farewell) = match outcome {
        Outcome::Ended(code) => {
            session.finish(code).map_err(std::io::Error::other)?;
            (
                format!("ended({code})"),
                Some(format!("\n\n{} ended.\n", def.title)),
            )
        }
        Outcome::TimedOut => {
            session.timeout().map_err(std::io::Error::other)?;
            (
                "timed-out".to_string(),
                Some("\n\nTime limit reached — the door was closed.\n".to_string()),
            )
        }
        Outcome::Hangup => {
            session.abort().map_err(std::io::Error::other)?;
            ("hangup".to_string(), None)
        }
    };
    let stats = bridge.stats();
    audit(
        shared,
        &authed.account.login,
        "door-run",
        format!(
            "{} node={node} outcome={label} out={}B in={}B via=telnet",
            def.id, stats.door_to_remote, stats.remote_to_door
        ),
    );
    if let Some(text) = farewell {
        t.write_str(&text).await?;
    }
    drop(lease);
    Ok(())
}

/// The deadline encloses every await in the bridge, including the entering
/// banner and backpressured writes. An inner select timer alone cannot stop a
/// child while one of that select's branches is awaiting a slow peer.
async fn drive<S>(
    t: &mut TelnetStream<S>,
    child: &mut Child,
    bridge: &mut BridgeBuffer,
    deadline: Option<tokio::time::Instant>,
    banner: &str,
) -> Outcome
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let run = async {
        if t.write_str(banner).await.is_err() {
            return Outcome::Hangup;
        }
        pump(t, child, bridge).await
    };
    let outcome = match deadline {
        Some(deadline) => tokio::time::timeout_at(deadline, run)
            .await
            .unwrap_or(Outcome::TimedOut),
        None => run.await,
    };
    if !matches!(outcome, Outcome::Ended(_)) {
        let _ = child.kill().await; // kill also reaps before any refund or menu
    }
    outcome
}

/// The bidirectional byte pump: child stdout → (bridge, IAC-doubled) →
/// telnet; telnet payload → (re-escaped, bridge) → child stdin. The enclosing
/// driver owns the deadline and kills/reaps on timeout or hangup.
async fn pump<S>(t: &mut TelnetStream<S>, child: &mut Child, bridge: &mut BridgeBuffer) -> Outcome
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut stdin = child.stdin.take();
    let mut stdout = child.stdout.take();
    let mut buf = [0u8; 4096];
    let mut wire = Vec::new();
    // The shell's `door <id>` line ended in telnet CR LF (or CR NUL);
    // `read_line` consumed up to the CR and pushed the tail byte back, so
    // the first payload chunk we see starts with that dangling terminator.
    // Swallow it once — it belongs to the menu command, not to the door.
    let mut swallow_line_tail = true;
    loop {
        tokio::select! {
            status = child.wait() => {
                // The child is gone, but its last output may still sit in
                // the pipe; drain to EOF (bounded — a lingering grandchild
                // could hold the write end open) and forward it.
                if let Some(out) = stdout.as_mut() {
                    let forward = async {
                        loop {
                            match out.read(&mut buf).await {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    wire.clear();
                                    bridge.door_to_remote(&buf[..n], &mut wire);
                                    if t.write_raw(&wire).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                    };
                    let _ = tokio::time::timeout(Duration::from_secs(2), forward).await;
                }
                let code = status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
                return Outcome::Ended(code);
            }
            read = read_stdout(&mut stdout, &mut buf) => {
                match read {
                    Some(n) => {
                        wire.clear();
                        bridge.door_to_remote(&buf[..n], &mut wire);
                        if t.write_raw(&wire).await.is_err() {
                            return Outcome::Hangup;
                        }
                    }
                    // Stdout hit EOF; stop polling it and wait for exit.
                    None => stdout = None,
                }
            }
            input = t.next_input() => {
                match input {
                    Ok(Some(Input::Data(mut data))) => {
                        if swallow_line_tail {
                            swallow_line_tail = false;
                            if data.first().is_some_and(|&b| b == b'\n' || b == 0) {
                                data.remove(0);
                            }
                        }
                        if data.is_empty() {
                            continue;
                        }
                        if let Some(si) = stdin.as_mut() {
                            // Re-escape to wire form (the telnet layer
                            // already undoubled IACs), then let the bridge
                            // collapse it back — see the module docs.
                            let rewired = escape_iac(&data);
                            wire.clear();
                            bridge.remote_to_door(&rewired, &mut wire);
                            if si.write_all(&wire).await.is_err() || si.flush().await.is_err() {
                                // The door closed its stdin; it may still be
                                // producing output, so keep pumping.
                                stdin = None;
                            }
                        }
                    }
                    // NAWS / TTYPE updates mid-door: absorbed by the stream.
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => {
                        return Outcome::Hangup;
                    }
                }
            }
        }
    }
}

/// Read from the door's stdout while it is open; pend forever after EOF so
/// the select loop stops burning cycles on a closed pipe.
async fn read_stdout(stdout: &mut Option<ChildStdout>, buf: &mut [u8]) -> Option<usize> {
    match stdout {
        Some(out) => match out.read(buf).await {
            Ok(0) | Err(_) => None,
            Ok(n) => Some(n),
        },
        None => std::future::pending().await,
    }
}

/// Spawn the door process: `%`-token-expanded argv, working dir (the drop
/// directory unless the door pins one), conventional environment, piped
/// stdio. `kill_on_drop` backstops every early-exit path.
fn spawn_door(
    def: &DoorDef,
    drop_dir: &Path,
    dropfile: &Path,
    node: u16,
) -> std::io::Result<Child> {
    let expand = |arg: &str| expand_tokens(arg, drop_dir, dropfile, node);
    let mut cmd = Command::new(expand(def.program().unwrap_or_default()));
    cmd.args(def.args().iter().map(|a| expand(a)))
        .current_dir(
            def.working_dir
                .clone()
                .unwrap_or_else(|| drop_dir.to_path_buf()),
        )
        .env("RABBITHOLE_DOOR_ID", &def.id)
        .env("RABBITHOLE_DOOR_NODE", node.to_string())
        .env("RABBITHOLE_DOOR_DROPDIR", drop_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    cmd.spawn()
}

/// Expand the `%`-tokens documented in the module docs. Unknown pairs (and
/// a trailing lone `%`) pass through verbatim.
fn expand_tokens(arg: &str, drop_dir: &Path, dropfile: &Path, node: u16) -> String {
    let mut out = String::with_capacity(arg.len());
    let mut chars = arg.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('D') => out.push_str(&drop_dir.display().to_string()),
            Some('F') => out.push_str(&dropfile.display().to_string()),
            Some('N') => out.push_str(&node.to_string()),
            Some('H') => out.push('0'),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// Project the caller onto a [`DoorContext`] for the drop file: terminal
/// size from NAWS, persona name as alias/real name, the RBAC-projected
/// security level (see [`rabbithole_server_core::security_level`] for the
/// documented role→SL table and within-role class/grant/revoke nudges), and
/// the session's effective time budget.
fn door_context<S>(
    shared: &Arc<Shared>,
    t: &TelnetStream<S>,
    authed: &AuthedUser,
    limit: Option<Duration>,
) -> DoorContext
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (cols, rows) = t.window().unwrap_or((80, 25));
    DoorContext {
        node: 1, // pinned to the real lease by prepare_dropfile
        com_port: 0,
        baud: 0,
        rows,
        cols,
        bbs_name: shared.config.read().name,
        sysop_name: "SysOp".to_string(),
        bbs_id: "RABBIT".to_string(),
        user: DoorUser {
            real_name: authed.persona.screen_name.clone(),
            alias: authed.persona.screen_name.clone(),
            location: String::new(),
            security_level: u16::from(security_level(&authed.subject)),
            time_left_mins: limit
                .map(|d| u32::try_from((d.as_secs() / 60).max(1)).unwrap_or(u32::MAX))
                .unwrap_or(60),
            emulation: Emulation::Ansi,
            is_ansi: true,
        },
        session_start: SystemTime::now(),
    }
}

/// Fire-and-forget audit record, same conventions as the native admin family.
fn audit(shared: &Arc<Shared>, actor: &str, action: &str, detail: String) {
    let pool = shared.pool.clone();
    let actor = actor.to_string();
    let action = action.to_string();
    tokio::spawn(async move {
        let _ = AuditRepo(&pool).record(&actor, &action, &detail).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabbithole_legacy_doors::{DropFile, NodeRange};

    fn def(daily: Option<u32>) -> DoorDef {
        DoorDef {
            id: "lord".into(),
            title: "LORD".into(),
            command: vec!["lord".into()],
            working_dir: None,
            dropfile: DropFile::Door32Sys,
            io_mode: IoMode::Stdio,
            nodes: NodeRange::any(),
            daily_limit_mins: daily,
        }
    }

    #[test]
    fn tokens_expand_and_escape() {
        let dir = Path::new("/srv/doors/node3");
        let file = Path::new("/srv/doors/node3/DOOR32.SYS");
        assert_eq!(expand_tokens("-n%N", dir, file, 3), "-n3".to_string());
        assert_eq!(
            expand_tokens("%D/run.sh %F", dir, file, 3),
            "/srv/doors/node3/run.sh /srv/doors/node3/DOOR32.SYS"
        );
        assert_eq!(expand_tokens("%H", dir, file, 3), "0");
        assert_eq!(expand_tokens("100%%", dir, file, 3), "100%");
        assert_eq!(expand_tokens("%x%", dir, file, 3), "%x%");
    }

    async fn fixture(secs: u64) -> (DoorService, i64) {
        let pool = rabbithole_store_server::open_in_memory().await.unwrap();
        let account = rabbithole_store_server::repo::AccountsRepo(&pool)
            .create("alice", None, "Alice", 1, None)
            .await
            .unwrap()
            .id;
        let cfg = ServerConfig {
            doors_session_max_secs: secs,
            ..ServerConfig::default()
        };
        (
            DoorService::from_config(&cfg, Path::new("."), pool).unwrap(),
            account,
        )
    }

    #[tokio::test]
    async fn prior_usage_and_global_cap_both_reduce_the_next_budget() {
        let (service, account) = fixture(600).await;
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10 * 86_400);
        let usage = DoorUsageRepo(&service.pool);
        let spent = usage
            .reserve(account, "lord", 10, 1_800_000, 1_500_000)
            .await
            .unwrap()
            .unwrap();
        usage.settle(spent.id, spent.granted_ms).await.unwrap();
        let mut lease = service
            .reserve(&def(Some(30)), account, now)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lease.limit, Some(Duration::from_secs(300)));
        lease.finish().await;
        let mut next_day = service
            .reserve(&def(Some(30)), account, now + Duration::from_secs(86_400))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            next_day.limit,
            Some(Duration::from_secs(600)),
            "new day does not remove global session cap"
        );
        next_day.finish().await;
    }

    #[tokio::test]
    async fn midnight_bounds_limited_runs_and_preparation_cannot_cross_the_day() {
        let (service, account) = fixture(600).await;
        let midnight = SystemTime::UNIX_EPOCH + Duration::from_millis(11 * DAY_MILLIS);
        let before = midnight - Duration::from_millis(900);
        let mut lease = service
            .reserve(&def(Some(1)), account, before)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lease.limit, Some(Duration::from_millis(900)));
        assert!(lease.start(before + Duration::from_millis(100)).unwrap());
        assert_eq!(lease.limit, Some(Duration::from_millis(800)));
        lease.started = Some(tokio::time::Instant::now() - Duration::from_millis(250));
        lease.finish().await;
        let charged = DoorUsageRepo(&service.pool)
            .charged_ms(account, "lord", 10)
            .await
            .unwrap();
        assert!((250..=900).contains(&charged));
        let mut expired = service
            .reserve(&def(Some(1)), account, before)
            .await
            .unwrap()
            .unwrap();
        assert!(!expired.start(midnight).unwrap());
        expired.finish().await;
        assert_eq!(
            DoorUsageRepo(&service.pool)
                .charged_ms(account, "lord", 10)
                .await
                .unwrap(),
            charged
        );
        let mut fresh = service
            .reserve(&def(Some(1)), account, midnight)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fresh.limit, Some(Duration::from_secs(60)));
        fresh.finish().await;
    }

    #[tokio::test]
    async fn unlimited_daily_doors_ignore_midnight_and_synthetic_account_ids() {
        let (service, _) = fixture(0).await;
        let midnight = SystemTime::UNIX_EPOCH + Duration::from_millis(11 * DAY_MILLIS);
        let mut lease = service
            .reserve(&def(None), -42, midnight - Duration::from_millis(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(lease.limit, None);
        assert!(lease.start(midnight).unwrap());
        assert!(lease.deadline().is_none());
        assert!(service
            .reserve(&def(Some(1)), 0, midnight)
            .await
            .unwrap()
            .is_none());
        assert!(service
            .reserve(&def(Some(1)), -42, midnight)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn cancelled_leases_refund_once_and_database_failure_refuses_admission() {
        let (service, account) = fixture(600).await;
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10 * 86_400);
        let mut lease = service
            .reserve(&def(Some(1)), account, now)
            .await
            .unwrap()
            .unwrap();
        assert!(lease.start(now).unwrap());
        lease.started = Some(tokio::time::Instant::now() - Duration::from_secs(2));
        drop(lease);
        let charged = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let charged = DoorUsageRepo(&service.pool)
                    .charged_ms(account, "lord", 10)
                    .await
                    .unwrap();
                if charged < 60_000 {
                    break charged;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!((2000..60_000).contains(&charged));
        service.pool.close().await;
        assert!(service.reserve(&def(Some(1)), account, now).await.is_err());
    }
}
