//! Pure, DOM-free web-admin state and its event reducer.
//!
//! Like [`crate::state`] and [`crate::files`], this module holds **no** Leptos
//! or `web_sys` types so the reducer is unit-tested on the host with
//! `cargo test`. The admin components in [`crate::components`] own a reactive
//! `RwSignal<AdminState>` and fold [`AdminEvent`]s into it via
//! [`AdminState::apply`].
//!
//! Class and account rows are reused straight from
//! [`rabbithole_proto::admin`] rather than re-modelled, so the wire types and
//! the view stay in lockstep. The one view-local shape is [`ConfigEntry`]: a
//! flat key/value pair accumulated from `ConfigGet` reads.

/// The server config keys the console loads on entry — the ones an operator
/// reaches for: what the place is called and says, who may join, the chat
/// and transfer limits, and how it is advertised. Names are the server's own
/// (`Config::get_key`/`set_key`); the demo mock seeds the same keys.
pub const OPERATOR_KEYS: &[&str] = &[
    "name",
    "motd",
    "agreement",
    "registration_mode",
    "guest_enabled",
    "chat_max_len",
    "upload_quota_bytes",
    "max_concurrent_transfers",
    "transfer_rate_bytes_per_sec",
    "ws_public_url",
    "advertise_host",
    "announce_enabled",
    "announce_description",
    "announce_sysop",
];

use rabbithole_proto::admin::{AccountEntry, ClassEntry, InviteCode};

use crate::wire::AdminEvent;

/// One resolved config key/value pair, accumulated from reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigEntry {
    /// Config key.
    pub key: String,
    /// Current value.
    pub value: String,
}

/// The full, flat web-admin UI model. `Default` is the empty state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdminState {
    /// Permission classes.
    pub classes: Vec<ClassEntry>,
    /// The current page of accounts.
    pub accounts: Vec<AccountEntry>,
    /// What an operator has typed to find one.
    pub account_find: String,
    /// What each board keeps, once this burrow has said. A board listing
    /// carries neither number, so until the answer lands the console knows
    /// nothing — which is not the same as a burrow that cannot say.
    pub board_keeping: Keeping,
    /// Total accounts across all pages.
    pub account_total: u64,
    /// Resolved config key/value pairs.
    pub config: Vec<ConfigEntry>,
    /// The most recently minted invite code, if any.
    pub last_invite: Option<InviteCode>,
    /// One-line status/error line for the console.
    pub status: String,
}

/// What the console knows about board retention on the burrow it is
/// looking at. Asked and unanswered is its own state: saying "this burrow
/// does not say" while the question is still in flight is a lie the
/// operator would act on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Keeping {
    /// Asked, no answer yet.
    #[default]
    Asking,
    /// What the burrow said: `(threads kept, threads it holds now)` by slug.
    Said(std::collections::BTreeMap<String, (u32, u64)>),
    /// The burrow will not say — older than the question.
    Cannot,
}

impl Keeping {
    /// What this burrow said about one board, if it said anything.
    pub fn board(&self, slug: &str) -> Option<(u32, u64)> {
        match self {
            Keeping::Said(boards) => boards.get(slug).copied(),
            _ => None,
        }
    }

    /// The listing, as the burrow gave it.
    pub fn said(boards: &[rabbithole_proto::board::BoardKept]) -> Self {
        Keeping::Said(
            boards
                .iter()
                .map(|b| (b.slug.clone(), (b.max_threads, b.threads)))
                .collect(),
        )
    }
}

/// What a board keeps, said for a person: the limit, and how many threads
/// are there now.
pub fn keeping_line(kept: &Keeping, slug: &str) -> String {
    let kept = match kept {
        Keeping::Asking => return "Asking this burrow what this board keeps\u{2026}".to_string(),
        Keeping::Cannot => return "This burrow does not say what this board keeps.".to_string(),
        said => said.board(slug),
    };
    let here = |now: u64| match now {
        1 => "1 thread here now.".to_string(),
        n => format!("{n} threads here now."),
    };
    match kept {
        None => "This burrow did not mention this board.".to_string(),
        Some((0, now)) => format!("Every thread is kept. {}", here(now)),
        Some((1, now)) => format!(
            "Only the newest thread is kept; the rest go as new ones start. {}",
            here(now)
        ),
        Some((max, now)) => format!(
            "The newest {max} threads are kept; the rest go as new ones start. {}",
            here(now)
        ),
    }
}

