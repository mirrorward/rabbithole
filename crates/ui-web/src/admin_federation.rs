//! Peers, trusted origins and backups, as the admin console holds them, with
//! the words for each. DOM-free and host-tested.

use rabbithole_proto::admin::{
    origin_trust, peer_state, BackupEntry, BackupVerified, OriginEntry, PeerEntry,
};

pub use crate::admin_moderation::parse_hash as parse_key;
use crate::files::human_size;

/// What a peer's state is called.
pub fn peer_state_name(state: u8) -> &'static str {
    match state {
        peer_state::PENDING => "Waiting for approval",
        peer_state::DISCONNECTED => "Approved, not connected",
        peer_state::CONNECTED => "Connected",
        _ => "Unknown",
    }
}

/// The short form of a key for a row: its first and last eight hex digits.
pub fn short_key(key: &[u8; 32]) -> String {
    crate::admin_moderation::short_hash(key)
}

/// What a peer is called on its row: the name it announced, else its origin,
/// else its key.
pub fn peer_title(peer: &PeerEntry) -> String {
    let name = peer.name.trim();
    if !name.is_empty() {
        return name.to_string();
    }
    match peer.origin.as_deref().map(str::trim) {
        Some(origin) if !origin.is_empty() => origin.to_string(),
        _ => short_key(&peer.key),
    }
}

/// Why an origin's key is believed.
pub fn trust_name(trust: u8) -> &'static str {
    match trust {
        origin_trust::DIRECT_PEER => "Proven on a direct session",
        origin_trust::OPERATOR => "Pinned by an operator",
        _ => "Unknown",
    }
}

/// Whether text reads as a federation server name, by the burrow's own rule:
/// lowercase letters, digits, dots, dashes and underscores, starting and
/// ending with a letter or digit, at most 253 bytes.
pub fn origin_is_acceptable(origin: &str) -> bool {
    let bytes = origin.as_bytes();
    if bytes.is_empty() || bytes.len() > 253 {
        return false;
    }
    let alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    bytes
        .iter()
        .all(|&b| alnum(b) || matches!(b, b'-' | b'_' | b'.'))
        && alnum(bytes[0])
        && alnum(bytes[bytes.len() - 1])
}

/// A snapshot's time stamp as a person reads it: `2026-09-18T22:41:03.12Z`
/// becomes `2026-09-18 22:41 UTC`. Anything else is shown as it came.
pub fn stamp_label(rfc3339: &str) -> String {
    let b = rfc3339.as_bytes();
    if b.len() >= 16 && b[10] == b'T' && b[4] == b'-' && b[13] == b':' {
        format!("{} {} UTC", &rfc3339[..10], &rfc3339[11..16])
    } else {
        rfc3339.to_string()
    }
}

/// One line about a snapshot: what it holds and who wrote it.
pub fn backup_line(entry: &BackupEntry) -> String {
    let files = if entry.files == 1 {
        "1 file".to_string()
    } else {
        format!("{} files", entry.files)
    };
    format!(
        "{files}, {}, written by burrow {}",
        human_size(entry.total_bytes.min(i64::MAX as u64) as i64),
        entry.version
    )
}

/// What a check found, in a sentence.
pub fn check_line(checked: &BackupVerified) -> String {
    if checked.ok {
        format!(
            "Checked: every file matches its hash and the database is sound ({} files, {}).",
            checked.files,
            human_size(checked.total_bytes.min(i64::MAX as u64) as i64)
        )
    } else {
        format!("Failed the check: {}", checked.detail)
    }
}

/// Unix seconds as a UTC calendar date and time (proleptic Gregorian). For
/// the demo burrow's snapshot names; a real burrow stamps its own.
pub fn civil_utc(unix: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400) as u32;
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, secs / 3600, (secs / 60) % 60, secs % 60)
}

/// The Peers and Backups panes' state beyond what the roster holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FederationState {
    /// Every peer the burrow has met, by key.
    pub peers: Vec<PeerEntry>,
    /// The origins whose keys are believed.
    pub origins: Vec<OriginEntry>,
    /// Where snapshots go, as the burrow resolves it.
    pub backups_dir: String,
    /// The snapshots there, oldest first.
    pub backups: Vec<BackupEntry>,
    /// The latest check of each snapshot, by name.
    pub checks: Vec<BackupVerified>,
    /// The burrow's stations, as the operator's pane shows them.
    pub stations: Vec<rabbithole_proto::radio::RadioStationStatus>,
}

impl FederationState {
    /// A check came back: it replaces any earlier one for the same snapshot.
    pub fn checked(&mut self, result: BackupVerified) {
        self.checks.retain(|c| c.name != result.name);
        self.checks.push(result);
    }

