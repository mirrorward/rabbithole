//! DOOR32.SYS — the Mystic / EleBBS 11-line drop file.
//!
//! The modern standard for telnet-era boards. Exactly 11 lines, `CRLF`
//! terminated, one field per line (1-based):
//!
//! ```text
//!  1  Comm type           0 = local, 1 = serial, 2 = telnet
//!  2  Comm / socket handle (0 for local)
//!  3  Baud rate           (0 for local)
//!  4  BBS id / software name
//!  5  User record position (in the user file)
//!  6  User real name
//!  7  User handle / alias
//!  8  Security level
//!  9  Time left (minutes)
//! 10  Emulation           0 = ASCII, 1 = ANSI, 2 = AVATAR, 3 = RIP, 4 = MaxGfx
//! 11  Node number
//! ```
//!
//! On write, `com_port == 0` maps to comm type `0` (local) with a `0` handle;
//! any non-zero port maps to comm type `1` (serial) with the port as its
//! handle. A later slice that owns real sockets will set comm type `2`.

use crate::context::{DoorContext, Emulation};
use crate::error::Error;
use crate::util::{join_crlf, split_lines};

/// Number of lines in a valid DOOR32.SYS file.
pub const LINE_COUNT: usize = 11;

/// Render `ctx` as an 11-line DOOR32.SYS drop file (`CRLF` line endings).
#[must_use]
pub fn write_door32_sys(ctx: &DoorContext) -> String {
    let u = &ctx.user;
    let (comm_type, handle) = if ctx.com_port == 0 {
        (0u8, 0u16)
    } else {
        (1u8, ctx.com_port)
    };

    let lines = vec![
        comm_type.to_string(),               // 1
        handle.to_string(),                  // 2
        ctx.baud.to_string(),                // 3
        ctx.bbs_id.clone(),                  // 4
        "0".to_string(),                     // 5 user record position
        u.real_name.clone(),                 // 6
        u.alias.clone(),                     // 7
        u.security_level.to_string(),        // 8
        u.time_left_mins.to_string(),        // 9
        u.emulation.to_door32().to_string(), // 10
        ctx.node.to_string(),                // 11
    ];
    debug_assert_eq!(lines.len(), LINE_COUNT);
    join_crlf(&lines)
}

/// Parse a DOOR32.SYS file back into a partial [`DoorContext`].
///
/// Best-effort: unparseable or missing fields keep their [`Default`] values.
/// The only error is [`Error::Empty`]. Never panics on truncated/malformed
/// input. The comm handle (line 2) is used to recover `com_port`.
pub fn read_door32_sys(text: &str) -> Result<DoorContext, Error> {
    let lines = split_lines(text);
    if lines.iter().all(|l| l.trim().is_empty()) {
        return Err(Error::Empty);
    }
    let mut ctx = DoorContext::default();
    let get = |i: usize| lines.get(i).map(|s| s.trim());

    if let Some(v) = get(1).and_then(|s| s.parse().ok()) {
        ctx.com_port = v;
    }
    if let Some(v) = get(2).and_then(|s| s.parse().ok()) {
        ctx.baud = v;
    }
    if let Some(v) = get(3) {
        ctx.bbs_id = v.to_string();
    }
    if let Some(v) = get(5) {
        ctx.user.real_name = v.to_string();
    }
    if let Some(v) = get(6) {
        ctx.user.alias = v.to_string();
    }
    if let Some(v) = get(7).and_then(|s| s.parse().ok()) {
        ctx.user.security_level = v;
    }
    if let Some(v) = get(8).and_then(|s| s.parse().ok()) {
        ctx.user.time_left_mins = v;
    }
    if let Some(code) = get(9).and_then(|s| s.parse::<u8>().ok()) {
        ctx.user.emulation = Emulation::from_door32(code);
        ctx.user.is_ansi = code != 0;
    }
    if let Some(v) = get(10).and_then(|s| s.parse().ok()) {
        ctx.node = v;
    }
    Ok(ctx)
}
