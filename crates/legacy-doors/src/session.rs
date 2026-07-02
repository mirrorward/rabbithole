//! The door-session lifecycle: [`DoorSession`], its [`SessionState`] machine,
//! and the [`prepare_dropfile`] convenience.
//!
//! A session moves through a small, strict state machine:
//!
//! ```text
//!                    start            finish
//!   Preparing ────────────► Running ─────────► Ended { exit_code }
//!       │                     │ │
//!       │ abort       timeout │ │ abort
//!       ▼                     ▼ ▼
//!    Aborted             TimedOut Aborted
//! ```
//!
//! * **Preparing** — the drop file has been (or is being) written into the
//!   session's drop directory; no process exists yet.
//! * **Running** — the driver has spawned the door process.
//! * **Ended / TimedOut / Aborted** — terminal states; every further event
//!   is rejected with [`Error::BadTransition`].
//!
//! ## Purity & the process seam
//!
//! [`DoorSession`] is a **pure FSM**: it never spawns a process, never reads
//! a clock, never touches the filesystem. Timestamps are *injected* — the
//! driving slice passes `SystemTime` values into [`DoorSession::new`] and
//! [`DoorSession::start`] — so the machine is fully deterministic and
//! testable. The drop-directory path it carries is just data: the driver
//! creates the directory, writes the [`prepare_dropfile`] output into it,
//! spawns the process, and reports the outcome back via
//! [`finish`](DoorSession::finish) / [`timeout`](DoorSession::timeout) /
//! [`abort`](DoorSession::abort).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::context::DoorContext;
use crate::door::DoorDef;
use crate::error::Error;

/// Where a [`DoorSession`] is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// Drop file written (or being written); process not yet spawned.
    Preparing,
    /// The door process is running.
    Running,
    /// The door exited on its own with this exit code. Terminal.
    Ended {
        /// The process exit code (`0` = clean exit).
        exit_code: i32,
    },
    /// The driver killed the door for exceeding its time budget. Terminal.
    TimedOut,
    /// The session was cancelled (caller hung up, sysop intervened, or
    /// preparation failed). Terminal.
    Aborted,
}

impl SessionState {
    /// Whether this state admits no further transitions.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        !matches!(self, SessionState::Preparing | SessionState::Running)
    }

    /// A short lowercase name for the state, used in error messages.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            SessionState::Preparing => "preparing",
            SessionState::Running => "running",
            SessionState::Ended { .. } => "ended",
            SessionState::TimedOut => "timed-out",
            SessionState::Aborted => "aborted",
        }
    }
}

/// One caller's trip through one door: node, drop directory, injected
/// timestamps, and the lifecycle state machine.
///
/// ```
/// use std::time::{Duration, SystemTime};
/// use rabbithole_legacy_doors::{DoorSession, SessionState};
///
/// let t0 = SystemTime::UNIX_EPOCH;
/// let mut s = DoorSession::new("lord", 2, "/tmp/doors/node2", t0);
/// assert_eq!(s.state(), SessionState::Preparing);
/// s.start(t0 + Duration::from_secs(1)).unwrap();
/// s.finish(0).unwrap();
/// assert_eq!(s.state(), SessionState::Ended { exit_code: 0 });
/// assert!(s.finish(0).is_err()); // terminal: no further transitions
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoorSession {
    door_id: String,
    node: u16,
    drop_dir: PathBuf,
    created_at: SystemTime,
    started_at: Option<SystemTime>,
    state: SessionState,
}

impl DoorSession {
    /// A fresh session in [`SessionState::Preparing`].
    ///
    /// `created_at` is injected by the caller (no ambient clock is read);
    /// `drop_dir` is the per-session directory the drop file lives in.
    #[must_use]
    pub fn new(
        door_id: impl Into<String>,
        node: u16,
        drop_dir: impl Into<PathBuf>,
        created_at: SystemTime,
    ) -> Self {
        DoorSession {
            door_id: door_id.into(),
            node,
            drop_dir: drop_dir.into(),
            created_at,
            started_at: None,
            state: SessionState::Preparing,
        }
    }

    /// Mark the door process as spawned, recording the injected start time.
    ///
    /// # Errors
    ///
    /// [`Error::BadTransition`] unless the session is in
    /// [`SessionState::Preparing`].
    pub fn start(&mut self, at: SystemTime) -> Result<(), Error> {
        match self.state {
            SessionState::Preparing => {
                self.started_at = Some(at);
                self.state = SessionState::Running;
                Ok(())
            }
            other => Err(bad(other, "start")),
        }
    }

