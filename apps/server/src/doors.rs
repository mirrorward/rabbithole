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
use rabbithole_server_core::{AuthedUser, Caps, Role, ServerConfig};
use rabbithole_store_server::repo::AuditRepo;
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
}

impl DoorService {
    /// Build from config. When `doors_enabled`, every `[[doors]]` entry is
    /// validated (and duplicate ids rejected) — a misconfigured door list
    /// fails boot loudly rather than surfacing at first launch. When
    /// disabled, the list is ignored entirely.
    pub fn from_config(cfg: &ServerConfig, data_dir: &Path) -> anyhow::Result<DoorService> {
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

    /// Effective wall-clock budget for one session of `def`: the smaller of
    /// the global `doors_session_max_secs` cap and the door's own
    /// `daily_limit_mins`. `None` = unlimited.
    fn time_limit(&self, def: &DoorDef) -> Option<Duration> {
        let global =
            (self.session_max_secs > 0).then(|| Duration::from_secs(self.session_max_secs));
        let per_door = def
            .daily_limit_mins
            .map(|m| Duration::from_secs(u64::from(m) * 60));
        match (global, per_door) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
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
    let limit = doors.time_limit(&def);

    let mut session = DoorSession::new(&def.id, node, &drop_dir, SystemTime::now());
    let (filename, contents) =
        prepare_dropfile(&def, &door_context(shared, t, authed, limit), node);
    let dropfile = drop_dir.join(filename);
    let prepared = async {
        tokio::fs::create_dir_all(&drop_dir).await?;
        tokio::fs::write(&dropfile, contents.as_bytes()).await
    }
    .await;
    if let Err(e) = prepared {
        let _ = session.abort();
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

    let mut child = match spawn_door(&def, &drop_dir, &dropfile, node) {
        Ok(c) => c,
        Err(e) => {
            let _ = session.abort();
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
    t.write_str(&format!("\nEntering {} (node {node})...\n\n", def.title))
        .await?;

    // Always socket-mode: the remote leg is telnet (see the module docs).
    let mut bridge = BridgeBuffer::new(IoMode::Socket);
    let outcome = pump(t, &mut child, &mut bridge, limit).await;

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

/// The bidirectional byte pump: child stdout → (bridge, IAC-doubled) →
/// telnet; telnet payload → (re-escaped, bridge) → child stdin. Ends on
/// child exit, caller hangup, or the time budget expiring — the child is
/// killed (and reaped) on the latter two.
async fn pump<S>(
    t: &mut TelnetStream<S>,
    child: &mut Child,
    bridge: &mut BridgeBuffer,
    limit: Option<Duration>,
) -> Outcome
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
    // A far-future default keeps the deadline arm inert when unlimited
    // (roughly a year — no live telnet call outlasts it).
    let deadline = tokio::time::sleep(limit.unwrap_or(Duration::from_secs(365 * 24 * 3600)));
    tokio::pin!(deadline);
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
                            let _ = child.kill().await;
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
                        let _ = child.kill().await;
                        return Outcome::Hangup;
                    }
                }
            }
            _ = &mut deadline => {
                let _ = child.kill().await;
                return Outcome::TimedOut;
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
/// size from NAWS, persona name as alias/real name, role-derived security
/// level, and the session's effective time budget.
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
            security_level: security_level(authed.subject.role),
            time_left_mins: limit
                .map(|d| u32::try_from((d.as_secs() / 60).max(1)).unwrap_or(u32::MAX))
                .unwrap_or(60),
            emulation: Emulation::Ansi,
            is_ansi: true,
        },
        session_start: SystemTime::now(),
    }
}

/// The classic 0–255 BBS security level a role projects to.
fn security_level(role: Role) -> u16 {
    match role {
        Role::Guest => 10,
        Role::User => 30,
        Role::Moderator => 60,
        Role::Admin => 90,
        Role::Superuser => 100,
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

    #[test]
    fn time_limit_takes_the_smaller_budget() {
        let svc = |secs: u64| DoorService {
            enabled: true,
            registry: DoorRegistry::new(),
            nodes: Arc::new(NodePool::new(1)),
            root: PathBuf::from("."),
            session_max_secs: secs,
        };
        // Global 3600s vs door 30min → 30min wins.
        assert_eq!(
            svc(3600).time_limit(&def(Some(30))),
            Some(Duration::from_secs(1800))
        );
        // Global 600s vs door 30min → global wins.
        assert_eq!(
            svc(600).time_limit(&def(Some(30))),
            Some(Duration::from_secs(600))
        );
        // Unlimited global, unlimited door.
        assert_eq!(svc(0).time_limit(&def(None)), None);
        // Unlimited global, door budget applies.
        assert_eq!(
            svc(0).time_limit(&def(Some(1))),
            Some(Duration::from_secs(60))
        );
    }
}
