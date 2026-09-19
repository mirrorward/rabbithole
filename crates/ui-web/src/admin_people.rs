//! People, as the admin console holds them: what each capability is called,
//! what an operator may do to whom, and what came of the last thing they did.
//! DOM-free and host-tested.
//!
//! The burrow enforces every rule here (`apps/server/src/handlers15.rs`). The
//! console mirrors them so it never *offers* what would be refused: a control
//! that is always going to fail is a lie about what the operator can do.

use std::collections::BTreeSet;

use rabbithole_proto::admin::{AccountEntry, InviteEntry};

use crate::wire::AdminEvent;

/// The `Role` ordinals, as the burrow numbers them.
pub const ROLES: [(u8, &str, &str); 5] = [
    (0, "Guest", "Looks around. No direct messages, no uploads."),
    (1, "Member", "An ordinary account."),
    (
        2,
        "Moderator",
        "Keeps order: removes posts, works the report queue.",
    ),
    (
        3,
        "Admin",
        "Runs the burrow: settings, accounts, everything here.",
    ),
    (
        4,
        "Superuser",
        "The owner. Holds every capability and answers to nobody.",
    ),
];

/// What a role is called.
pub fn role_name(role: u8) -> &'static str {
    ROLES
        .iter()
        .find(|(n, ..)| *n == role)
        .map(|(_, name, _)| *name)
        .unwrap_or("Unknown")
}

const SUPERUSER: u8 = 4;

/// May an operator of `my_role` change this account at all? Below yourself,
/// never yourself; a superuser is exempt from the ordering only.
pub fn may_manage(my_role: u8, my_login: &str, account: &AccountEntry) -> bool {
    if account.login == my_login {
        return false;
    }
    my_role == SUPERUSER || account.role < my_role
}

/// Why not, in the operator's words.
pub fn why_not(my_role: u8, my_login: &str, account: &AccountEntry) -> Option<&'static str> {
    if account.login == my_login {
        Some("This is you. Your own account is changed from your profile, not from here.")
    } else if !may_manage(my_role, my_login, account) {
        Some("You can only change accounts below your own role.")
    } else {
        None
    }
}

/// The roles an operator of `my_role` may hand out: up to their own.
pub fn assignable_roles(my_role: u8) -> Vec<(u8, &'static str)> {
    ROLES
        .iter()
        .filter(|(n, ..)| my_role == SUPERUSER || *n <= my_role)
        .map(|(n, name, _)| (*n, *name))
        .collect()
}

/// One capability: its bit, what it is called, and what holding it allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cap {
    /// The bit index (`1 << bit`), as the burrow defines it.
    pub bit: u8,
    /// What it is called.
    pub name: &'static str,
    /// What holding it allows.
    pub help: &'static str,
}

/// Capabilities that belong together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapGroup {
    /// The heading.
    pub title: &'static str,
    /// Its capabilities.
    pub caps: &'static [Cap],
}

const fn cap(bit: u8, name: &'static str, help: &'static str) -> Cap {
    Cap { bit, name, help }
}

