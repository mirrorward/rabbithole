//! Chat service: rooms with membership, invites, bans, and scrollback.
//!
//! Rooms live in memory (the lobby is permanent and everyone is a member;
//! ad-hoc private rooms vanish when their last member leaves). Membership
//! is per-session; chat pushes are delivered only to member sessions.
//! Room persistence across restarts is future work — noted in PLAN.

use std::collections::{HashMap, HashSet, VecDeque};

use parking_lot::RwLock;

use crate::bus::{EventBus, ServerEvent};

/// The room every burrow has.
pub const LOBBY: &str = "lobby";

/// Maximum scrollback lines retained per room.
const SCROLLBACK: usize = 500;

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
    history: VecDeque<ChatLine>,
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

    /// Validate and broadcast a line (sender must be a member).
    pub fn send(
        &self,
        room_name: &str,
        session_id: u64,
        from: &str,
        text: &str,
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
            if !room.members.contains_key(&session_id) {
                return Err(ChatError::NotMember);
            }
            let line = ChatLine {
                room: room.name.clone(),
                from: from.to_string(),
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

    #[test]
    fn lobby_membership_and_send() {
        let chat = service();
        chat.send(LOBBY, 1, "alice", "hello").unwrap();
        assert!(matches!(
            chat.send(LOBBY, 99, "ghost", "boo"),
            Err(ChatError::NotMember)
        ));
        assert!(matches!(
            chat.send("nowhere", 1, "alice", "hi"),
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
        chat.send("Tea Party", 2, "bob", "more tea").unwrap();

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
}
