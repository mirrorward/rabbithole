//! Chat service: rooms with membership, invites, bans, mutes, slow-mode,
//! and scrollback.
//!
//! Rooms live in memory (the lobby is permanent and everyone is a member;
//! ad-hoc private rooms vanish when their last member leaves). Membership
//! is per-session; chat pushes are delivered only to member sessions.
//! Room persistence across restarts is future work — noted in PLAN.
//!
//! Moderation (Wave 13): per-room **mutes** (a muted member keeps receiving
//! events but their sends are refused; optionally timed, with lazy expiry
//! against an injected clock) and **slow-mode** (a between-message minimum
//! per member; creators and CHAT_MODERATE holders are exempt). Both are
//! gated on the same creator-or-moderator rule as topic/kick and apply to
//! every surface that sends through [`ChatService::send`] — native, Hotline,
//! and telnet alike. The clock is caller-injected monotonic milliseconds
//! (the [`crate::ratelimit::now_ms`] convention), so tests are deterministic.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use parking_lot::RwLock;

use crate::bus::{EventBus, ServerEvent};

/// The room every burrow has.
pub const LOBBY: &str = "lobby";

/// Maximum scrollback lines retained per room.
const SCROLLBACK: usize = 500;

/// Longest accepted slow-mode interval (one hour); larger asks are clamped.
pub const MAX_SLOW_MODE_SECS: u32 = 3600;

#[derive(Debug, Clone)]
pub struct ChatLine {
    pub room: String,
    pub from: String,
    pub text: String,
    pub at_unix_ms: i64,
}

/// Room summary for listings.
#[derive(Debug, Clone)]
pub struct RoomSummary {
    pub name: String,
    pub category: String,
    pub topic: String,
    pub private: bool,
    pub member_count: u32,
    pub created_by: String,
}

struct Room {
    name: String, // display-cased
    category: String,
    topic: String,
    private: bool,
    persistent: bool,
    created_by_account: i64,
    created_by_name: String,
    /// session_id → screen_name (denormalized for member lists).
    members: HashMap<u64, String>,
    /// Accounts allowed into a private room.
    invited: HashSet<i64>,
    banned: HashSet<i64>,
    /// account → mute expiry in injected-clock milliseconds
    /// (`None` = permanent). Expired entries are pruned lazily.
    muted: HashMap<i64, Option<u64>>,
    /// Slow-mode interval in seconds; `0` = off.
    slow_mode_secs: u32,
    /// account → last accepted send (injected-clock ms), tracked only while
    /// slow-mode is on; cleared when it is turned off.
    last_sent_ms: HashMap<i64, u64>,
    history: VecDeque<ChatLine>,
}

impl Room {
    /// Is `account` muted right now? An expired timed mute is dropped the
    /// first time it is consulted (lazy expiry).
    fn muted_now(&mut self, account: i64, now_ms: u64) -> bool {
        match self.muted.get(&account) {
            None => false,
            Some(None) => true,
            Some(Some(until)) if *until > now_ms => true,
            Some(Some(_)) => {
                self.muted.remove(&account);
                false
            }
        }
    }
}

/// Identity and standing of a chat sender, consulted by the moderation
/// gates in [`ChatService::send`].
#[derive(Debug, Clone, Copy)]
pub struct Sender<'a> {
    /// Membership key.
    pub session_id: u64,
    /// Mute / slow-mode key.
    pub account_id: i64,
    /// Whether the caller holds CHAT_MODERATE: exempt from slow-mode
    /// (mutes still apply — a muted moderator can unmute themselves).
    pub is_moderator: bool,
    /// Display name stamped on the line.
    pub screen_name: &'a str,
}

