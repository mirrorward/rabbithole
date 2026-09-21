//! Keeping a room: its pace, who is muted, who may be taken out, and what
//! the burrow says when it will not.
//!
//! The burrow has done all of this since wave 13 — mute, unmute, slow mode,
//! removing somebody and barring them — and says how a room is kept when
//! asked. What it says is numbers and codes; a person wants to know that
//! the room is going slowly and why their message did not go. All pure, so
//! the wording is tested rather than looked at.

use std::collections::BTreeMap;

use rabbithole_proto::chat::{MutedMember, RoomModeration};
use rabbithole_proto::ErrorCode;

use crate::admin_settings::humanize_secs;
use crate::wire::{RoomAskKind, RoomKeepingAnswer};

/// The paces a room can be set to: `(seconds, words)`. Off first.
pub const PACES: [(u32, &str); 7] = [
    (0, "Off"),
    (5, "5 seconds"),
    (30, "30 seconds"),
    (60, "1 minute"),
    (300, "5 minutes"),
    (900, "15 minutes"),
    (3_600, "1 hour"),
];

/// How long somebody can be muted for: `(seconds, words)`, `None` until
/// somebody lifts it.
pub const MUTES: [(Option<u32>, &str); 4] = [
    (Some(600), "For 10 minutes"),
    (Some(3_600), "For an hour"),
    (Some(86_400), "For a day"),
    (None, "Until lifted"),
];

/// How this person has seen one burrow's rooms kept. What the burrow says
/// no to is said in the scrollback, where a person is looking, like every
/// other refusal of a room.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeepingState {
    /// How each room is kept, as last heard, by room (in lower case: a
    /// burrow does not tell `Den` from `den`).
    pub rooms: BTreeMap<String, RoomModeration>,
    /// When each room's answer arrived, Unix milliseconds: a mute's time
    /// left is counted from it.
    heard_at: BTreeMap<String, i64>,
    /// When a room is next to be looked at again because a timed mute in
    /// it runs out: a burrow lets it lapse without a word.
    looks_due: BTreeMap<String, i64>,
}

/// A room's key: rooms are one room whatever their case.
fn key(room: &str) -> String {
    room.to_lowercase()
}

impl KeepingState {
    /// How `room` is kept, as last heard.
    pub fn of(&self, room: &str) -> Option<&RoomModeration> {
        self.rooms.get(&key(room))
    }

    /// When a mute heard of in `room` with `remaining_secs` left ends, in
    /// Unix milliseconds; `None` for one with no end.
    pub fn ends_at(&self, room: &str, remaining_secs: Option<u32>) -> Option<i64> {
        let heard = *self.heard_at.get(&key(room))?;
        remaining_secs.map(|secs| heard + i64::from(secs) * 1_000)
    }

    /// Fold in what the burrow said, as heard by `me` at `now_ms`. Returns
    /// what to do next.
    ///
    /// Somebody muted or the pace changed: a room's keepers look again
    /// (they are shown everybody's mutes); anybody else has all they need
    /// in the news itself — their own mute, or the new pace — so a lobby
    /// full of people does not all ask at once.
    pub fn answered(&mut self, answer: RoomKeepingAnswer, me: &str, now_ms: i64) -> Option<Next> {
        match answer {
            RoomKeepingAnswer::Kept(kept) => {
                let room = key(&kept.room);
                let soonest = kept.muted.iter().filter_map(|m| m.remaining_secs).min();
                let name = kept.room.clone();
                self.rooms.insert(room.clone(), kept);
                self.heard_at.insert(room, now_ms);
                self.look_when(&name, soonest?, now_ms)
            }
            RoomKeepingAnswer::Muted {
                room,
                who,
                muted,
                secs,
            } => {
                let view = self.rooms.get_mut(&key(&room))?;
                if view.may_moderate {
                    return Some(Next::Look(room));
                }
                if who != me {
                    return None;
                }
                view.muted.retain(|m| m.screen_name != me);
                if !muted {
                    return None;
                }
                view.muted.push(MutedMember::new(me, secs));
                self.heard_at.insert(key(&room), now_ms);
                // Told by the news, and let lapse without any: look again
                // when it ends, as for a mute found on looking.
                self.look_when(&room, secs?, now_ms)
            }
            RoomKeepingAnswer::Paced { room, secs } => {
                if let Some(view) = self.rooms.get_mut(&key(&room)) {
                    view.slow_mode_secs = secs;
                }
                None
            }
            RoomKeepingAnswer::Removed { room, banned } => {
                self.rooms.remove(&key(&room));
                let words = if banned {
                    format!(
                        "You were taken out of {room}, and may not go back unless you are asked."
                    )
                } else {
                    format!("You were taken out of {room}.")
                };
                Some(Next::Leave { room, words })
            }
            // A keeper turned down may be looking at an old picture of the
            // room they are keeping.
            RoomKeepingAnswer::Refused { ask, .. } => matches!(
                ask,
                RoomAskKind::Mute | RoomAskKind::Unmute | RoomAskKind::Remove
            )
            .then_some(Next::LookHere),
        }
    }