    /// The latest check of `name`, if any.
    pub fn check_of(&self, name: &str) -> Option<&BackupVerified> {
        self.checks.iter().find(|c| c.name == name)
    }

    /// The snapshots listed anew; a check for one that is gone goes with it.
    pub fn listed_backups(&mut self, dir: String, backups: Vec<BackupEntry>) {
        self.checks
            .retain(|c| backups.iter().any(|b| b.name == c.name));
        self.backups_dir = dir;
        self.backups = backups;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_peer_is_named_by_what_it_said_then_its_origin_then_its_key() {
        let named = PeerEntry::new(
            [0xab; 32],
            "Grove",
            Some("grove.example".into()),
            None,
            peer_state::PENDING,
            false,
            false,
        );
        assert_eq!(peer_title(&named), "Grove");
        let origin_only = PeerEntry::new(
            [0xab; 32],
            " ",
            Some("grove.example".into()),
            None,
            peer_state::CONNECTED,
            true,
            false,
        );
        assert_eq!(peer_title(&origin_only), "grove.example");
        let bare = PeerEntry::new([0xab; 32], "", None, None, 9, false, false);
        assert_eq!(peer_title(&bare), "abababab\u{2026}abababab");
        assert_eq!(peer_state_name(peer_state::PENDING), "Waiting for approval");
        assert_eq!(peer_state_name(peer_state::CONNECTED), "Connected");
        assert_eq!(peer_state_name(9), "Unknown");
        assert_eq!(trust_name(origin_trust::OPERATOR), "Pinned by an operator");
        assert_eq!(
            trust_name(origin_trust::DIRECT_PEER),
            "Proven on a direct session"
        );
    }

    #[test]
    fn an_origin_follows_the_burrows_own_name_rule() {
        assert!(origin_is_acceptable("grove.example"));
        assert!(origin_is_acceptable("a"));
        assert!(origin_is_acceptable("warren-2_b.example"));
        assert!(!origin_is_acceptable(""));
        assert!(!origin_is_acceptable("Grove.example"));
        assert!(!origin_is_acceptable("-grove"));
        assert!(!origin_is_acceptable("grove."));
        assert!(!origin_is_acceptable("grove example"));
        assert!(!origin_is_acceptable(&"a".repeat(254)));
        assert_eq!(parse_key(&"ab".repeat(32)), Some([0xab; 32]));
    }

    #[test]
    fn a_snapshot_reads_as_a_date_and_a_line() {
        assert_eq!(
            stamp_label("2026-09-18T22:41:03.123456Z"),
            "2026-09-18 22:41 UTC"
        );
        assert_eq!(stamp_label("2026-09-18T22:41:03Z"), "2026-09-18 22:41 UTC");
        assert_eq!(stamp_label("yesterday"), "yesterday");
        let entry = BackupEntry::new("snapshot-1", "", "0.222.0", 12, 4_200_000);
        assert_eq!(
            backup_line(&entry),
            format!(
                "12 files, {}, written by burrow 0.222.0",
                human_size(4_200_000)
            )
        );
        let one = BackupEntry::new("snapshot-1", "", "0.222.0", 1, 10);
        assert!(backup_line(&one).starts_with("1 file,"));
        let good = BackupVerified::new("snapshot-1", true, "ok", 12, 4_200_000);
        assert!(check_line(&good).starts_with("Checked: every file"));
        let bad = BackupVerified::new("snapshot-1", false, "hash mismatch for burrow.db", 0, 0);
        assert_eq!(
            check_line(&bad),
            "Failed the check: hash mismatch for burrow.db"
        );
    }

    #[test]
    fn the_calendar_is_right_around_the_edges() {
        assert_eq!(civil_utc(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil_utc(951_782_400), (2000, 2, 29, 0, 0, 0));
        assert_eq!(civil_utc(1_789_771_263), (2026, 9, 18, 22, 41, 3));
        assert_eq!(civil_utc(-1), (1969, 12, 31, 23, 59, 59));
    }

    #[test]
    fn a_check_replaces_the_last_and_leaves_with_its_snapshot() {
        let mut state = FederationState::default();
        state.checked(BackupVerified::new("snapshot-1", false, "bad", 0, 0));
        state.checked(BackupVerified::new("snapshot-1", true, "ok", 3, 9));
        state.checked(BackupVerified::new("snapshot-2", true, "ok", 3, 9));
        assert_eq!(state.checks.len(), 2);
        assert!(state.check_of("snapshot-1").unwrap().ok);
        state.listed_backups(
            "/srv/backups".into(),
            vec![BackupEntry::new("snapshot-2", "", "", 3, 9)],
        );
        assert!(state.check_of("snapshot-1").is_none());
        assert!(state.check_of("snapshot-2").is_some());
        assert_eq!(state.backups_dir, "/srv/backups");
    }
}