/// Every capability a class can carry, grouped the way an operator thinks
/// about them. A host test holds the bits to the server's own.
pub const CAPS: &[CapGroup] = &[
    CapGroup {
        title: "Being here",
        caps: &[
            cap(
                0,
                "See the burrow",
                "Without this, nothing is visible at all.",
            ),
            cap(1, "See who is on", "Ask for the list of people connected."),
            cap(
                2,
                "Cannot be kicked",
                "Moderators cannot remove this person\u{2019}s session.",
            ),
        ],
    },
    CapGroup {
        title: "Chat and messages",
        caps: &[
            cap(8, "Read chat", "See what is said in rooms."),
            cap(9, "Speak in chat", "Say things in rooms."),
            cap(10, "Make rooms", "Create new chat rooms."),
            cap(11, "Moderate chat", "Kick and mute people in any room."),
            cap(16, "Send direct messages", "Write to one person privately."),
        ],
    },
    CapGroup {
        title: "Boards",
        caps: &[
            cap(20, "Read boards", "Read posts and threads."),
            cap(21, "Post to boards", "Start threads and reply."),
            cap(
                22,
                "Moderate boards",
                "Create boards, and edit or remove anyone\u{2019}s posts.",
            ),
        ],
    },
    CapGroup {
        title: "Files",
        caps: &[
            cap(28, "Browse files", "See what is in the file areas."),
            cap(29, "Download", "Fetch files, from the burrow or its swarm."),
            cap(30, "Upload", "Add files, within the upload quota."),
            cap(
                31,
                "Manage files",
                "Create areas and folders, and remove or describe anyone\u{2019}s files.",
            ),
            cap(
                32,
                "Open drop boxes",
                "See what people have left in drop boxes.",
            ),
            cap(
                40,
                "Offer files to the swarm",
                "Tell the burrow which files this person can share.",
            ),
        ],
    },
    CapGroup {
        title: "Gateways",
        caps: &[cap(
            44,
            "Run door games",
            "Launch doors from the telnet BBS.",
        )],
    },
    CapGroup {
        title: "Running the burrow",
        caps: &[
            cap(
                48,
                "Kick sessions",
                "Disconnect someone below their own role.",
            ),
            cap(49, "Ban people", "Reserved: nothing checks this yet."),
            cap(
                50,
                "Manage accounts",
                "Everything on this page: accounts, classes, invitations.",
            ),
            cap(
                51,
                "Change settings",
                "Every setting in this console, and the theme.",
            ),
            cap(52, "Broadcast", "Send a notice to everyone connected."),
            cap(
                53,
                "Read the audit log",
                "Reserved: nothing checks this yet.",
            ),
            cap(
                54,
                "Moderate content",
                "Work the report queue, quarantine content, and manage the deny list.",
            ),
        ],
    },
];

/// Whether `mask` carries `cap`.
pub fn has(mask: u64, cap: &Cap) -> bool {
    mask & (1u64 << cap.bit) != 0
}

/// `mask` with `cap` switched on or off.
pub fn with(mask: u64, cap: &Cap, on: bool) -> u64 {
    if on {
        mask | (1u64 << cap.bit)
    } else {
        mask & !(1u64 << cap.bit)
    }
}

/// Bits in `mask` that no entry of [`CAPS`] names: kept as they are when a
/// class is edited, so a newer burrow's capabilities are not wiped by an older
/// console.
pub fn unnamed_bits(mask: u64) -> u64 {
    let named = CAPS
        .iter()
        .flat_map(|g| g.caps.iter())
        .fold(0u64, |m, c| m | (1u64 << c.bit));
    mask & !named
}

/// A password an operator can hand to someone: long, typeable, and from the
/// platform's own randomness (`bytes`), with the look-alikes left out.
pub fn password_from(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut out = String::new();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && i % 5 == 0 {
            out.push('-');
        }
        out.push(ALPHABET[*b as usize % ALPHABET.len()] as char);
    }
    out
}

/// How an invitation stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InviteStatus {
    /// Still good. The seconds it has left.
    Open(i64),
    /// Someone used it.
    UsedBy(String),
    /// Nobody did, and now nobody can.
    Expired,
}

/// How `invite` stands at `now` (unix seconds).
pub fn invite_status(invite: &InviteEntry, now: i64) -> InviteStatus {
    match &invite.used_by {
        Some(who) => InviteStatus::UsedBy(who.clone()),
        None if invite.expires_at > now => InviteStatus::Open(invite.expires_at - now),
        None => InviteStatus::Expired,
    }
}

/// What to reload after an action, because it changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reload {
    /// Nothing.
    Nothing,
    /// The account list.
    Accounts,
    /// The class list.
    Classes,
    /// The invitations.
    Invites,
}

/// The pane's state beyond the lists [`crate::admin::AdminState`] holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeopleState {
    /// Every invitation, newest first.
    pub invites: Vec<InviteEntry>,
    /// What came of the last action: whether it worked, and a sentence.
    pub notice: Option<(bool, String)>,
    /// The login the last successful create was for, so the form can clear.
    pub created: Option<String>,
    busy: BTreeSet<String>,
}

