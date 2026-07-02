//! ARCmail **bundle naming** — day-coded compressed-mail bundle file names.
//!
//! Echomail is not sent as raw `.PKT` files; the scanner compresses one or more
//! packets into a *mail bundle* (historically ARC, later ZIP, but the naming
//! convention stuck). A bundle's name encodes the sender→receiver address
//! difference and the day of the week it was created:
//!
//! ```text
//!   <diff>.<weekday><seq>
//!   └─┬──┘  └──┬───┘└─┬─┘
//!     │        │      └ sequence 0-9 then a-z (up to 36 bundles / day)
//!     │        └ two-letter weekday: mo tu we th fr sa su
//!     └ 8 hex chars: (origNet-destNet):(origNode-destNode), wrapping u16
//! ```
//!
//! The **diff** is `format!("{:04x}{:04x}", origNet - destNet, origNode -
//! destNode)` with each subtraction taken modulo 2^16 (so a downlink with a
//! higher number wraps rather than underflowing) — this is the classic
//! FrontDoor/Binkley bundle basename. Two bundles created for the same link on
//! the same weekday collide on the base+weekday; the **sequence** character
//! disambiguates them, counting `0..9` then `a..z`. [`next_bundle_name`] picks
//! the lowest free sequence given the names already present, matching how a real
//! tosser renames around collisions.
//!
//! Everything here is a pure string computation — no clock (the caller supplies
//! the [`Weekday`]) and no filesystem (the caller supplies the existing names).

use std::collections::HashSet;

use crate::address::FtnAddress;

/// Day of the week, selecting the two-letter bundle extension prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Weekday {
    /// Monday (`mo`).
    Monday,
    /// Tuesday (`tu`).
    Tuesday,
    /// Wednesday (`we`).
    Wednesday,
    /// Thursday (`th`).
    Thursday,
    /// Friday (`fr`).
    Friday,
    /// Saturday (`sa`).
    Saturday,
    /// Sunday (`su`).
    Sunday,
}

impl Weekday {
    /// The two-letter weekday code used in the bundle extension.
    pub fn code(self) -> &'static str {
        match self {
            Weekday::Monday => "mo",
            Weekday::Tuesday => "tu",
            Weekday::Wednesday => "we",
            Weekday::Thursday => "th",
            Weekday::Friday => "fr",
            Weekday::Saturday => "sa",
            Weekday::Sunday => "su",
        }
    }
}

/// Maximum number of bundles that can be named for one link on one weekday:
/// sequence characters `0..9` (10) plus `a..z` (26).
pub const MAX_BUNDLES_PER_DAY: u8 = 36;

/// The 8-hex-digit bundle basename for a sender/receiver pair.
///
/// Computed as the wrapping `u16` difference of the net and node numbers, so it
/// is symmetric-ish and never panics on ordering. Zones/points are not encoded
/// (bundles are per net/node link).
pub fn bundle_basename(orig: &FtnAddress, dest: &FtnAddress) -> String {
    let net = orig.net.wrapping_sub(dest.net);
    let node = orig.node.wrapping_sub(dest.node);
    format!("{net:04x}{node:04x}")
}

/// Map a sequence index `0..36` to its extension character (`0..9`, then
/// `a..z`). Returns `None` once the 36 per-day slots are exhausted.
pub fn sequence_char(seq: u8) -> Option<char> {
    match seq {
        0..=9 => Some((b'0' + seq) as char),
        10..=35 => Some((b'a' + (seq - 10)) as char),
        _ => None,
    }
}

/// The full bundle name for a link, weekday, and sequence index, e.g.
/// `011801d0.mo0`. Returns `None` if `seq >= 36`.
pub fn bundle_name(orig: &FtnAddress, dest: &FtnAddress, day: Weekday, seq: u8) -> Option<String> {
    let ch = sequence_char(seq)?;
    Some(format!(
        "{}.{}{}",
        bundle_basename(orig, dest),
        day.code(),
        ch
    ))
}