pub struct ChatService {
    bus: EventBus,
    rooms: RwLock<HashMap<String, Room>>, // keyed lowercase
    max_len: usize,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ChatError {
    #[error("no such room: {0}")]
    NoSuchRoom(String),
    #[error("message too long ({len} > {max})")]
    TooLong { len: usize, max: usize },
    #[error("empty message")]
    Empty,
    #[error("room already exists")]
    AlreadyExists,
    #[error("not a member of this room")]
    NotMember,
    #[error("not permitted")]
    Forbidden,
    #[error("bad room name")]
    BadName,
    #[error("you are muted in this room")]
    Muted,
    #[error("slow mode is on: wait {retry_after_secs}s before sending again")]
    SlowMode { retry_after_secs: u32 },
}

fn key(name: &str) -> String {
    name.trim().to_lowercase()
}

impl ChatService {
    pub fn new(bus: EventBus, max_len: usize) -> Self {
        let service = Self {
            bus,
            rooms: RwLock::default(),
            max_len,
        };
        service.rooms.write().insert(
            key(LOBBY),
            Room {
                name: LOBBY.into(),
                category: "General".into(),
                topic: String::new(),
                private: false,
                persistent: true,
                created_by_account: 0,
                created_by_name: "server".into(),
                members: HashMap::new(),
                invited: HashSet::new(),
                banned: HashSet::new(),
                muted: HashMap::new(),
                slow_mode_secs: 0,
                last_sent_ms: HashMap::new(),
                history: VecDeque::new(),
            },
        );
        service
    }

    /// Called at session start: everyone is in the lobby.
    pub fn join_lobby(&self, session_id: u64, screen_name: &str) {
        if let Some(room) = self.rooms.write().get_mut(&key(LOBBY)) {
            room.members.insert(session_id, screen_name.to_string());
        }
    }

    /// Called at session end: drop membership everywhere, reaping empty
    /// ad-hoc rooms.
    pub fn session_closed(&self, session_id: u64) {
        let mut rooms = self.rooms.write();
        rooms.retain(|_, room| {
            room.members.remove(&session_id);
            room.persistent || !room.members.is_empty()
        });
    }

    /// Is this session a member of the room?
    pub fn is_member(&self, room: &str, session_id: u64) -> bool {
        self.rooms
            .read()
            .get(&key(room))
            .is_some_and(|r| r.members.contains_key(&session_id))
    }

    fn summary(room: &Room) -> RoomSummary {
        RoomSummary {
            name: room.name.clone(),
            category: room.category.clone(),
            topic: room.topic.clone(),
            private: room.private,
            member_count: room.members.len() as u32,
            created_by: room.created_by_name.clone(),
        }
    }

    /// Rooms visible to a viewer: public + private ones they belong to or
    /// are invited to.
    pub fn list(&self, viewer_session: u64, viewer_account: i64) -> Vec<RoomSummary> {
        let rooms = self.rooms.read();
        let mut out: Vec<RoomSummary> = rooms
            .values()
            .filter(|r| {
                !r.private
                    || r.members.contains_key(&viewer_session)
                    || r.invited.contains(&viewer_account)
            })
            .map(Self::summary)
            .collect();
        out.sort_by(|a, b| {
            (a.name != LOBBY)
                .cmp(&(b.name != LOBBY))
                .then(a.name.cmp(&b.name))
        });
        out
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &self,
        name: &str,
        category: &str,
        topic: &str,
        private: bool,
        creator_account: i64,
        creator_name: &str,
        creator_session: u64,
    ) -> Result<RoomSummary, ChatError> {
        let display = name.trim();
        if display.is_empty() || display.len() > 32 || display.contains(['/', '\n']) {
            return Err(ChatError::BadName);
        }
        let k = key(display);
        let mut rooms = self.rooms.write();
        if rooms.contains_key(&k) {
            return Err(ChatError::AlreadyExists);
        }
        let mut members = HashMap::new();
        members.insert(creator_session, creator_name.to_string());
        let mut invited = HashSet::new();
        invited.insert(creator_account);
        let room = Room {
            name: display.to_string(),
            category: category.trim().to_string(),
            topic: topic.trim().to_string(),
            private,
            persistent: false,
            created_by_account: creator_account,
            created_by_name: creator_name.to_string(),
            members,
            invited,
            banned: HashSet::new(),
            muted: HashMap::new(),
            slow_mode_secs: 0,
            last_sent_ms: HashMap::new(),
            history: VecDeque::new(),
        };
        let summary = Self::summary(&room);
        rooms.insert(k, room);
        Ok(summary)
    }

