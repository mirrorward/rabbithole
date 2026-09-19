//! Moderation, as the admin console holds it: the report queue, the sessions
//! a moderator can end, the hash-deny list and the audit log, with the words
//! for each. DOM-free and host-tested.

use rabbithole_proto::admin::{
    report_action, report_state, subject_kind, AuditEntry, DenyHashEntry, ReportEntry,
};

/// What a report is about, as a sentence fragment.
pub fn subject_name(kind: u8) -> &'static str {
    match kind {
        subject_kind::POST => "a post",
        subject_kind::DM => "a direct message",
        subject_kind::FILE => "a file",
        subject_kind::USER => "a person",
        _ => "something",
    }
}

/// The report's subject, as a short reference a moderator can act on: a
/// person by name, a file by its id, a post or message by the start of its
/// hex id.
pub fn subject_ref(kind: u8, subject: &[u8]) -> String {
    match kind {
        subject_kind::USER => String::from_utf8_lossy(subject).to_string(),
        subject_kind::FILE if subject.len() == 8 => {
            let mut b = [0u8; 8];
            b.copy_from_slice(subject);
            format!("file #{}", i64::from_le_bytes(b))
        }
        _ => {
            let hex = hex::encode(subject);
            if hex.len() > 12 {
                format!("{}\u{2026}", &hex[..12])
            } else {
                hex
            }
        }
    }
}

/// The queue's four states, in the order they are shown, with labels.
pub const STATES: [(u8, &str); 4] = [
    (report_state::OPEN, "Open"),
    (report_state::REVIEWING, "Being looked at"),
    (report_state::RESOLVED, "Resolved"),
    (report_state::DISMISSED, "Dismissed"),
];

/// What a state is called.
pub fn state_name(state: u8) -> &'static str {
    STATES
        .iter()
        .find(|(s, _)| *s == state)
        .map(|(_, n)| *n)
        .unwrap_or("Unknown")
}

/// The actions a moderator can take on a report in `state`, as
/// `(action, label)`: claim an open one, then resolve or dismiss it.
pub fn actions_for(state: u8) -> &'static [(u8, &'static str)] {
    match state {
        report_state::OPEN => &[
            (report_action::CLAIM, "Look into it"),
            (report_action::RESOLVE, "Resolve"),
            (report_action::DISMISS, "Dismiss"),
        ],
        report_state::REVIEWING => &[
            (report_action::RESOLVE, "Resolve"),
            (report_action::DISMISS, "Dismiss"),
        ],
        _ => &[],
    }
}

/// One line about a report for its row.
pub fn report_line(report: &ReportEntry) -> String {
    format!(
        "{} about {}, {}",
        subject_name(report.subject_kind),
        subject_ref(report.subject_kind, &report.subject_ref),
        report.reason.trim()
    )
}

/// A blake3 content id as a moderator types it: 64 hex characters, case
/// and surrounding space forgiven.
pub fn parse_hash(text: &str) -> Option<[u8; 32]> {
    let text = text.trim();
    if text.len() != 64 {
        return None;
    }
    let bytes = hex::decode(text).ok()?;
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&bytes);
    Some(hash)
}

/// The short form of a denied hash for its row.
pub fn short_hash(hash: &[u8; 32]) -> String {
    let hex = hex::encode(hash);
    format!("{}\u{2026}{}", &hex[..8], &hex[56..])
}

/// One line of the audit log, as a person reads it.
pub fn audit_line(entry: &AuditEntry) -> String {
    let what = match entry.action.as_str() {
        "config-set" => format!("changed a setting: {}", entry.detail),
        "account-create" => format!("made the account {}", entry.detail),
        "account-set" => format!("changed an account: {}", entry.detail),
        "account-password-set" => format!("gave {} a new password", entry.detail),
        "account-totp-reset" => format!("removed two-factor from {}", entry.detail),
        "class-set" => format!("changed a class: {}", entry.detail),
        "invite-create" => format!("made the invitation {}", entry.detail),
        "invite-revoke" => format!("withdrew the invitation {}", entry.detail),
        "kick" => format!("kicked {}", entry.detail),
        "broadcast" => format!("broadcast: {}", entry.detail),
        "theme-set" => format!("published the theme {}", entry.detail),
        "theme-clear" => "cleared the theme".to_string(),
        "board-update" => format!("changed the board {}", entry.detail),
        "board-delete" => format!("removed the board {}", entry.detail),
        other => format!("{other}: {}", entry.detail),
    };
    format!("{} {}", entry.actor, what.trim_end_matches(": "))
}