/// Choose the lowest-sequence bundle name for `(orig, dest, day)` that is not
/// already present in `existing`.
///
/// This is the collision handler a scanner runs when queueing a new bundle:
/// starting at sequence `0`, it returns the first name free of the set of
/// filenames already in the outbound directory. Comparison is case-insensitive
/// (DOS filesystems were), so `011801D0.MO0` in `existing` blocks `...mo0`.
/// Returns `None` only when all 36 daily slots are taken.
pub fn next_bundle_name(
    orig: &FtnAddress,
    dest: &FtnAddress,
    day: Weekday,
    existing: &[String],
) -> Option<String> {
    let taken: HashSet<String> = existing.iter().map(|s| s.to_ascii_lowercase()).collect();
    (0..MAX_BUNDLES_PER_DAY)
        .filter_map(|seq| bundle_name(orig, dest, day, seq))
        .find(|name| !taken.contains(&name.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(net: u16, node: u16) -> FtnAddress {
        FtnAddress::new(2, net, node, 0)
    }

    #[test]
    fn basename_is_wrapping_hex_diff() {
        // 280-1 = 279 = 0x0117, 464-1 = 463 = 0x01cf
        assert_eq!(bundle_basename(&addr(280, 464), &addr(1, 1)), "011701cf");
        // Underflow wraps rather than panicking: 1-280 = -279 -> 0xfee9.
        assert_eq!(bundle_basename(&addr(1, 1), &addr(280, 464)), "fee9fe31");
    }

    #[test]
    fn sequence_chars_span_0_to_z() {
        assert_eq!(sequence_char(0), Some('0'));
        assert_eq!(sequence_char(9), Some('9'));
        assert_eq!(sequence_char(10), Some('a'));
        assert_eq!(sequence_char(35), Some('z'));
        assert_eq!(sequence_char(36), None);
    }

    #[test]
    fn bundle_name_shape() {
        let n = bundle_name(&addr(280, 464), &addr(1, 1), Weekday::Monday, 0).unwrap();
        assert_eq!(n, "011701cf.mo0");
        let n2 = bundle_name(&addr(280, 464), &addr(1, 1), Weekday::Friday, 10).unwrap();
        assert_eq!(n2, "011701cf.fra");
        assert!(bundle_name(&addr(280, 464), &addr(1, 1), Weekday::Sunday, 36).is_none());
    }

    #[test]
    fn all_weekday_codes() {
        for (d, c) in [
            (Weekday::Monday, "mo"),
            (Weekday::Tuesday, "tu"),
            (Weekday::Wednesday, "we"),
            (Weekday::Thursday, "th"),
            (Weekday::Friday, "fr"),
            (Weekday::Saturday, "sa"),
            (Weekday::Sunday, "su"),
        ] {
            assert_eq!(d.code(), c);
        }
    }

    #[test]
    fn next_name_skips_taken_slots() {
        let o = addr(280, 464);
        let d = addr(1, 1);
        let existing = vec![
            "011701cf.mo0".to_string(),
            "011701CF.MO1".to_string(), // uppercase must still collide
        ];
        let name = next_bundle_name(&o, &d, Weekday::Monday, &existing).unwrap();
        assert_eq!(name, "011701cf.mo2");
    }

    #[test]
    fn next_name_first_slot_when_empty() {
        let name = next_bundle_name(&addr(280, 464), &addr(1, 1), Weekday::Tuesday, &[]).unwrap();
        assert_eq!(name, "011701cf.tu0");
    }

    #[test]
    fn next_name_exhausts_after_36() {
        let o = addr(280, 464);
        let d = addr(1, 1);
        let existing: Vec<String> = (0..MAX_BUNDLES_PER_DAY)
            .map(|s| bundle_name(&o, &d, Weekday::Wednesday, s).unwrap())
            .collect();
        assert!(next_bundle_name(&o, &d, Weekday::Wednesday, &existing).is_none());
    }
}