    /// Look at `room` again once a mute with `secs` left, heard of at
    /// `now_ms`, has run out — unless a look is already due by then.
    fn look_when(&mut self, room: &str, secs: u32, now_ms: i64) -> Option<Next> {
        let at = now_ms + i64::from(secs) * 1_000;
        let k = key(room);
        if self.looks_due.get(&k).is_some_and(|due| *due <= at) {
            return None;
        }
        self.looks_due.insert(k, at);
        Some(Next::LookAt {
            room: room.to_string(),
            at_ms: at,
        })
    }

    /// A look at `room` was due by now: whether it still is (a later
    /// answer may have moved it), clearing it if so.
    pub fn look_due(&mut self, room: &str, now_ms: i64) -> bool {
        let room = key(room);
        match self.looks_due.get(&room) {
            Some(due) if *due <= now_ms => {
                self.looks_due.remove(&room);
                true
            }
            _ => false,
        }
    }
}

/// What the app should do after an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Ask again how this room is kept.
    Look(String),
    /// Ask again how the room on show is kept.
    LookHere,
    /// Ask again how `room` is kept once it is `at_ms`: a timed mute in it
    /// runs out then.
    LookAt { room: String, at_ms: i64 },
    /// This person is no longer in `room`: go back to the lobby, and say so
    /// there.
    Leave { room: String, words: String },
}

/// What a refusal means for a person, said for what they tried. `None` for
/// the pane's own look being refused (a private room says nothing to an
/// outsider, and nobody asked for anything).
pub fn refusal(ask: RoomAskKind, code: ErrorCode) -> Option<String> {
    use ErrorCode as E;
    use RoomAskKind as K;
    let keepers = "Only the room's maker or a moderator can do that";
    Some(match (ask, code) {
        (K::Look, _) => return None,
        (K::Say, E::Muted) => "You are muted in this room, so that was not sent.".to_string(),
        (K::Say, E::SlowMode { retry_after_secs }) => format!(
            "This room is going slowly. You can say something again in {}.",
            humanize_secs(u64::from(retry_after_secs.max(1)))
        ),
        (K::Say, E::RateLimited) => {
            "That is a lot at once. Wait a moment, then send it again.".to_string()
        }
        // Not in the room any more (a dropped connection, or taken out), no
        // agreement accepted, or no word to talk: the code does not say
        // which, so the words point at what usually helps.
        (K::Say, E::Forbidden) => {
            "That was not sent. If you have been away, pick the room again to go back in."
                .to_string()
        }
        (K::Say, _) => "That was not sent.".to_string(),
        (K::Mute | K::Remove, E::Forbidden) => {
            format!("{keepers}, and never to the room's maker.")
        }
        (K::Topic | K::Pace | K::Unmute, E::Forbidden) => format!("{keepers}."),
        (K::Invite, E::Forbidden) => "That is not yours to do here.".to_string(),
        (K::Unmute, E::NotFound) => "They were not muted.".to_string(),
        (K::Mute | K::Remove, E::NotFound) => "Nobody here goes by that name.".to_string(),
        (K::Invite, E::NotFound) => "Nobody on this burrow goes by that name.".to_string(),
        (K::Topic, E::NotFound) => "No such room.".to_string(),
        (_, E::RateLimited) => "That is a lot at once. Wait a moment, then try again.".to_string(),
        _ => "The burrow did not take that.".to_string(),
    })
}

/// What a room's pace means, or `None` when it has none.
pub fn pace_line(secs: u32) -> Option<String> {
    (secs > 0).then(|| {
        format!(
            "Slow mode: one message each every {}.",
            humanize_secs(u64::from(secs))
        )
    })
}