impl AdminState {
    /// Fold a single [`AdminEvent`] into the state. Unknown
    /// (`#[non_exhaustive]`) events are ignored.
    pub fn apply(&mut self, event: &AdminEvent) {
        match event {
            AdminEvent::ClassesListed(classes) => self.classes = classes.clone(),
            AdminEvent::AccountsListed { accounts, total } => {
                self.accounts = accounts.clone();
                self.account_total = *total;
            }
            AdminEvent::InviteCreated(code) => {
                self.status = format!("Invite {} created.", code.code);
                self.last_invite = Some(code.clone());
            }
            AdminEvent::ConfigLoaded { key, value } => self.upsert_config(key, value),
            // The described settings live in [`crate::admin_settings`].
            AdminEvent::ConfigDescribed(_)
            | AdminEvent::SurfacesReported(_)
            | AdminEvent::InvitesListed(_)
            | AdminEvent::ReportsListed(..)
            | AdminEvent::HeldListed(..)
            | AdminEvent::DenyHashesListed(_)
            | AdminEvent::AuditListed(_)
            | AdminEvent::PeersListed(_)
            | AdminEvent::OriginsListed(_)
            | AdminEvent::BackupsListed(..)
            | AdminEvent::StationsListed(_)
            | AdminEvent::BackupMade(_)
            | AdminEvent::BackupChecked(_) => {}
            // What boards keep has one write path, in
            // [`crate::app::AppState::fold_admin_reply`], where the reply
            // can be checked against the burrow it was asked of. Folding it
            // here as well would put another burrow's answer in this
            // burrow's console.
            AdminEvent::BoardKeepingListed(_) => {}
            AdminEvent::ConfigApplied { applied_live } => {
                self.status = if *applied_live {
                    "Config saved and applied live.".to_string()
                } else {
                    "Config saved; a restart is required to apply it.".to_string()
                };
            }
            AdminEvent::Ack(msg) => self.status = msg.clone(),
            AdminEvent::Failed(detail) => self.status = format!("Error: {detail}"),
            // Live feed/gateway counters belong to the syndication panel.
            AdminEvent::GatewayStatsLoaded(_) => {}
            AdminEvent::ThemeBundleApplied(info) => {
                self.status = if info.present {
                    format!("Published theme {}.", info.name)
                } else {
                    "Theme cleared.".to_string()
                };
            }
        }
    }

    /// Insert or replace a config pair keyed by `key`.
    fn upsert_config(&mut self, key: &str, value: &str) {
        if let Some(slot) = self.config.iter_mut().find(|c| c.key == key) {
            slot.value = value.to_string();
        } else {
            self.config.push(ConfigEntry {
                key: key.to_string(),
                value: value.to_string(),
            });
        }
    }

