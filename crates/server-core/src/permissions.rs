//! Authorization: roles → capability bitmask → nearest-ancestor ACLs.
//!
//! Three layers, evaluated cheaply in order (PLAN §7):
//!
//! 1. **Role** — coarse ordered tier; supplies a default mask.
//! 2. **Class + per-account overrides** — the KDX lesson: rights live on
//!    named classes; accounts add `grant_mask` / subtract `revoke_mask`.
//! 3. **ACLs** — per-resource overrides with nearest-ancestor inheritance;
//!    at the winning level, deny beats allow.
//!
//! "Hide vs deny" is two bits: a folder the principal can't [`Caps::SEE`]
//! is invisible; one they can see but not [`Caps::FILE_DOWNLOAD`] from is
//! visible-but-denied.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Capability bits. A `u64` newtype with named constants — checks are one
/// AND. Bits are allocated in blocks by domain; unused bits are reserved
/// for their wave.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caps(pub u64);

impl Caps {
    pub const NONE: Caps = Caps(0);

    // -- visibility / session (0..8)
    pub const SEE: Caps = Caps(1 << 0); // resource is visible at all
    pub const WHO: Caps = Caps(1 << 1); // may query the who-list
    pub const CANNOT_BE_KICKED: Caps = Caps(1 << 2);

    // -- chat (8..16)
    pub const CHAT_READ: Caps = Caps(1 << 8);
    pub const CHAT_SEND: Caps = Caps(1 << 9);
    pub const CHAT_CREATE_ROOM: Caps = Caps(1 << 10);
    pub const CHAT_MODERATE: Caps = Caps(1 << 11);

    // -- direct messages (16..20)
    pub const DM_SEND: Caps = Caps(1 << 16);

    // -- boards (20..28)
    pub const BOARD_READ: Caps = Caps(1 << 20);
    pub const BOARD_POST: Caps = Caps(1 << 21);
    pub const BOARD_MODERATE: Caps = Caps(1 << 22);

    // -- files (28..40)
    pub const FILE_LIST: Caps = Caps(1 << 28);
    pub const FILE_DOWNLOAD: Caps = Caps(1 << 29);
    pub const FILE_UPLOAD: Caps = Caps(1 << 30);
    pub const FILE_MANAGE: Caps = Caps(1 << 31);
    pub const DROPBOX_VIEW: Caps = Caps(1 << 32);

    // -- swarm (40..44)
    pub const SWARM_ADVERTISE: Caps = Caps(1 << 40);

    // -- admin (48..64)
    pub const USER_KICK: Caps = Caps(1 << 48);
    pub const USER_BAN: Caps = Caps(1 << 49);
    pub const ACCOUNT_ADMIN: Caps = Caps(1 << 50);
    pub const CONFIG_ADMIN: Caps = Caps(1 << 51);
    pub const BROADCAST: Caps = Caps(1 << 52);
    pub const AUDIT_READ: Caps = Caps(1 << 53);

    pub const fn union(self, other: Caps) -> Caps {
        Caps(self.0 | other.0)
    }

    pub const fn contains(self, needed: Caps) -> bool {
        self.0 & needed.0 == needed.0
    }
}

impl std::ops::BitOr for Caps {
    type Output = Caps;
    fn bitor(self, rhs: Caps) -> Caps {
        self.union(rhs)
    }
}

/// Ordered role tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Role {
    Guest = 0,
    User = 1,
    Moderator = 2,
    Admin = 3,
    Superuser = 4,
}

impl Role {
    pub fn from_ordinal(n: u8) -> Role {
        match n {
            0 => Role::Guest,
            1 => Role::User,
            2 => Role::Moderator,
            3 => Role::Admin,
            _ => Role::Superuser,
        }
    }