/// A muted person's own line: until when (`until`, a time of day already
/// said the person's way), or until somebody lifts it. A time rather than
/// "10 minutes more", which would be wrong a minute later.
pub fn muted_line(until: Option<&str>) -> String {
    match until {
        Some(at) => format!("You are muted here until {at}."),
        None => "You are muted here until a keeper lifts it.".to_string(),
    }
}

/// When something ends, said from now: the time today, or which day. `end`
/// and `now` are times of day (`HH:MM`, the person's own clock), `diff_ms`
/// how far off the end is. A day's mute said as "15:14" would read as over
/// already.
pub fn when_words(end: &str, now: &str, diff_ms: i64) -> String {
    let minutes = |hhmm: &str| -> i64 {
        let (h, m) = hhmm.split_once(':').unwrap_or(("0", "0"));
        h.parse::<i64>().unwrap_or(0) * 60 + m.parse::<i64>().unwrap_or(0)
    };
    let days = (minutes(now) + diff_ms.max(0) / 60_000) / 1_440;
    match days {
        0 => end.to_string(),
        1 => format!("tomorrow at {end}"),
        n => format!("in {n} days, at {end}"),
    }
}

/// When somebody else's mute ends, for the room's keepers.
pub fn mute_left(until: Option<&str>) -> String {
    match until {
        Some(at) => format!("Muted until {at}"),
        None => "Muted until lifted".to_string(),
    }
}