    /// Record that the door exited on its own with `exit_code`.
    ///
    /// # Errors
    ///
    /// [`Error::BadTransition`] unless the session is in
    /// [`SessionState::Running`].
    pub fn finish(&mut self, exit_code: i32) -> Result<(), Error> {
        match self.state {
            SessionState::Running => {
                self.state = SessionState::Ended { exit_code };
                Ok(())
            }
            other => Err(bad(other, "finish")),
        }
    }

    /// Record that the driver killed the door for exceeding its time budget.
    ///
    /// # Errors
    ///
    /// [`Error::BadTransition`] unless the session is in
    /// [`SessionState::Running`] (a session that never started cannot time
    /// out — abort it instead).
    pub fn timeout(&mut self) -> Result<(), Error> {
        match self.state {
            SessionState::Running => {
                self.state = SessionState::TimedOut;
                Ok(())
            }
            other => Err(bad(other, "timeout")),
        }
    }

    /// Cancel the session (hang-up, sysop kick, or preparation failure).
    ///
    /// # Errors
    ///
    /// [`Error::BadTransition`] if the session is already terminal. Aborting
    /// is legal from both [`SessionState::Preparing`] and
    /// [`SessionState::Running`].
    pub fn abort(&mut self) -> Result<(), Error> {
        match self.state {
            SessionState::Preparing | SessionState::Running => {
                self.state = SessionState::Aborted;
                Ok(())
            }
            other => Err(bad(other, "abort")),
        }
    }

    /// The current lifecycle state.
    #[must_use]
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// Whether the session has reached a terminal state.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.state.is_terminal()
    }

    /// The id of the [`DoorDef`] this session runs.
    #[must_use]
    pub fn door_id(&self) -> &str {
        &self.door_id
    }

    /// The node number allocated to this session.
    #[must_use]
    pub fn node(&self) -> u16 {
        self.node
    }

    /// The per-session directory the drop file was written into.
    #[must_use]
    pub fn drop_dir(&self) -> &Path {
        &self.drop_dir
    }

    /// When the session was created (injected at [`DoorSession::new`]).
    #[must_use]
    pub fn created_at(&self) -> SystemTime {
        self.created_at
    }

    /// When the door process was started, if it ever was.
    #[must_use]
    pub fn started_at(&self) -> Option<SystemTime> {
        self.started_at
    }

    /// The exit code, if the session ended via [`finish`](Self::finish).
    #[must_use]
    pub fn exit_code(&self) -> Option<i32> {
        match self.state {
            SessionState::Ended { exit_code } => Some(exit_code),
            _ => None,
        }
    }

    /// Time the door has been running as of the injected `now`: `None`
    /// before [`start`](Self::start), zero if `now` is earlier than the
    /// start time (a clock that went backwards never panics or underflows).
    #[must_use]
    pub fn elapsed(&self, now: SystemTime) -> Option<Duration> {
        let started = self.started_at?;
        Some(now.duration_since(started).unwrap_or(Duration::ZERO))
    }
}

/// Build the [`Error::BadTransition`] for `event` in `state`.
fn bad(state: SessionState, event: &'static str) -> Error {
    Error::BadTransition {
        state: state.name(),
        event,
    }
}

/// Render the drop file for launching `def` on behalf of `ctx`, pinned to
/// `node`.
///
/// Returns the conventional `(filename, contents)` pair — e.g.
/// `("DOOR32.SYS", "...")` — dispatching to the existing format writers via
/// [`DropFile::write`](crate::DropFile::write). The context's node number is
/// overridden with `node` so the drop file always agrees with the
/// [`NodeLease`](crate::NodeLease) the session actually holds.
///
/// This is still pure: the driving slice joins the filename onto the
/// session's drop directory and does the actual write.
///
/// ```
/// use rabbithole_legacy_doors::{
///     prepare_dropfile, DoorContext, DoorDef, DropFile, IoMode, NodeRange,
/// };
///
/// let def = DoorDef {
///     id: "lord".into(),
///     title: "Legend of the Red Dragon".into(),
///     command: vec!["lord".into()],
///     working_dir: None,
///     dropfile: DropFile::Door32Sys,
///     io_mode: IoMode::Stdio,
///     nodes: NodeRange::any(),
///     daily_limit_mins: None,
/// };
/// let (name, contents) = prepare_dropfile(&def, &DoorContext::default(), 7);
/// assert_eq!(name, "DOOR32.SYS");
/// assert!(contents.ends_with("7\r\n")); // node is the last DOOR32.SYS line
/// ```
#[must_use]
pub fn prepare_dropfile(def: &DoorDef, ctx: &DoorContext, node: u16) -> (&'static str, String) {
    let mut ctx = ctx.clone();
    ctx.node = node;
    (def.dropfile.filename(), def.dropfile.write(&ctx))
}
