//! The [`DoorContext`] data model: everything a door game needs to know about
//! the current call, independent of any particular drop-file format.
//!
//! This is the neutral, in-memory representation. The per-format writers in the
//! sibling modules project it onto DOOR.SYS / DORINFO1.DEF / DOOR32.SYS, and
//! the readers parse those back into a (partial) [`DoorContext`]. Nothing here
//! spawns a process or touches a socket — that belongs to a later Wave 6 slice.

use std::time::SystemTime;

/// Terminal emulation the caller is using.
///
/// The three variants correspond to the values DOOR32.SYS understands for its
/// emulation field (`0`/`1`/`2`); richer modes (RIP, etc.) collapse to the
/// nearest of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Emulation {
    /// Plain 7-bit ASCII, no color or cursor control.
    Ascii,
    /// ANSI (ANSI.SYS / VT-style escape sequences, 16 colors).
    #[default]
    Ansi,
    /// AVATAR/0+ terminal codes (common on FidoNet-era boards).
    Avatar,
}

impl Emulation {
    /// The DOOR32.SYS numeric code for this emulation.
    #[must_use]
    pub fn to_door32(self) -> u8 {
        match self {
            Emulation::Ascii => 0,
            Emulation::Ansi => 1,
            Emulation::Avatar => 2,
        }
    }

    /// Map a DOOR32.SYS emulation code back to an [`Emulation`]. Unknown codes
    /// (RIP=3, MaxGfx=4, …) fall back to [`Emulation::Ansi`], since they are all
    /// graphics-capable.
    #[must_use]
    pub fn from_door32(code: u8) -> Emulation {
        match code {
            0 => Emulation::Ascii,
            2 => Emulation::Avatar,
            _ => Emulation::Ansi,
        }
    }
}

/// The calling user's account facts, as a door cares about them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoorUser {
    /// User's real name (e.g. `"John Q Public"`).
    pub real_name: String,
    /// User's handle / alias (e.g. `"Neo"`).
    pub alias: String,
    /// User's location, typically `"City, ST"`.
    pub location: String,
    /// Security / access level (BBS-projected 0–255).
    pub security_level: u16,
    /// Time left in this call, in minutes.
    pub time_left_mins: u32,
    /// Terminal emulation in use.
    pub emulation: Emulation,
    /// Whether ANSI is available (kept alongside `emulation` because several
    /// drop files carry a standalone ANSI yes/no flag).
    pub is_ansi: bool,
}

impl Default for DoorUser {
    fn default() -> Self {
        DoorUser {
            real_name: String::new(),
            alias: String::new(),
            location: String::new(),
            security_level: 10,
            time_left_mins: 60,
            emulation: Emulation::Ansi,
            is_ansi: true,
        }
    }
}

/// Everything a door needs for the current call.
///
/// Fields use owned `String`s so a [`DoorContext`] is fully self-contained. All
/// fields have sensible defaults via [`Default`]; `session_start` defaults to
/// the Unix epoch so default contexts are deterministic (callers set
/// [`SystemTime::now`] when building a live one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoorContext {
    /// Node number the call is running on.
    pub node: u16,
    /// Comm port; `0` means a local/stdio session (no serial/socket).
    pub com_port: u16,
    /// Line/DTE speed in bits per second (`0` for local sessions).
    pub baud: u32,
    /// Terminal height in rows.
    pub rows: u16,
    /// Terminal width in columns.
    pub cols: u16,
    /// Name of the BBS (needed by DORINFO1.DEF).
    pub bbs_name: String,
    /// SysOp's name (needed by DOOR.SYS / DORINFO1.DEF).
    pub sysop_name: String,
    /// Short BBS identifier string (the DOOR32.SYS `bbsid` field).
    pub bbs_id: String,
    /// The calling user.
    pub user: DoorUser,
    /// When this session started.
    pub session_start: SystemTime,
}

impl Default for DoorContext {
    fn default() -> Self {
        DoorContext {
            node: 1,
            com_port: 0,
            baud: 0,
            rows: 25,
            cols: 80,
            bbs_name: "RabbitHole".to_string(),
            sysop_name: "SysOp".to_string(),
            bbs_id: "RABBIT".to_string(),
            user: DoorUser::default(),
            session_start: SystemTime::UNIX_EPOCH,
        }
    }
}