/// The words for a pace, to show the one a room is at; a pace the burrow
/// set that is not one of [`PACES`] is said as it is.
pub fn pace_label(secs: u32) -> String {
    PACES
        .iter()
        .find(|(s, _)| *s == secs)
        .map(|(_, words)| words.to_string())
        .unwrap_or_else(|| humanize_secs(u64::from(secs)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_room_is_said_the_way_a_person_would() {
        assert_eq!(pace_line(0), None);
        assert_eq!(
            pace_line(30).as_deref(),
            Some("Slow mode: one message each every 30 seconds.")
        );
        assert_eq!(muted_line(Some("15:14")), "You are muted here until 15:14.");
        assert!(muted_line(None).contains("until a keeper lifts it"));
        assert_eq!(mute_left(None), "Muted until lifted");
        assert_eq!(mute_left(Some("15:14")), "Muted until 15:14");
        // A day's mute is not over already; an hour past midnight is tomorrow.
        assert_eq!(
            when_words("15:14", "15:14", 86_400_000),
            "tomorrow at 15:14"
        );
        assert_eq!(when_words("00:30", "23:30", 3_600_000), "tomorrow at 00:30");
        assert_eq!(when_words("15:24", "15:14", 600_000), "15:24");
        assert_eq!(
            when_words("09:00", "09:00", 3 * 86_400_000),
            "in 3 days, at 09:00"
        );
        assert_eq!(pace_label(60), "1 minute");
        assert_eq!(pace_label(45), "45 seconds");
    }

    #[test]
    fn a_refusal_is_said_for_what_was_tried() {
        use ErrorCode as E;
        use RoomAskKind as K;
        let said = |ask, code| refusal(ask, code).unwrap_or_default();
        assert!(said(K::Say, E::Muted).contains("muted"));
        assert!(said(
            K::Say,
            E::SlowMode {
                retry_after_secs: 12
            }
        )
        .contains("12 seconds"));
        assert!(said(K::Mute, E::Forbidden).contains("never to the room's maker"));
        assert!(said(K::Pace, E::Forbidden).contains("maker or a moderator"));
        assert_eq!(said(K::Unmute, E::NotFound), "They were not muted.");
        assert!(said(K::Remove, E::NotFound).contains("Nobody here"));
        assert_eq!(
            refusal(K::Look, E::NotFound),
            None,
            "the pane's own look is not a no"
        );
    }

    #[test]
    fn what_the_burrow_says_lands_where_it_belongs() {
        use ErrorCode as E;
        let mut state = KeepingState::default();
        let kept = RoomModeration::new(
            "Den",
            30,
            true,
            vec!["pest".into()],
            vec![MutedMember::new("pest", Some(60))],
        );
        // Heard at 1000: a mute with a minute left ends at 61000, and the
        // room is looked at again then.
        assert_eq!(
            state.answered(RoomKeepingAnswer::Kept(kept.clone()), "carol", 1_000),
            Some(Next::LookAt {
                room: "Den".into(),
                at_ms: 61_000
            })
        );
        assert_eq!(
            state.of("den"),
            Some(&kept),
            "a room is one room in any case"
        );
        assert_eq!(state.ends_at("den", Some(60)), Some(61_000));
        // Heard again sooner, no second timer for the same end.
        assert_eq!(
            state.answered(RoomKeepingAnswer::Kept(kept.clone()), "carol", 1_500),
            None
        );
        assert!(!state.look_due("den", 30_000));
        assert!(state.look_due("den", 61_000));
        assert!(!state.look_due("den", 61_000), "once");

        // A keeper looks again when somebody is muted; a pace is taken as
        // it comes.
        assert_eq!(
            state.answered(
                RoomKeepingAnswer::Muted {
                    room: "den".into(),
                    who: "bob".into(),
                    muted: true,
                    secs: None
                },
                "carol",
                2_000
            ),
            Some(Next::Look("den".into()))
        );
        assert_eq!(
            state.answered(
                RoomKeepingAnswer::Paced {
                    room: "den".into(),
                    secs: 0
                },
                "carol",
                2_000
            ),
            None
        );
        assert_eq!(state.of("den").unwrap().slow_mode_secs, 0);

        // A keeper turned down looks again at the room they are keeping; a
        // message turned down needs no second look.
        assert_eq!(
            state.answered(
                RoomKeepingAnswer::Refused {
                    ask: RoomAskKind::Mute,
                    code: E::Forbidden
                },
                "carol",
                2_000
            ),
            Some(Next::LookHere)
        );
        assert_eq!(
            state.answered(
                RoomKeepingAnswer::Refused {
                    ask: RoomAskKind::Say,
                    code: E::Muted
                },
                "carol",
                2_000
            ),
            None
        );

        // Taken out: the room is forgotten, and the person is told why.
        match state.answered(
            RoomKeepingAnswer::Removed {
                room: "den".into(),
                banned: true,
            },
            "carol",
            3_000,
        ) {
            Some(Next::Leave { room, words }) => {
                assert_eq!(room, "den");
                assert!(words.contains("may not go back"), "{words}");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(state.of("den"), None);
    }

    #[test]
    fn somebody_who_does_not_keep_a_room_does_not_ask_again_for_news() {
        let mut state = KeepingState::default();
        let lobby = RoomModeration::new("lobby", 0, false, Vec::new(), Vec::new());
        state.answered(RoomKeepingAnswer::Kept(lobby), "pest", 0);
        // Somebody else muted: nothing to do, nothing to ask.
        let other = RoomKeepingAnswer::Muted {
            room: "lobby".into(),
            who: "bob".into(),
            muted: true,
            secs: None,
        };
        assert_eq!(state.answered(other, "pest", 0), None);
        assert!(state.of("lobby").unwrap().muted.is_empty());
        // Muted themselves: they know, from the news.
        let mine = RoomKeepingAnswer::Muted {
            room: "lobby".into(),
            who: "pest".into(),
            muted: true,
            secs: Some(600),
        };
        // …and look again when it runs out, since nothing will say so.
        assert_eq!(
            state.answered(mine, "pest", 5_000),
            Some(Next::LookAt {
                room: "lobby".into(),
                at_ms: 605_000
            })
        );
        let view = state.of("lobby").unwrap();
        assert_eq!(view.muted, vec![MutedMember::new("pest", Some(600))]);
        assert_eq!(state.ends_at("lobby", Some(600)), Some(605_000));
        // And let talk again.
        let lifted = RoomKeepingAnswer::Muted {
            room: "lobby".into(),
            who: "pest".into(),
            muted: false,
            secs: None,
        };
        state.answered(lifted, "pest", 6_000);
        assert!(state.of("lobby").unwrap().muted.is_empty());
        // The pace, from the news.
        state.answered(
            RoomKeepingAnswer::Paced {
                room: "lobby".into(),
                secs: 30,
            },
            "pest",
            7_000,
        );
        assert_eq!(state.of("lobby").unwrap().slow_mode_secs, 30);
        // News of a room never looked at is nobody's business here.
        let elsewhere = RoomKeepingAnswer::Muted {
            room: "den".into(),
            who: "pest".into(),
            muted: true,
            secs: None,
        };
        assert_eq!(state.answered(elsewhere, "pest", 0), None);
    }
}