    /// The value currently held for `key`, if it has been read.
    pub fn config_value(&self, key: &str) -> Option<&str> {
        self.config
            .iter()
            .find(|c| c.key == key)
            .map(|c| c.value.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabbithole_proto::admin::ThemeBundleInfo;

    #[test]
    fn classes_and_accounts_replace_state() {
        let mut s = AdminState::default();
        s.apply(&AdminEvent::ClassesListed(vec![ClassEntry::new(
            "admin", 0xFF, 1,
        )]));
        assert_eq!(s.classes.len(), 1);
        s.apply(&AdminEvent::AccountsListed {
            accounts: vec![AccountEntry::new(1, "alice", 1, None, false)],
            total: 7,
        });
        assert_eq!(s.accounts.len(), 1);
        assert_eq!(s.account_total, 7);
        // A second listing replaces, not appends.
        s.apply(&AdminEvent::AccountsListed {
            accounts: vec![
                AccountEntry::new(2, "bob", 1, None, false),
                AccountEntry::new(3, "carol", 1, None, true),
            ],
            total: 7,
        });
        assert_eq!(s.accounts.len(), 2);
        assert_eq!(s.accounts[0].login, "bob");
    }

    #[test]
    fn config_reads_upsert_by_key() {
        let mut s = AdminState::default();
        s.apply(&AdminEvent::ConfigLoaded {
            key: "server.name".into(),
            value: "Rabbit Lobby".into(),
        });
        assert_eq!(s.config_value("server.name"), Some("Rabbit Lobby"));
        // Re-reading the same key updates in place.
        s.apply(&AdminEvent::ConfigLoaded {
            key: "server.name".into(),
            value: "New Warren".into(),
        });
        assert_eq!(s.config.len(), 1);
        assert_eq!(s.config_value("server.name"), Some("New Warren"));
        // An unknown key is appended.
        s.apply(&AdminEvent::ConfigLoaded {
            key: "server.motd".into(),
            value: "hi".into(),
        });
        assert_eq!(s.config.len(), 2);
    }

    #[test]
    fn config_applied_reports_live_vs_restart() {
        let mut s = AdminState::default();
        s.apply(&AdminEvent::ConfigApplied { applied_live: true });
        assert!(s.status.contains("applied live"));
        s.apply(&AdminEvent::ConfigApplied {
            applied_live: false,
        });
        assert!(s.status.contains("restart"));
    }

    #[test]
    fn invite_created_records_code_and_status() {
        let mut s = AdminState::default();
        s.apply(&AdminEvent::InviteCreated(InviteCode::new("ABC123", 42)));
        assert_eq!(s.last_invite.as_ref().unwrap().code, "ABC123");
        assert!(s.status.contains("ABC123"));
    }

    #[test]
    fn ack_and_failure_surface_on_status() {
        let mut s = AdminState::default();
        s.apply(&AdminEvent::Ack("Broadcast sent.".into()));
        assert_eq!(s.status, "Broadcast sent.");
        s.apply(&AdminEvent::Failed("nope".into()));
        assert!(s.status.contains("nope"));
    }

    #[test]
    fn published_theme_names_itself_on_status() {
        let mut s = AdminState::default();
        let mut info = ThemeBundleInfo::default();
        info.present = true;
        info.name = "Wonderland".into();
        s.apply(&AdminEvent::ThemeBundleApplied(info));
        assert_eq!(s.status, "Published theme Wonderland.");
    }

    #[test]
    fn a_keeping_listing_is_not_folded_by_the_plain_reducer() {
        // Every live session folds admin events into the one console model
        // before anything checks which burrow answered. What boards keep is
        // therefore written in one guarded place only, and must not be
        // written here: see `AppState::fold_admin_reply`.
        let mut s = AdminState::default();
        s.apply(&AdminEvent::BoardKeepingListed(vec![
            rabbithole_proto::board::BoardKept::new("b", 7, 3),
        ]));
        assert_eq!(s.board_keeping, Keeping::Asking);
    }

    #[test]
    fn what_a_board_keeps_is_said_the_way_a_person_would() {
        let said = |max, now| {
            Keeping::Said(std::collections::BTreeMap::from([(
                "b".to_string(),
                (max, now),
            )]))
        };
        assert_eq!(
            keeping_line(&said(0, 1), "b"),
            "Every thread is kept. 1 thread here now."
        );
        assert_eq!(
            keeping_line(&said(0, 12), "b"),
            "Every thread is kept. 12 threads here now."
        );
        assert!(keeping_line(&said(1, 3), "b").starts_with("Only the newest thread is kept"));
        assert!(keeping_line(&said(50, 3), "b").starts_with("The newest 50 threads are kept"));
        // Asked and unanswered, a burrow that cannot say, and one that
        // answered without this board are three different things.
        assert!(keeping_line(&Keeping::Asking, "b").starts_with("Asking"));
        assert!(keeping_line(&Keeping::Cannot, "b").contains("does not say"));
        assert!(keeping_line(&said(1, 1), "other").contains("did not mention"));
    }
}