impl PeopleState {
    /// An action was sent. `tag` is its [`crate::wire::AdminCommand::tag`].
    pub fn sent(&mut self, tag: &str) {
        self.busy.insert(tag.to_string());
        self.notice = None;
    }

    /// Whether the action `tag` is waiting on the burrow.
    pub fn is_busy(&self, tag: &str) -> bool {
        self.busy.contains(tag)
    }

    /// Fold the reply to the action `tag`. Returns what to reload.
    pub fn apply(&mut self, tag: &str, events: &[AdminEvent]) -> Reload {
        self.busy.remove(tag);
        let (kind, subject) = tag
            .strip_prefix('*')
            .map(|t| t.split_once(':').unwrap_or((t, "")))
            .unwrap_or(("", ""));
        let mut reload = Reload::Nothing;
        for event in events {
            match event {
                AdminEvent::InvitesListed(invites) => self.invites = invites.clone(),
                AdminEvent::InviteCreated(_) => reload = Reload::Invites,
                AdminEvent::Ack(_) => {
                    let (text, then) = succeeded(kind, subject);
                    if kind == "account-create" {
                        self.created = Some(subject.to_string());
                    }
                    if !text.is_empty() {
                        self.notice = Some((true, text));
                    }
                    reload = then;
                }
                AdminEvent::Failed(detail) => {
                    self.notice = Some((false, refused(kind, subject, detail)));
                    // A stale list is the usual reason an action is refused.
                    reload = match kind {
                        "invite-revoke" => Reload::Invites,
                        "account-set" | "account-password" | "account-totp" => Reload::Accounts,
                        _ => Reload::Nothing,
                    };
                }
                _ => {}
            }
        }
        reload
    }
}

fn succeeded(kind: &str, subject: &str) -> (String, Reload) {
    match kind {
        "account-create" => (format!("Made the account {subject}."), Reload::Accounts),
        "account-password" => (
            format!("{subject} has a new password, and was signed out everywhere."),
            Reload::Nothing,
        ),
        "account-totp" => (
            format!(
                "Two-factor was removed from {subject}. They can sign in with their password \
                 and set it up again."
            ),
            Reload::Nothing,
        ),
        "account-set" => (format!("Saved {subject}."), Reload::Accounts),
        "class-set" => (format!("Saved the {subject} class."), Reload::Classes),
        "invite-revoke" => ("Withdrew the invitation.".to_string(), Reload::Invites),
        _ => (String::new(), Reload::Nothing),
    }
}

