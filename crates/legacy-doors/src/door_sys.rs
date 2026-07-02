//! DOOR.SYS — the canonical **GAP Communications** 52-line drop file.
//!
//! DOOR.SYS is the most widely supported drop file: one field per line, 52
//! lines, `CRLF`-terminated. Line numbers below are 1-based (as the format is
//! traditionally documented):
//!
//! ```text
//!  1  Comm port           "COM0:" = local, else "COMn:"
//!  2  Baud rate           (DCE) e.g. 38400
//!  3  Data bits           usually 8
//!  4  Node number
//!  5  Locked (DTE) rate   e.g. 38400
//!  6  Screen (snoop)      Y/N
//!  7  Printer toggle      Y/N
//!  8  Page bell           Y/N
//!  9  Caller alarm        Y/N
//! 10  User full (real) name
//! 11  User location       City, ST
//! 12  Home / voice phone
//! 13  Work / data phone
//! 14  Password            (never emitted — blank)
//! 15  Security level
//! 16  Total calls to date
//! 17  Last call date      MM/DD/YY
//! 18  Seconds left this call
//! 19  Minutes left this call
//! 20  Graphics mode       "GR" (ANSI) / "NG" (none)
//! 21  Page length (rows)
//! 22  Expert mode         Y/N
//! 23  Conferences registered in
//! 24  Conference exited to door from
//! 25  Expiration date     MM/DD/YY
//! 26  User record number
//! 27  Default protocol    e.g. Z
//! 28  Total uploads
//! 29  Total downloads
//! 30  Daily download K total
//! 31  Daily download K max
//! 32  Birth date          MM/DD/YY
//! 33  Path to user file
//! 34  Path to GEN directory
//! 35  SysOp's name
//! 36  User's handle / alias
//! 37  Next event time     HH:MM
//! 38  Error-correcting connection  Y/N
//! 39  ANSI available      Y/N
//! 40  Record locking      Y/N
//! 41  Default text color  (attribute number)
//! 42  Time credits (minutes)
//! 43  Last new-file scan date  MM/DD/YY
//! 44  Time of this call   HH:MM
//! 45  Time of last call   HH:MM
//! 46  Max files per day
//! 47  Files downloaded today
//! 48  Total K uploaded
//! 49  Total K downloaded
//! 50  User comment
//! 51  Total doors opened
//! 52  Total messages left
//! ```

use crate::context::{DoorContext, Emulation};
use crate::datetime::{date_mmddyy, time_hhmm};
use crate::error::Error;
use crate::util::{join_crlf, parse_com, split_lines};

/// Number of lines in a valid DOOR.SYS file.
pub const LINE_COUNT: usize = 52;

/// Render `ctx` as a 52-line DOOR.SYS drop file (`CRLF` line endings).
#[must_use]
pub fn write_door_sys(ctx: &DoorContext) -> String {
    let u = &ctx.user;
    let com = if ctx.com_port == 0 {
        "COM0:".to_string()
    } else {
        format!("COM{}:", ctx.com_port)
    };
    let secs_left = u.time_left_mins.saturating_mul(60);
    let yn = |b: bool| if b { "Y" } else { "N" };

    let lines = vec![
        com,                                             // 1
        ctx.baud.to_string(),                            // 2
        "8".to_string(),                                 // 3
        ctx.node.to_string(),                            // 4
        ctx.baud.to_string(),                            // 5
        "Y".to_string(),                                 // 6
        "N".to_string(),                                 // 7
        "N".to_string(),                                 // 8
        "N".to_string(),                                 // 9
        u.real_name.clone(),                             // 10
        u.location.clone(),                              // 11
        String::new(),                                   // 12
        String::new(),                                   // 13
        String::new(),                                   // 14 password
        u.security_level.to_string(),                    // 15
        "0".to_string(),                                 // 16
        date_mmddyy(ctx.session_start),                  // 17
        secs_left.to_string(),                           // 18
        u.time_left_mins.to_string(),                    // 19
        if u.is_ansi { "GR" } else { "NG" }.to_string(), // 20
        ctx.rows.to_string(),                            // 21
        "Y".to_string(),                                 // 22
        String::new(),                                   // 23
        "0".to_string(),                                 // 24
        String::new(),                                   // 25
        "0".to_string(),                                 // 26
        "Z".to_string(),                                 // 27
        "0".to_string(),                                 // 28
        "0".to_string(),                                 // 29
        "0".to_string(),                                 // 30
        "0".to_string(),                                 // 31
        String::new(),                                   // 32
        String::new(),                                   // 33
        String::new(),                                   // 34
        ctx.sysop_name.clone(),                          // 35
        u.alias.clone(),                                 // 36
        "00:00".to_string(),                             // 37
        "Y".to_string(),                                 // 38
        yn(u.is_ansi).to_string(),                       // 39
        "N".to_string(),                                 // 40
        "7".to_string(),                                 // 41
        u.time_left_mins.to_string(),                    // 42
        String::new(),                                   // 43
        time_hhmm(ctx.session_start),                    // 44
        "00:00".to_string(),                             // 45
        "0".to_string(),                                 // 46
        "0".to_string(),                                 // 47
        "0".to_string(),                                 // 48
        "0".to_string(),                                 // 49
        String::new(),                                   // 50
        "0".to_string(),                                 // 51
        "0".to_string(),                                 // 52
    ];
    debug_assert_eq!(lines.len(), LINE_COUNT);
    join_crlf(&lines)
}

/// Parse a DOOR.SYS file back into a partial [`DoorContext`].
///
/// Best-effort: fields the format does not carry, or that fail to parse, keep
/// their [`Default`] values. The only error is [`Error::Empty`] for input with
/// no content. Never panics on truncated or malformed data.
pub fn read_door_sys(text: &str) -> Result<DoorContext, Error> {
    let lines = split_lines(text);
    if lines.iter().all(|l| l.trim().is_empty()) {
        return Err(Error::Empty);
    }
    let mut ctx = DoorContext::default();
    let get = |i: usize| lines.get(i).map(|s| s.trim());

    if let Some(v) = get(0) {
        ctx.com_port = parse_com(v);
    }
    if let Some(v) = get(1).and_then(|s| s.parse().ok()) {
        ctx.baud = v;
    }
    if let Some(v) = get(3).and_then(|s| s.parse().ok()) {
        ctx.node = v;
    }
    if let Some(v) = get(9) {
        ctx.user.real_name = v.to_string();
    }
    if let Some(v) = get(10) {
        ctx.user.location = v.to_string();
    }
    if let Some(v) = get(14).and_then(|s| s.parse().ok()) {
        ctx.user.security_level = v;
    }
    if let Some(v) = get(18).and_then(|s| s.parse().ok()) {
        ctx.user.time_left_mins = v;
    }
    if let Some(v) = get(20).and_then(|s| s.parse().ok()) {
        ctx.rows = v;
    }
    if let Some(v) = get(34) {
        ctx.sysop_name = v.to_string();
    }
    if let Some(v) = get(35) {
        ctx.user.alias = v.to_string();
    }
    if let Some(v) = get(38) {
        ctx.user.is_ansi = v.eq_ignore_ascii_case("Y");
        ctx.user.emulation = if ctx.user.is_ansi {
            Emulation::Ansi
        } else {
            Emulation::Ascii
        };
    }
    Ok(ctx)
}
