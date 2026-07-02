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
    /// Total accounts across all pages.
    pub account_total: u64,
    /// Resolved config key/value pairs.
    pub config: Vec<ConfigEntry>,
    /// The most recently minted invite code, if any.
    pub last_invite: Option<InviteCode>,
    /// One-line status/error line for the console.
    pub status: String,
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
            AdminEvent::ConfigApplied { applied_live } => {
                self.status = if *applied_live {
                    "Config saved and applied live.".to_string()
                } else {
                    "Config saved; a restart is required to apply it.".to_string()
                };
            }
            AdminEvent::Ack(msg) => self.status = msg.clone(),
            AdminEvent::Failed(detail) => self.status = format!("Error: {detail}"),
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
        self.status = format!("Loaded {key}.");
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
}