/// The moderation pane's state beyond what [`crate::admin::AdminState`] and
/// the session roster hold.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModerationState {
    /// The reports under the filter last asked for.
    pub reports: Vec<ReportEntry>,
    /// How many there are in all under that filter.
    pub total: u64,
    /// The hash-deny list.
    pub deny: Vec<DenyHashEntry>,
    /// The audit log, oldest first.
    pub audit: Vec<AuditEntry>,
}

/// A deny entry's own description for its row.
pub fn deny_line(entry: &DenyHashEntry) -> String {
    let why = entry.reason.trim();
    if why.is_empty() {
        format!("denied by {}", entry.added_by)
    } else {
        format!("{why} (denied by {})", entry.added_by)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_says_what_it_is_about_in_a_line() {
        let post = ReportEntry::new(
            1,
            3,
            subject_kind::POST,
            vec![0xab; 32],
            "Spam.",
            0,
            report_state::OPEN,
            "",
            None,
            "",
        );
        assert_eq!(
            report_line(&post),
            "a post about abababababab\u{2026}, Spam."
        );
        let person = ReportEntry::new(
            2,
            3,
            subject_kind::USER,
            b"dormouse".to_vec(),
            " Rude. ",
            0,
            report_state::OPEN,
            "",
            None,
            "",
        );
        assert_eq!(report_line(&person), "a person about dormouse, Rude.");
        assert_eq!(
            subject_ref(subject_kind::FILE, &42i64.to_le_bytes()),
            "file #42"
        );
        assert_eq!(subject_name(9), "something");
    }

    #[test]
    fn the_queue_offers_the_right_actions_for_each_state() {
        let labels = |state| {
            actions_for(state)
                .iter()
                .map(|(_, l)| *l)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            labels(report_state::OPEN),
            ["Look into it", "Resolve", "Dismiss"]
        );
        assert_eq!(labels(report_state::REVIEWING), ["Resolve", "Dismiss"]);
        assert!(labels(report_state::RESOLVED).is_empty());
        assert!(labels(report_state::DISMISSED).is_empty());
        assert_eq!(state_name(report_state::REVIEWING), "Being looked at");
        assert_eq!(STATES.len(), 4);
    }

    #[test]
    fn a_hash_is_taken_as_typed_and_shown_short() {
        let hex = "ab".repeat(32);
        let hash = parse_hash(&format!("  {}  ", hex.to_uppercase())).unwrap();
        assert_eq!(hash, [0xab; 32]);
        assert!(parse_hash("abc").is_none());
        assert!(parse_hash(&"zz".repeat(32)).is_none());
        assert_eq!(short_hash(&hash), "abababab\u{2026}abababab");
    }

    #[test]
    fn the_audit_log_reads_as_sentences() {
        let e = |action: &str, detail: &str| AuditEntry::new(0, "ada", action, detail);
        assert_eq!(
            audit_line(&e("config-set", "motd=Hello")),
            "ada changed a setting: motd=Hello"
        );
        assert_eq!(
            audit_line(&e("kick", "session 12")),
            "ada kicked session 12"
        );
        assert_eq!(audit_line(&e("theme-clear", "")), "ada cleared the theme");
        assert_eq!(
            audit_line(&e("something-new", "x=1")),
            "ada something-new: x=1"
        );
        let denied = DenyHashEntry::new([1; 32], "", "mo", 0);
        assert_eq!(deny_line(&denied), "denied by mo");
        let denied = DenyHashEntry::new([1; 32], "malware", "mo", 0);
        assert_eq!(deny_line(&denied), "malware (denied by mo)");
    }
}