    pub fn join(
        &self,
        name: &str,
        session_id: u64,
        account_id: i64,
        screen_name: &str,
    ) -> Result<RoomSummary, ChatError> {
        let mut rooms = self.rooms.write();
        let room = rooms
            .get_mut(&key(name))
            .ok_or_else(|| ChatError::NoSuchRoom(name.into()))?;
        if room.banned.contains(&account_id) {
            return Err(ChatError::Forbidden);
        }
        if room.private
            && !room.invited.contains(&account_id)
            && room.created_by_account != account_id
        {
            return Err(ChatError::Forbidden);
        }
        room.members.insert(session_id, screen_name.to_string());
        Ok(Self::summary(room))
    }

    pub fn leave(&self, name: &str, session_id: u64) -> Result<(), ChatError> {
        if key(name) == key(LOBBY) {
            return Err(ChatError::Forbidden); // the lobby is forever
        }
        let mut rooms = self.rooms.write();
        let k = key(name);
        let Some(room) = rooms.get_mut(&k) else {
            return Err(ChatError::NoSuchRoom(name.into()));
        };
        room.members.remove(&session_id);
        if !room.persistent && room.members.is_empty() {
            rooms.remove(&k);
        }
        Ok(())
    }

    /// Record an invitation (inviter must be a member).
    pub fn invite(
        &self,
        name: &str,
        inviter_session: u64,
        target_account: i64,
    ) -> Result<(), ChatError> {
        let mut rooms = self.rooms.write();
        let room = rooms
            .get_mut(&key(name))
            .ok_or_else(|| ChatError::NoSuchRoom(name.into()))?;
        if !room.members.contains_key(&inviter_session) {
            return Err(ChatError::NotMember);
        }
        room.invited.insert(target_account);
        room.banned.remove(&target_account); // an invite forgives a ban
        Ok(())
    }

    /// Creator-or-moderator gate for topic/kick.
    fn can_moderate(room: &Room, account_id: i64, is_moderator: bool) -> bool {
        is_moderator || room.created_by_account == account_id
    }

    pub fn set_topic(
        &self,
        name: &str,
        topic: &str,
        account_id: i64,
        is_moderator: bool,
    ) -> Result<(), ChatError> {
        let mut rooms = self.rooms.write();
        let room = rooms
            .get_mut(&key(name))
            .ok_or_else(|| ChatError::NoSuchRoom(name.into()))?;
        if !Self::can_moderate(room, account_id, is_moderator) {
            return Err(ChatError::Forbidden);
        }
        room.topic = topic.trim().chars().take(200).collect();
        Ok(())
    }

    /// Kick (and optionally ban) every session of `target_account`.
    /// Returns the kicked session ids.
    pub fn kick(
        &self,
        name: &str,
        by_account: i64,
        by_is_moderator: bool,
        target_account: i64,
        target_sessions: &[u64],
        ban: bool,
    ) -> Result<Vec<u64>, ChatError> {
        let mut rooms = self.rooms.write();
        let room = rooms
            .get_mut(&key(name))
            .ok_or_else(|| ChatError::NoSuchRoom(name.into()))?;
        if !Self::can_moderate(room, by_account, by_is_moderator) {
            return Err(ChatError::Forbidden);
        }
        if room.created_by_account == target_account {
            return Err(ChatError::Forbidden); // can't kick the creator
        }
        let mut kicked = Vec::new();
        for s in target_sessions {
            if room.members.remove(s).is_some() {
                kicked.push(*s);
            }
        }
        if ban {
            room.banned.insert(target_account);
            room.invited.remove(&target_account);
        }
        Ok(kicked)
    }

