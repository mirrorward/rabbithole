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

    // -- legacy doors (44..48)
    /// May launch door games on the telnet surface (member+ by default;
    /// operators can grant it to guests or revoke it per class/account).
    pub const DOOR_RUN: Caps = Caps(1 << 44);

    // -- admin (48..64)
    pub const USER_KICK: Caps = Caps(1 << 48);
    pub const USER_BAN: Caps = Caps(1 << 49);
    pub const ACCOUNT_ADMIN: Caps = Caps(1 << 50);
    pub const CONFIG_ADMIN: Caps = Caps(1 << 51);
    pub const BROADCAST: Caps = Caps(1 << 52);
    pub const AUDIT_READ: Caps = Caps(1 << 53);
    /// The Wave 13 moderation suite: work the report queue, quarantine
    /// content for review, and manage the hash-deny list. A dedicated bit
    /// (the `DOOR_RUN` precedent) rather than a reuse of `BOARD_MODERATE`,
    /// because the queue spans posts, DMs, files, *and* users — gating it on
    /// a board-scoped bit would let a boards-only moderator act on files.
    /// Moderator+ by default; grantable/revocable per class/account.
    pub const MODERATE: Caps = Caps(1 << 54);

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
            | Caps::SWARM_ADVERTISE
            | Caps::DOOR_RUN;
        let moderator = user
            | Caps::CHAT_MODERATE
            | Caps::BOARD_MODERATE
            | Caps::USER_KICK
            | Caps::DROPBOX_VIEW
            | Caps::AUDIT_READ
            | Caps::MODERATE;
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

    /// Parse a config-facing role name (the `*_min_role` keys). Accepts
    /// `guest`, `user` (or its class alias `member`), `moderator`, and
    /// `admin` — case-insensitively. `superuser` is deliberately not a valid
    /// listener minimum: a surface only the superuser can enter is a surface
    /// switched off, and the `*_enabled` toggles already express that.
    pub fn parse_min_role(s: &str) -> Option<Role> {
        match s.trim().to_ascii_lowercase().as_str() {
            "guest" => Some(Role::Guest),
            "user" | "member" => Some(Role::User),
            "moderator" => Some(Role::Moderator),
            "admin" => Some(Role::Admin),
            _ => None,
        }
    }

    /// The canonical config-facing name for this role (the inverse of
    /// [`Role::parse_min_role`]), used in refusal messages.
    pub fn min_role_name(self) -> &'static str {
        match self {
            Role::Guest => "guest",
            Role::User => "user",
            Role::Moderator => "moderator",
            Role::Admin => "admin",
            Role::Superuser => "superuser",
        }
    }
}

// ---------------------------------------------------------------------------
// Legacy security-level projection (Wave 6 polish)
// ---------------------------------------------------------------------------

/// The participation capabilities that nudge a subject's projected security
/// level *within* its role band (see [`security_level`]). Order is fixed so
/// the projection is deterministic; each entry is worth ±2 levels.
const SL_NUDGE_CAPS: [Caps; 12] = [
    Caps::DM_SEND,
    Caps::BOARD_POST,
    Caps::FILE_DOWNLOAD,
    Caps::FILE_UPLOAD,
    Caps::CHAT_CREATE_ROOM,
    Caps::SWARM_ADVERTISE,
    Caps::DOOR_RUN,
    Caps::DROPBOX_VIEW,
    Caps::CHAT_MODERATE,
    Caps::BOARD_MODERATE,
    Caps::USER_KICK,
    Caps::FILE_MANAGE,
];