/// A refusal, in the operator's words. `detail` is what the wire layer made of
/// the error frame (`server error: Forbidden`).
fn refused(kind: &str, subject: &str, detail: &str) -> String {
    let code = |name: &str| detail.contains(name);
    match kind {
        "account-create" if code("AlreadyExists") => {
            format!("The login {subject} is taken, by an account or by someone\u{2019}s persona.")
        }
        "account-create" if code("BadRequest") => "A login has no spaces and at most 32 \
             characters, and a password needs at least 8."
            .to_string(),
        "account-create" if code("Forbidden") => {
            "You cannot hand out a role above your own.".to_string()
        }
        "account-password" if code("BadRequest") => {
            "A password needs at least 8 characters.".to_string()
        }
        "account-totp" if code("NotFound") => {
            format!("{subject} does not have two-factor set up.")
        }
        "class-set" if code("Forbidden") => {
            "You cannot grant a capability you do not hold yourself.".to_string()
        }
        "invite-revoke" if code("NotFound") => {
            "That invitation was already used or withdrawn.".to_string()
        }
        _ if code("Forbidden") => {
            "You can only change accounts below your own role, and never your own.".to_string()
        }
        _ if code("NotFound") => format!("There is no account called {subject} any more."),
        _ => format!("The burrow did not take it: {detail}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabbithole_server_core::{Caps, Role};

    fn account(login: &str, role: u8) -> AccountEntry {
        AccountEntry::new(1, login, role, None, false)
    }

    #[test]
    fn the_capability_bits_are_the_burrows_own() {
        let theirs: &[(u64, &str)] = &[
            (Caps::SEE.0, "See the burrow"),
            (Caps::WHO.0, "See who is on"),
            (Caps::CANNOT_BE_KICKED.0, "Cannot be kicked"),
            (Caps::CHAT_READ.0, "Read chat"),
            (Caps::CHAT_SEND.0, "Speak in chat"),
            (Caps::CHAT_CREATE_ROOM.0, "Make rooms"),
            (Caps::CHAT_MODERATE.0, "Moderate chat"),
            (Caps::DM_SEND.0, "Send direct messages"),
            (Caps::BOARD_READ.0, "Read boards"),
            (Caps::BOARD_POST.0, "Post to boards"),
            (Caps::BOARD_MODERATE.0, "Moderate boards"),
            (Caps::FILE_LIST.0, "Browse files"),
            (Caps::FILE_DOWNLOAD.0, "Download"),
            (Caps::FILE_UPLOAD.0, "Upload"),
            (Caps::FILE_MANAGE.0, "Manage files"),
            (Caps::DROPBOX_VIEW.0, "Open drop boxes"),
            (Caps::SWARM_ADVERTISE.0, "Offer files to the swarm"),
            (Caps::DOOR_RUN.0, "Run door games"),
            (Caps::USER_KICK.0, "Kick sessions"),
            (Caps::USER_BAN.0, "Ban people"),
            (Caps::ACCOUNT_ADMIN.0, "Manage accounts"),
            (Caps::CONFIG_ADMIN.0, "Change settings"),
            (Caps::BROADCAST.0, "Broadcast"),
            (Caps::AUDIT_READ.0, "Read the audit log"),
            (Caps::MODERATE.0, "Moderate content"),
        ];
        let ours: Vec<(u64, &str)> = CAPS
            .iter()
            .flat_map(|g| g.caps.iter())
            .map(|c| (1u64 << c.bit, c.name))
            .collect();
        assert_eq!(ours, theirs, "a capability moved, or a new one has no name");
        // Every capability a stock role holds has a name here.
        for role in [Role::Guest, Role::User, Role::Moderator, Role::Admin] {
            assert_eq!(unnamed_bits(role.default_caps().0), 0, "{role:?}");
        }
        assert_eq!(Role::Superuser as u8, SUPERUSER);
        assert_eq!(ROLES.len(), SUPERUSER as usize + 1);
    }

    #[test]
    fn the_console_offers_only_what_the_burrow_would_allow() {
        // Below yourself, never yourself, never a peer.
        assert!(may_manage(3, "ada", &account("alice", 1)));
        assert!(may_manage(3, "ada", &account("mo", 2)));
        assert!(!may_manage(3, "ada", &account("bea", 3)));
        assert!(!may_manage(3, "ada", &account("root", 4)));
        assert!(!may_manage(3, "ada", &account("ada", 3)));
        assert!(may_manage(4, "root", &account("bea", 3)));
        assert!(may_manage(4, "root", &account("other-root", 4)));
        assert!(!may_manage(4, "root", &account("root", 4)));
        assert!(why_not(3, "ada", &account("ada", 3))
            .unwrap()
            .contains("you"));
        assert!(why_not(3, "ada", &account("bea", 3))
            .unwrap()
            .contains("below"));
        assert!(why_not(3, "ada", &account("alice", 1)).is_none());

        let names = |r| {
            assignable_roles(r)
                .into_iter()
                .map(|(_, n)| n)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(3), ["Guest", "Member", "Moderator", "Admin"]);
        assert_eq!(names(2), ["Guest", "Member", "Moderator"]);
        assert_eq!(names(4).len(), 5);
        assert_eq!(role_name(2), "Moderator");
        assert_eq!(role_name(9), "Unknown");
    }

    #[test]
    fn editing_a_class_keeps_bits_this_console_has_no_name_for() {
        let upload = CAPS[3].caps[2];
        assert_eq!(upload.name, "Upload");
        let future = 1u64 << 60;
        let mask = future | Caps::CHAT_READ.0;
        assert!(!has(mask, &upload));
        let edited = with(mask, &upload, true);
        assert!(has(edited, &upload));
        assert_eq!(
            unnamed_bits(edited),
            future,
            "the unknown bit survives the edit"
        );
        assert_eq!(with(edited, &upload, false), mask);
    }

    #[test]
    fn a_generated_password_is_long_and_has_no_look_alikes() {
        let pw = password_from(&[0, 1, 2, 3, 4, 250, 251, 252, 253, 254, 9, 99, 199, 17, 77]);
        assert_eq!(pw.len(), 17, "15 characters in three groups: {pw}");
        assert_eq!(pw.matches('-').count(), 2);
        assert!(pw.chars().count() >= 8);
        for c in "0O1lIi".chars() {
            assert!(!pw.contains(c), "{pw} has a look-alike");
        }
    }

    #[test]
    fn an_invitation_is_open_used_or_expired() {
        let open = InviteEntry::new("A", "ada", 1_000, None);
        assert_eq!(invite_status(&open, 400), InviteStatus::Open(600));
        assert_eq!(invite_status(&open, 1_000), InviteStatus::Expired);
        let used = InviteEntry::new("B", "ada", 1_000, Some("zed".into()));
        assert_eq!(
            invite_status(&used, 5_000),
            InviteStatus::UsedBy("zed".into())
        );
    }

    #[test]
    fn each_action_says_what_came_of_it_and_what_to_reload() {
        let mut s = PeopleState::default();
        s.sent("*account-create:carol");
        assert!(s.is_busy("*account-create:carol"));
        let ack = [AdminEvent::Ack("Done.".into())];
        assert_eq!(s.apply("*account-create:carol", &ack), Reload::Accounts);
        assert!(!s.is_busy("*account-create:carol"));
        assert_eq!(s.notice, Some((true, "Made the account carol.".into())));
        assert_eq!(s.created.as_deref(), Some("carol"));

        let failed = |code: &str| [AdminEvent::Failed(format!("server error: {code}"))];
        s.apply("*account-create:carol", &failed("AlreadyExists"));
        assert!(s.notice.as_ref().unwrap().1.contains("is taken"));
        assert!(!s.notice.as_ref().unwrap().0);
        s.apply("*account-create:x", &failed("Forbidden"));
        assert!(s.notice.as_ref().unwrap().1.contains("above your own"));

        assert_eq!(s.apply("*account-password:alice", &ack), Reload::Nothing);
        assert!(s
            .notice
            .as_ref()
            .unwrap()
            .1
            .contains("signed out everywhere"));
        s.apply("*account-totp:alice", &failed("NotFound"));
        assert_eq!(
            s.notice.as_ref().unwrap().1,
            "alice does not have two-factor set up."
        );
        assert_eq!(
            s.apply("*account-set:bea", &failed("Forbidden")),
            Reload::Accounts
        );
        assert!(s.notice.as_ref().unwrap().1.contains("below your own role"));
        assert_eq!(s.apply("*class-set:helpers", &ack), Reload::Classes);
        s.apply("*class-set:helpers", &failed("Forbidden"));
        assert!(s.notice.as_ref().unwrap().1.contains("do not hold"));

        let listed = [AdminEvent::InvitesListed(vec![InviteEntry::new(
            "A", "ada", 9, None,
        )])];
        assert_eq!(s.apply("*invites", &listed), Reload::Nothing);
        assert_eq!(s.invites.len(), 1);
        assert_eq!(s.apply("*invite-revoke:A", &ack), Reload::Invites);
        assert_eq!(
            s.apply("*invite-revoke:A", &failed("NotFound")),
            Reload::Invites
        );
        assert!(s.notice.as_ref().unwrap().1.contains("already used"));
    }
}