    /// Mute `target_account` in a room (the lobby included): their sends
    /// are refused with [`ChatError::Muted`] while they stay a member and
    /// keep receiving events. `duration` of `None` is permanent — until
    /// unmuted or the room is reaped; a timed mute expires lazily against
    /// the injected clock. Creator-or-moderator gated; the room's creator
    /// can't be muted (mirroring kick).
    pub fn mute(
        &self,
        name: &str,
        by_account: i64,
        by_is_moderator: bool,
        target_account: i64,
        duration: Option<Duration>,
        now_ms: u64,
    ) -> Result<(), ChatError> {
        let mut rooms = self.rooms.write();
        let room = rooms
            .get_mut(&key(name))
            .ok_or_else(|| ChatError::NoSuchRoom(name.into()))?;
        if !Self::can_moderate(room, by_account, by_is_moderator) {
            return Err(ChatError::Forbidden);
        }
        if room.created_by_account == target_account {
            return Err(ChatError::Forbidden); // can't mute the creator
        }
        let until = duration
            .map(|d| now_ms.saturating_add(u64::try_from(d.as_millis()).unwrap_or(u64::MAX)));
        room.muted.insert(target_account, until);
        Ok(())
    }

    /// Lift a mute (same gate as [`mute`](Self::mute)). Returns `false`
    /// when the target wasn't muted any more — including a timed mute that
    /// had already expired — so callers can skip the audit/push.
    pub fn unmute(
        &self,
        name: &str,
        by_account: i64,
        by_is_moderator: bool,
        target_account: i64,
        now_ms: u64,
    ) -> Result<bool, ChatError> {
        let mut rooms = self.rooms.write();
        let room = rooms
            .get_mut(&key(name))
            .ok_or_else(|| ChatError::NoSuchRoom(name.into()))?;
        if !Self::can_moderate(room, by_account, by_is_moderator) {
            return Err(ChatError::Forbidden);
        }
        let was_muted = room.muted_now(target_account, now_ms);
        room.muted.remove(&target_account);
        Ok(was_muted)
    }

    /// Is this account muted in the room right now? Lazy expiry applies.
    pub fn is_muted(&self, name: &str, account_id: i64, now_ms: u64) -> bool {
        self.rooms
            .write()
            .get_mut(&key(name))
            .is_some_and(|r| r.muted_now(account_id, now_ms))
    }

    /// Set the room's slow-mode interval: the between-message minimum per
    /// member. `0` turns it off (and clears the per-member send clocks);
    /// values above [`MAX_SLOW_MODE_SECS`] are clamped. Returns the applied
    /// value. Creator-or-moderator gated; creators and moderators are
    /// exempt from the interval itself.
    pub fn set_slow_mode(
        &self,
        name: &str,
        seconds: u32,
        by_account: i64,
        by_is_moderator: bool,
    ) -> Result<u32, ChatError> {
        let mut rooms = self.rooms.write();
        let room = rooms
            .get_mut(&key(name))
            .ok_or_else(|| ChatError::NoSuchRoom(name.into()))?;
        if !Self::can_moderate(room, by_account, by_is_moderator) {
            return Err(ChatError::Forbidden);
        }
        let applied = seconds.min(MAX_SLOW_MODE_SECS);
        room.slow_mode_secs = applied;
        if applied == 0 {
            room.last_sent_ms.clear();
        }
        Ok(applied)
    }

    /// The room's current slow-mode interval in seconds (`0` = off, or no
    /// such room).
    pub fn slow_mode_secs(&self, name: &str) -> u32 {
        self.rooms
            .read()
            .get(&key(name))
            .map_or(0, |r| r.slow_mode_secs)
    }