    /// Default capability mask for the role (the class mask layers on top).
    pub fn default_caps(self) -> Caps {
        // Guests can speak in chat (the Hotline tradition; operators can
        // revoke it on the guest class). Rate limiting is the real guard.
        let guest = Caps::SEE
            | Caps::WHO
            | Caps::CHAT_READ
            | Caps::CHAT_SEND
            | Caps::BOARD_READ
            | Caps::FILE_LIST;
        let user = guest
            | Caps::DM_SEND
            | Caps::BOARD_POST
            | Caps::FILE_DOWNLOAD
            | Caps::FILE_UPLOAD
            | Caps::CHAT_CREATE_ROOM
            | Caps::SWARM_ADVERTISE;
        let moderator = user
            | Caps::CHAT_MODERATE
            | Caps::BOARD_MODERATE
            | Caps::USER_KICK
            | Caps::DROPBOX_VIEW
            | Caps::AUDIT_READ;
        let admin = moderator
            | Caps::USER_BAN
            | Caps::ACCOUNT_ADMIN
            | Caps::CONFIG_ADMIN
            | Caps::BROADCAST
            | Caps::FILE_MANAGE;
        let superuser = Caps(u64::MAX);
        match self {
            Role::Guest => guest,
            Role::User => user,
            Role::Moderator => moderator,
            Role::Admin => admin,
            Role::Superuser => superuser,
        }
    }

    pub fn class_name(self) -> &'static str {
        match self {
            Role::Guest => "guest",
            Role::User => "member",
            Role::Moderator => "moderator",
            Role::Admin => "admin",
            Role::Superuser => "superuser",
        }
    }
}

/// Who an ACL rule applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Principal {
    Everyone,
    Role(Role),
    Class(i64),
    Account(i64),
}

/// One rule: allow these bits, deny those.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AclRule {
    pub allow: u64,
    pub deny: u64,
}

/// The in-memory ACL table, loaded from the store and mutated through
/// admin ops. Resources are `/`-separated paths; rules on `""` (the root)
/// apply to everything.
#[derive(Debug, Default, Clone)]
pub struct AclTable {
    /// resource path → rules per principal.
    rules: HashMap<String, Vec<(Principal, AclRule)>>,
}

impl AclTable {
    pub fn insert(&mut self, resource: &str, principal: Principal, rule: AclRule) {
        let entry = self.rules.entry(resource.to_string()).or_default();
        if let Some(existing) = entry.iter_mut().find(|(p, _)| *p == principal) {
            existing.1 = rule;
        } else {
            entry.push((principal, rule));
        }
    }

    /// Combined (allow, deny) of every rule at `resource` matching any of
    /// `principals`; `None` if no rule at this level matches.
    fn level_rule(&self, resource: &str, principals: &[Principal]) -> Option<AclRule> {
        let entries = self.rules.get(resource)?;
        let mut combined: Option<AclRule> = None;
        for (principal, rule) in entries {
            if principals.contains(principal) {
                let c = combined.get_or_insert_with(AclRule::default);
                c.allow |= rule.allow;
                c.deny |= rule.deny;
            }
        }
        combined
    }
}

/// The evaluator. Base masks come from role/class/account; ACLs override.
#[derive(Debug, Default)]
pub struct PermissionEvaluator {
    acl: AclTable,
    /// Bumped on every ACL mutation; cached results from older generations
    /// are discarded.
    generation: u64,
    cache: parking_lot::RwLock<HashMap<CacheKey, (u64, u64)>>,
}

/// Everything that can change the answer must be in the key: the subject's
/// identity AND its mask inputs (so an account edit can't serve stale
/// results), plus the resource.
type CacheKey = (i64, u64, u8, i64, String);

/// The identity facts evaluation needs.
#[derive(Debug, Clone, Copy)]
pub struct Subject {
    pub account_id: i64,
    pub role: Role,
    pub class_id: Option<i64>,
    pub class_mask: u64,
    pub grant_mask: u64,
    pub revoke_mask: u64,
}