/// Project a permission [`Subject`] onto the classic 0–255 BBS security
/// level (SL) that legacy drop files (DOOR.SYS, DOOR32.SYS, DORINFO1.DEF)
/// carry. Doors use the SL for their own gating, so the projection must be
/// deterministic and must never rank a higher role below a lower one.
///
/// ## The table
///
/// | Role      | base SL | band (clamp) |
/// |-----------|---------|--------------|
/// | Guest     |  10     |  1 – 25      |
/// | User      |  30     | 26 – 70      |
/// | Moderator |  80     | 71 – 95      |
/// | Admin     | 100     | 96 – 250     |
/// | Superuser | 255     | 255 (fixed)  |
///
/// ## Within-role adjustments
///
/// Class base masks and per-account grant/revoke masks (via
/// [`Subject::base_caps`]) shift the SL inside the role's band: each
/// capability in the participation list ([`SL_NUDGE_CAPS`]) adds **+2** when
/// the subject holds it beyond its role default, and **−2** when a
/// role-default capability has been revoked. Per-resource ACLs never
/// influence the SL — the drop file describes the *caller*, not a resource.
///
/// ## Monotonicity
///
/// The bands are disjoint and ordered (guest ≤ 25 < 26 ≤ user ≤ 70 < 71 ≤
/// moderator ≤ 95 < 96 ≤ admin ≤ 250 < 255), and adjustments are clamped
/// into the band, so a maximally-granted lower role always projects strictly
/// below a maximally-revoked higher role. Superusers are always 255,
/// regardless of masks (mirroring their bypass in the evaluator).
pub fn security_level(subject: &Subject) -> u8 {
    if subject.role == Role::Superuser {
        return 255;
    }
    let (base, floor, ceil): (i32, i32, i32) = match subject.role {
        Role::Guest => (10, 1, 25),
        Role::User => (30, 26, 70),
        Role::Moderator => (80, 71, 95),
        Role::Admin => (100, 96, 250),
        Role::Superuser => unreachable!("handled above"),
    };
    let defaults = subject.role.default_caps().0;
    let held = subject.base_caps();
    let mut level = base;
    for cap in SL_NUDGE_CAPS {
        let by_default = defaults & cap.0 == cap.0;
        let in_hand = held & cap.0 == cap.0;
        match (by_default, in_hand) {
            (false, true) => level += 2, // granted beyond the role
            (true, false) => level -= 2, // revoked from the role
            _ => {}
        }
    }
    // The clamp bounds are small positive constants, so the cast is exact.
    level.clamp(floor, ceil) as u8
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
    fn security_level_table_for_plain_roles() {
        // A subject with no class/account mask tweaks sits on the role base.
        assert_eq!(security_level(&subject(Role::Guest)), 10);
        assert_eq!(security_level(&subject(Role::User)), 30);
        assert_eq!(security_level(&subject(Role::Moderator)), 80);
        assert_eq!(security_level(&subject(Role::Admin)), 100);
        assert_eq!(security_level(&subject(Role::Superuser)), 255);
    }

    #[test]
    fn security_level_grants_nudge_up_revokes_nudge_down() {
        // A guest granted DM_SEND (not a guest default) gains +2.
        let mut g = subject(Role::Guest);
        g.grant_mask = Caps::DM_SEND.0;
        assert_eq!(security_level(&g), 12);
        // Two extra participation caps: +4.
        g.grant_mask |= Caps::DOOR_RUN.0;
        assert_eq!(security_level(&g), 14);

        // A member with DOOR_RUN revoked loses 2.
        let mut u = subject(Role::User);
        u.revoke_mask = Caps::DOOR_RUN.0;
        assert_eq!(security_level(&u), 28);

        // Class masks count the same way as grants.
        let mut c = subject(Role::Guest);
        c.class_mask = Caps::BOARD_POST.0;
        assert_eq!(security_level(&c), 12);
    }

    #[test]
    fn security_level_clamps_to_the_role_band() {
        // A guest granted everything still tops out below the user floor.
        let mut g = subject(Role::Guest);
        g.grant_mask = u64::MAX;
        assert_eq!(security_level(&g), 25);
        // A user with everything revoked still bottoms out above it.
        let mut u = subject(Role::User);
        u.revoke_mask = u64::MAX;
        assert_eq!(security_level(&u), 26);
        assert!(security_level(&g) < security_level(&u));
    }

    #[test]
    fn security_level_is_monotonic_across_roles() {
        let roles = [
            Role::Guest,
            Role::User,
            Role::Moderator,
            Role::Admin,
            Role::Superuser,
        ];
        // For any mask shape, a higher role never projects a lower SL — even
        // comparing a fully-granted lower role to a fully-revoked higher one.
        let shapes: [(u64, u64, u64); 5] = [
            (0, 0, 0),
            (u64::MAX, 0, 0),
            (0, u64::MAX, 0),
            (0, 0, u64::MAX),
            (Caps::DOOR_RUN.0, Caps::DM_SEND.0, Caps::FILE_UPLOAD.0),
        ];
        for (lo_shape, hi_shape) in shapes
            .iter()
            .flat_map(|a| shapes.iter().map(move |b| (a, b)))
        {
            for pair in roles.windows(2) {
                let mut lo = subject(pair[0]);
                (lo.class_mask, lo.grant_mask, lo.revoke_mask) = *lo_shape;
                let mut hi = subject(pair[1]);
                (hi.class_mask, hi.grant_mask, hi.revoke_mask) = *hi_shape;
                assert!(
                    security_level(&lo) < security_level(&hi),
                    "{:?}{lo_shape:?} vs {:?}{hi_shape:?}",
                    pair[0],
                    pair[1],
                );
            }
        }
    }

    #[test]
    fn security_level_superuser_ignores_masks() {
        let mut s = subject(Role::Superuser);
        s.revoke_mask = u64::MAX;
        assert_eq!(security_level(&s), 255);
    }

    #[test]
    fn min_role_names_round_trip() {
        for role in [Role::Guest, Role::User, Role::Moderator, Role::Admin] {
            assert_eq!(Role::parse_min_role(role.min_role_name()), Some(role));
        }
        assert_eq!(Role::parse_min_role("member"), Some(Role::User));
        assert_eq!(Role::parse_min_role(" Admin "), Some(Role::Admin));
        assert_eq!(Role::parse_min_role("superuser"), None);
        assert_eq!(Role::parse_min_role("wizard"), None);
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