    /// `(session_id, screen_name)` pairs of a room's members, sorted by
    /// session id — used by protocol bridges (e.g. the Hotline surface) to
    /// fan pushes out to member sessions. Empty when the room doesn't exist.
    /// This is an internal fan-out helper: callers gate visibility themselves
    /// (the public member listing is [`members`](Self::members)).
    pub fn member_sessions(&self, name: &str) -> Vec<(u64, String)> {
        let rooms = self.rooms.read();
        let Some(room) = rooms.get(&key(name)) else {
            return Vec::new();
        };
        let mut out: Vec<(u64, String)> =
            room.members.iter().map(|(s, n)| (*s, n.clone())).collect();
        out.sort_by_key(|(s, _)| *s);
        out
    }

    pub fn members(&self, name: &str, viewer_session: u64) -> Result<Vec<String>, ChatError> {
        let rooms = self.rooms.read();
        let room = rooms
            .get(&key(name))
            .ok_or_else(|| ChatError::NoSuchRoom(name.into()))?;
        if room.private && !room.members.contains_key(&viewer_session) {
            return Err(ChatError::NotMember);
        }
        let mut names: Vec<String> = room.members.values().cloned().collect();
        names.sort();
        Ok(names)
    }

    /// Validate and broadcast a line (sender must be a member). `now_ms`
    /// is the injected monotonic clock the mute/slow-mode gates run on.
    pub fn send(
        &self,
        room_name: &str,
        sender: Sender<'_>,
        text: &str,
        now_ms: u64,
    ) -> Result<ChatLine, ChatError> {
        let text = text.trim_end();
        if text.trim().is_empty() {
            return Err(ChatError::Empty);
        }
        if text.len() > self.max_len {
            return Err(ChatError::TooLong {
                len: text.len(),
                max: self.max_len,
            });
        }
        let line = {
            let mut rooms = self.rooms.write();
            let room = rooms
                .get_mut(&key(room_name))
                .ok_or_else(|| ChatError::NoSuchRoom(room_name.into()))?;
            if !room.members.contains_key(&sender.session_id) {
                return Err(ChatError::NotMember);
            }
            // Moderation gates (Wave 13): mute first, then slow-mode.
            if room.muted_now(sender.account_id, now_ms) {
                return Err(ChatError::Muted);
            }
            if room.slow_mode_secs > 0 {
                if !Self::can_moderate(room, sender.account_id, sender.is_moderator) {
                    if let Some(last) = room.last_sent_ms.get(&sender.account_id) {
                        let wait_ms = u64::from(room.slow_mode_secs).saturating_mul(1000);
                        let elapsed = now_ms.saturating_sub(*last);
                        if elapsed < wait_ms {
                            let retry_after_secs =
                                u32::try_from((wait_ms - elapsed).div_ceil(1000))
                                    .unwrap_or(u32::MAX);
                            return Err(ChatError::SlowMode { retry_after_secs });
                        }
                    }
                }
                room.last_sent_ms.insert(sender.account_id, now_ms);
            }
            let line = ChatLine {
                room: room.name.clone(),
                from: sender.screen_name.to_string(),
                text: text.to_string(),
                at_unix_ms: chrono::Utc::now().timestamp_millis(),
            };
            if room.history.len() == SCROLLBACK {
                room.history.pop_front();
            }
            room.history.push_back(line.clone());
            line
        };
        self.bus.publish(ServerEvent::Chat {
            room: line.room.clone(),
            from: line.from.clone(),
            text: line.text.clone(),
        });
        Ok(line)
    }