impl Subject {
    /// Base mask before ACLs: role default, class mask, account overrides.
    pub fn base_caps(&self) -> u64 {
        let mut caps = self.role.default_caps().0 | self.class_mask;
        caps |= self.grant_mask;
        caps &= !self.revoke_mask;
        // Superuser overrides everything, including revokes.
        if self.role == Role::Superuser {
            caps = u64::MAX;
        }
        caps
    }

    fn principals(&self) -> Vec<Principal> {
        let mut p = vec![
            Principal::Everyone,
            Principal::Role(self.role),
            Principal::Account(self.account_id),
        ];
        if let Some(c) = self.class_id {
            p.push(Principal::Class(c));
        }
        p
    }

    fn cache_key(&self, resource: &str) -> CacheKey {
        (
            self.account_id,
            self.base_caps(),
            self.role as u8,
            self.class_id.unwrap_or(-1),
            resource.to_string(),
        )
    }
}

impl PermissionEvaluator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn replace_table(&mut self, acl: AclTable) {
        self.acl = acl;
        self.invalidate();
    }

    pub fn insert_rule(&mut self, resource: &str, principal: Principal, rule: AclRule) {
        self.acl.insert(resource, principal, rule);
        self.invalidate();
    }

    pub fn invalidate(&mut self) {
        self.generation += 1;
        self.cache.write().clear();
    }

    /// Effective capabilities of `subject` on `resource`.
    ///
    /// Walk from the resource up its ancestors; the **nearest** level with
    /// any matching rule wins, and within that level deny beats allow.
    /// Superusers bypass ACLs entirely.
    pub fn effective(&self, subject: &Subject, resource: &str) -> u64 {
        if subject.role == Role::Superuser {
            return u64::MAX;
        }
        let key = subject.cache_key(resource);
        if let Some(&(generation, caps)) = self.cache.read().get(&key) {
            if generation == self.generation {
                return caps;
            }
        }

        let mut caps = subject.base_caps();
        let principals = subject.principals();
        for level in ancestors(resource) {
            if let Some(rule) = self.acl.level_rule(level, &principals) {
                caps |= rule.allow;
                caps &= !rule.deny; // deny wins at the winning level
                break; // nearest ancestor wins; stop walking
            }
        }

        self.cache.write().insert(key, (self.generation, caps));
        caps
    }

    /// Convenience: does `subject` hold `needed` on `resource`?
    pub fn allows(&self, subject: &Subject, resource: &str, needed: Caps) -> bool {
        self.effective(subject, resource) & needed.0 == needed.0
    }
}

