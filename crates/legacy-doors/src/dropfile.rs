//! Format-agnostic dispatch: the [`DropFile`] enum plus [`write`] and
//! [`detect`] helpers.
//!
//! Use [`DropFile`] when the concrete format is chosen at runtime (e.g. from a
//! per-door config value), and [`detect`] to sniff which format a buffer holds
//! when reading one back.

use crate::context::DoorContext;
use crate::door32::write_door32_sys;
use crate::door_sys::write_door_sys;
use crate::dorinfo::write_dorinfo1;
use crate::util::split_lines;

/// The classic door drop-file formats this crate can produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DropFile {
    /// GAP DOOR.SYS (52 lines).
    DoorSys,
    /// RBBS/QuickBBS DORINFO1.DEF.
    DorInfo1,
    /// Mystic/EleBBS DOOR32.SYS (11 lines).
    Door32Sys,
}

impl DropFile {
    /// The conventional on-disk filename for this format.
    #[must_use]
    pub fn filename(self) -> &'static str {
        match self {
            DropFile::DoorSys => "DOOR.SYS",
            DropFile::DorInfo1 => "DORINFO1.DEF",
            DropFile::Door32Sys => "DOOR32.SYS",
        }
    }

    /// Render `ctx` in this format. See also the free [`write`] function.
    #[must_use]
    pub fn write(self, ctx: &DoorContext) -> String {
        write(self, ctx)
    }
}

/// Render `ctx` in the requested `kind`.
#[must_use]
pub fn write(kind: DropFile, ctx: &DoorContext) -> String {
    match kind {
        DropFile::DoorSys => write_door_sys(ctx),
        DropFile::DorInfo1 => write_dorinfo1(ctx),
        DropFile::Door32Sys => write_door32_sys(ctx),
    }
}

/// Best-effort sniff of which drop-file format `bytes` holds.
///
/// Returns `None` for empty input or anything that matches no known shape.
/// The checks are ordered most- to least-specific:
///
/// * DOOR.SYS — first line looks like `COMn:` and the file is long.
/// * DORINFO1.DEF — the baud line (line 5) contains `BAUD`.
/// * DOOR32.SYS — the first line is a small integer (the comm type).
#[must_use]
pub fn detect(bytes: &[u8]) -> Option<DropFile> {
    let text = std::str::from_utf8(bytes).ok()?;
    let lines = split_lines(text);
    let first = lines.first().map(|s| s.trim()).unwrap_or_default();
    if first.is_empty() {
        return None;
    }

    if first.to_ascii_uppercase().starts_with("COM") && lines.len() >= 40 {
        return Some(DropFile::DoorSys);
    }
    if lines
        .get(4)
        .is_some_and(|l| l.to_ascii_uppercase().contains("BAUD"))
    {
        return Some(DropFile::DorInfo1);
    }
    if first.parse::<u8>().is_ok() {
        return Some(DropFile::Door32Sys);
    }
    None
}