    /// Most recent `limit` lines, oldest first. Private rooms require
    /// membership; public scrollback is open.
    pub fn history(
        &self,
        name: &str,
        viewer_session: u64,
        limit: usize,
    ) -> Result<Vec<ChatLine>, ChatError> {
        let rooms = self.rooms.read();
        let room = rooms
            .get(&key(name))
            .ok_or_else(|| ChatError::NoSuchRoom(name.into()))?;
        if room.private && !room.members.contains_key(&viewer_session) {
            return Err(ChatError::NotMember);
        }
        let skip = room.history.len().saturating_sub(limit);
        Ok(room.history.iter().skip(skip).cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> ChatService {
        let s = ChatService::new(EventBus::default(), 64);
        s.join_lobby(1, "alice");
        s.join_lobby(2, "bob");
        s
    }

    /// A plain (non-moderator) sender. Convention here: session 1/account
    /// 10 is alice, session 2/account 20 is bob.
    fn sender(session_id: u64, account_id: i64, screen_name: &str) -> Sender<'_> {
        Sender {
            session_id,
            account_id,
            is_moderator: false,
            screen_name,
        }
    }

    #[test]
    fn lobby_membership_and_send() {
        let chat = service();
        chat.send(LOBBY, sender(1, 10, "alice"), "hello", 0)
            .unwrap();
        assert!(matches!(
            chat.send(LOBBY, sender(99, 990, "ghost"), "boo", 0),
            Err(ChatError::NotMember)
        ));
        assert!(matches!(
            chat.send("nowhere", sender(1, 10, "alice"), "hi", 0),
            Err(ChatError::NoSuchRoom(_))
        ));
        let h = chat.history(LOBBY, 1, 10).unwrap();
        assert_eq!(h.len(), 1);
    }

    #[test]
    fn create_join_leave_and_reaping() {
        let chat = service();
        chat.create(
            "Tea Party",
            "Social",
            "mad hatters only",
            false,
            10,
            "alice",
            1,
        )
        .unwrap();
        assert!(matches!(
            chat.create("tea party", "", "", false, 10, "alice", 1),
            Err(ChatError::AlreadyExists),
        ));
        // Case-insensitive join.
        chat.join("TEA PARTY", 2, 20, "bob").unwrap();
        assert_eq!(chat.members("Tea Party", 1).unwrap(), vec!["alice", "bob"]);
        // The fan-out helper pairs each member session with its name.
        assert_eq!(
            chat.member_sessions("tea party"),
            vec![(1, "alice".to_string()), (2, "bob".to_string())]
        );
        assert!(chat.member_sessions("nowhere").is_empty());
        chat.send("Tea Party", sender(2, 20, "bob"), "more tea", 0)
            .unwrap();

        // Non-persistent room reaps when empty.
        chat.leave("Tea Party", 1).unwrap();
        chat.leave("Tea Party", 2).unwrap();
        assert!(matches!(
            chat.join("Tea Party", 2, 20, "bob"),
            Err(ChatError::NoSuchRoom(_))
        ));
        // The lobby can't be left.
        assert!(matches!(chat.leave(LOBBY, 1), Err(ChatError::Forbidden)));
    }

    #[test]
    fn private_rooms_invites_and_bans() {
        let chat = service();
        chat.create("secret", "", "", true, 10, "alice", 1).unwrap();
        // Uninvited can't join or list members.
        assert!(matches!(
            chat.join("secret", 2, 20, "bob"),
            Err(ChatError::Forbidden)
        ));
        assert!(matches!(
            chat.members("secret", 2),
            Err(ChatError::NotMember)
        ));
        assert!(matches!(
            chat.history("secret", 2, 10),
            Err(ChatError::NotMember)
        ));
        // Only members can invite.
        assert!(matches!(
            chat.invite("secret", 2, 20),
            Err(ChatError::NotMember)
        ));
        chat.invite("secret", 1, 20).unwrap();
        chat.join("secret", 2, 20, "bob").unwrap();

        // Kick + ban: bob can't rejoin; creator can't be kicked; a fresh
        // invite forgives the ban.
        let kicked = chat.kick("secret", 10, false, 20, &[2], true).unwrap();
        assert_eq!(kicked, vec![2]);
        assert!(matches!(
            chat.join("secret", 2, 20, "bob"),
            Err(ChatError::Forbidden)
        ));
        assert!(matches!(
            chat.kick("secret", 20, false, 10, &[1], false),
            Err(ChatError::Forbidden)
        ));
        chat.invite("secret", 1, 20).unwrap();
        chat.join("secret", 2, 20, "bob").unwrap();

        // Random member can't set topic; creator and moderators can.
        assert!(matches!(
            chat.set_topic("secret", "x", 20, false),
            Err(ChatError::Forbidden)
        ));
        chat.set_topic("secret", "wonderland business", 10, false)
            .unwrap();
        chat.set_topic("secret", "mod override", 999, true).unwrap();
    }

    #[test]
    fn listing_respects_privacy() {
        let chat = service();
        chat.create("open", "General", "", false, 10, "alice", 1)
            .unwrap();
        chat.create("hidden", "", "", true, 10, "alice", 1).unwrap();
        // Bob (uninvited) sees lobby + open only.
        let names: Vec<String> = chat.list(2, 20).into_iter().map(|r| r.name).collect();
        assert_eq!(names, vec!["lobby", "open"]);
        // Invited bob sees hidden too.
        chat.invite("hidden", 1, 20).unwrap();
        let names: Vec<String> = chat.list(2, 20).into_iter().map(|r| r.name).collect();
        assert_eq!(names, vec!["lobby", "hidden", "open"]);
        // Lobby sorts first.
        assert_eq!(chat.list(1, 10)[0].name, LOBBY);
    }

    #[test]
    fn session_close_reaps_memberships() {
        let chat = service();
        chat.create("temp", "", "", true, 10, "alice", 1).unwrap();
        chat.session_closed(1);
        assert!(matches!(
            chat.members("temp", 1),
            Err(ChatError::NoSuchRoom(_))
        ));
        assert!(!chat.is_member(LOBBY, 1));
        assert!(chat.is_member(LOBBY, 2));
    }

    #[test]
    fn mute_refuses_sends_until_unmute_or_expiry() {
        let chat = service();
        // A plain member can't mute; a moderator can (lobby included).
        assert!(matches!(
            chat.mute(LOBBY, 20, false, 10, None, 0),
            Err(ChatError::Forbidden)
        ));
        assert!(matches!(
            chat.mute("nowhere", 999, true, 10, None, 0),
            Err(ChatError::NoSuchRoom(_))
        ));
        chat.mute(LOBBY, 999, true, 10, None, 0).unwrap();
        assert!(chat.is_muted(LOBBY, 10, 0));
        assert!(matches!(
            chat.send(LOBBY, sender(1, 10, "alice"), "gagged", 0),
            Err(ChatError::Muted)
        ));
        // The mute is per-account: bob speaks freely, and alice still
        // *receives* (membership untouched).
        chat.send(LOBBY, sender(2, 20, "bob"), "carry on", 0)
            .unwrap();
        assert!(chat.is_member(LOBBY, 1));

        // Unmute needs the same gate, restores the voice, and reports
        // whether anything was removed.
        assert!(matches!(
            chat.unmute(LOBBY, 20, false, 10, 0),
            Err(ChatError::Forbidden)
        ));
        assert!(chat.unmute(LOBBY, 999, true, 10, 0).unwrap());
        assert!(!chat.unmute(LOBBY, 999, true, 10, 0).unwrap());
        chat.send(LOBBY, sender(1, 10, "alice"), "free again", 0)
            .unwrap();

        // A timed mute expires lazily on the injected clock.
        chat.mute(LOBBY, 999, true, 10, Some(Duration::from_secs(5)), 1_000)
            .unwrap();
        assert!(matches!(
            chat.send(LOBBY, sender(1, 10, "alice"), "early", 5_999),
            Err(ChatError::Muted)
        ));
        chat.send(LOBBY, sender(1, 10, "alice"), "late", 6_000)
            .unwrap();
        assert!(!chat.is_muted(LOBBY, 10, 6_000));
        // An already-expired mute reads as "nothing to unmute".
        chat.mute(LOBBY, 999, true, 10, Some(Duration::from_secs(1)), 0)
            .unwrap();
        assert!(!chat.unmute(LOBBY, 999, true, 10, 10_000).unwrap());
    }

    #[test]
    fn slow_mode_spaces_sends_and_exempts_moderators() {
        let chat = service();
        chat.join_lobby(3, "mo");
        assert!(matches!(
            chat.set_slow_mode(LOBBY, 10, 20, false),
            Err(ChatError::Forbidden)
        ));
        assert!(matches!(
            chat.set_slow_mode("nowhere", 10, 999, true),
            Err(ChatError::NoSuchRoom(_))
        ));
        // Oversized asks clamp to the cap.
        assert_eq!(
            chat.set_slow_mode(LOBBY, 90_000, 999, true).unwrap(),
            MAX_SLOW_MODE_SECS
        );
        assert_eq!(chat.set_slow_mode(LOBBY, 10, 999, true).unwrap(), 10);
        assert_eq!(chat.slow_mode_secs(LOBBY), 10);

        // First line is free; a second inside the window is refused with
        // the remaining wait (rounded up).
        chat.send(LOBBY, sender(1, 10, "alice"), "one", 0).unwrap();
        match chat.send(LOBBY, sender(1, 10, "alice"), "two", 4_000) {
            Err(ChatError::SlowMode { retry_after_secs }) => assert_eq!(retry_after_secs, 6),
            other => panic!("expected a slow-mode refusal, got {other:?}"),
        }
        // Each member has their own clock.
        chat.send(LOBBY, sender(2, 20, "bob"), "mine", 4_000)
            .unwrap();
        // Past the window the send passes.
        chat.send(LOBBY, sender(1, 10, "alice"), "three", 10_000)
            .unwrap();
        // Moderators are exempt.
        let mo = Sender {
            session_id: 3,
            account_id: 30,
            is_moderator: true,
            screen_name: "mo",
        };
        chat.send(LOBBY, mo, "rapid", 10_000).unwrap();
        chat.send(LOBBY, mo, "fire", 10_001).unwrap();

        // 0 turns it off and clears the per-member clocks.
        assert_eq!(chat.set_slow_mode(LOBBY, 0, 999, true).unwrap(), 0);
        assert_eq!(chat.slow_mode_secs(LOBBY), 0);
        chat.send(LOBBY, sender(1, 10, "alice"), "free", 10_001)
            .unwrap();
        chat.send(LOBBY, sender(1, 10, "alice"), "flow", 10_002)
            .unwrap();
    }

    #[test]
    fn room_scoped_moderation_and_creator_protection() {
        let chat = service();
        chat.create("den", "", "", false, 10, "alice", 1).unwrap();
        chat.join("den", 2, 20, "bob").unwrap();

        // The creator moderates their own room; plain members don't.
        assert!(matches!(
            chat.mute("den", 20, false, 10, None, 0),
            Err(ChatError::Forbidden)
        ));
        chat.mute("den", 10, false, 20, None, 0).unwrap();
        assert!(chat.is_muted("den", 20, 0));
        assert!(matches!(
            chat.send("den", sender(2, 20, "bob"), "psst", 0),
            Err(ChatError::Muted)
        ));
        // Mutes are room-scoped: bob still speaks in the lobby.
        assert!(!chat.is_muted(LOBBY, 20, 0));
        chat.send(LOBBY, sender(2, 20, "bob"), "still here", 0)
            .unwrap();

        // The creator can't be muted, even by a global moderator.
        assert!(matches!(
            chat.mute("den", 999, true, 10, None, 0),
            Err(ChatError::Forbidden)
        ));

        // The creator is exempt from their room's slow-mode.
        chat.set_slow_mode("den", MAX_SLOW_MODE_SECS, 10, false)
            .unwrap();
        chat.send("den", sender(1, 10, "alice"), "a", 0).unwrap();
        chat.send("den", sender(1, 10, "alice"), "b", 1).unwrap();
    }
}