/// `"a/b/c"` → `["a/b/c", "a/b", "a", ""]` (most-specific first).
fn ancestors(resource: &str) -> impl Iterator<Item = &str> {
    let mut current = Some(resource);
    std::iter::from_fn(move || {
        let r = current?;
        current = if r.is_empty() {
            None
        } else {
            Some(r.rfind('/').map_or("", |i| &r[..i]))
        };
        Some(r)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subject(role: Role) -> Subject {
        Subject {
            account_id: 1,
            role,
            class_id: Some(10),
            class_mask: 0,
            grant_mask: 0,
            revoke_mask: 0,
        }
    }

    #[test]
    fn ancestors_walk_to_root() {
        let a: Vec<&str> = ancestors("a/b/c").collect();
        assert_eq!(a, vec!["a/b/c", "a/b", "a", ""]);
        let root: Vec<&str> = ancestors("").collect();
        assert_eq!(root, vec![""]);
    }

    #[test]
    fn role_defaults_are_ordered() {
        // Every capability a lower role has, higher roles keep.
        let roles = [
            Role::Guest,
            Role::User,
            Role::Moderator,
            Role::Admin,
            Role::Superuser,
        ];
        for pair in roles.windows(2) {
            let lower = pair[0].default_caps().0;
            let higher = pair[1].default_caps().0;
            assert_eq!(lower & higher, lower, "{:?} ⊄ {:?}", pair[0], pair[1]);
        }
    }

    #[test]
    fn deny_wins_at_winning_level() {
        let mut eval = PermissionEvaluator::new();
        eval.insert_rule(
            "files/private",
            Principal::Everyone,
            AclRule {
                allow: Caps::FILE_DOWNLOAD.0,
                deny: Caps::FILE_DOWNLOAD.0,
            },
        );
        let s = subject(Role::User);
        assert!(!eval.allows(&s, "files/private", Caps::FILE_DOWNLOAD));
    }

    #[test]
    fn nearest_ancestor_wins() {
        let mut eval = PermissionEvaluator::new();
        // Root denies downloads for everyone…
        eval.insert_rule(
            "",
            Principal::Everyone,
            AclRule {
                allow: 0,
                deny: Caps::FILE_DOWNLOAD.0,
            },
        );
        // …but files/public re-allows them.
        eval.insert_rule(
            "files/public",
            Principal::Everyone,
            AclRule {
                allow: Caps::FILE_DOWNLOAD.0,
                deny: 0,
            },
        );
        let s = subject(Role::User);
        assert!(eval.allows(&s, "files/public/readme.txt", Caps::FILE_DOWNLOAD));
        assert!(!eval.allows(&s, "files/other/thing.bin", Caps::FILE_DOWNLOAD));
    }

    #[test]
    fn hide_vs_deny_are_distinct_bits() {
        let mut eval = PermissionEvaluator::new();
        // Dropbox: visible, uploadable, not listable/downloadable.
        eval.insert_rule(
            "files/dropbox",
            Principal::Role(Role::User),
            AclRule {
                allow: Caps::SEE.0 | Caps::FILE_UPLOAD.0,
                deny: Caps::FILE_LIST.0 | Caps::FILE_DOWNLOAD.0,
            },
        );
        // Secret area: hidden entirely.
        eval.insert_rule(
            "files/secret",
            Principal::Role(Role::User),
            AclRule {
                allow: 0,
                deny: Caps::SEE.0,
            },
        );
        let s = subject(Role::User);
        assert!(eval.allows(&s, "files/dropbox", Caps::SEE | Caps::FILE_UPLOAD));
        assert!(!eval.allows(&s, "files/dropbox", Caps::FILE_LIST));
        assert!(!eval.allows(&s, "files/secret", Caps::SEE));
    }

    #[test]
    fn account_grant_and_revoke_masks() {
        let eval = PermissionEvaluator::new();
        // Guests can chat but cannot DM — grant/revoke around that bit.
        let mut s = subject(Role::Guest);
        assert!(!eval.allows(&s, "", Caps::DM_SEND));
        s.grant_mask = Caps::DM_SEND.0;
        assert!(eval.allows(&s, "", Caps::DM_SEND));
        s.revoke_mask = Caps::DM_SEND.0;
        assert!(!eval.allows(&s, "", Caps::DM_SEND), "revoke beats grant");
    }

    #[test]
    fn superuser_bypasses_acl_denies() {
        let mut eval = PermissionEvaluator::new();
        eval.insert_rule(
            "",
            Principal::Everyone,
            AclRule {
                allow: 0,
                deny: u64::MAX,
            },
        );
        let s = subject(Role::Superuser);
        assert!(eval.allows(&s, "anything/at/all", Caps::CONFIG_ADMIN));
    }

    #[test]
    fn cache_invalidates_on_rule_change() {
        let mut eval = PermissionEvaluator::new();
        let s = subject(Role::User);
        assert!(eval.allows(&s, "files/x", Caps::FILE_DOWNLOAD));
        eval.insert_rule(
            "files/x",
            Principal::Everyone,
            AclRule {
                allow: 0,
                deny: Caps::FILE_DOWNLOAD.0,
            },
        );
        assert!(!eval.allows(&s, "files/x", Caps::FILE_DOWNLOAD));
    }
}
