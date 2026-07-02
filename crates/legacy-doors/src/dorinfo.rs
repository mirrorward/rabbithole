//! DORINFO1.DEF — the RBBS-PC / QuickBBS drop file.
//!
//! One of the oldest drop files, born on RBBS-PC and adopted by QuickBBS and
//! the FidoNet door ecosystem. It is line-oriented ASCII with `CRLF`
//! terminators. This crate writes the widely-implemented 12-field layout (the
//! file is often quoted as "~13 lines" once the trailing terminator is
//! counted). Line numbers are 1-based:
//!
//! ```text
//!  1  BBS name
//!  2  SysOp first name
//!  3  SysOp last name
//!  4  Comm port           "COM0" = local, else "COMn"
//!  5  Baud/settings       "<baud> BAUD,N,8,1"
//!  6  Network type        0 = not networked
//!  7  Caller first name
//!  8  Caller last name
//!  9  Caller location     City, ST
//! 10  Emulation           0 = ASCII, 1 = ANSI graphics
//! 11  Security level
//! 12  Time left (minutes)
//! ```
//!
//! There is no reader: DORINFO1.DEF is a write-only announcement of the call,
//! and doors that use it do not write it back.

use crate::context::DoorContext;
use crate::util::{join_crlf, split_name};

/// Number of content lines in a DORINFO1.DEF file.
pub const LINE_COUNT: usize = 12;

/// Render `ctx` as a DORINFO1.DEF drop file (`CRLF` line endings).
#[must_use]
pub fn write_dorinfo1(ctx: &DoorContext) -> String {
    let u = &ctx.user;
    let (sysop_first, sysop_last) = split_name(&ctx.sysop_name);
    let (user_first, user_last) = split_name(&u.real_name);

    let lines = vec![
        ctx.bbs_name.clone(),                          // 1
        sysop_first,                                   // 2
        sysop_last,                                    // 3
        format!("COM{}", ctx.com_port),                // 4
        format!("{} BAUD,N,8,1", ctx.baud),            // 5
        "0".to_string(),                               // 6
        user_first,                                    // 7
        user_last,                                     // 8
        u.location.clone(),                            // 9
        if u.is_ansi { "1" } else { "0" }.to_string(), // 10
        u.security_level.to_string(),                  // 11
        u.time_left_mins.to_string(),                  // 12
    ];
    debug_assert_eq!(lines.len(), LINE_COUNT);
    join_crlf(&lines)
}
