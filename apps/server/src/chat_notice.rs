//! Text projections for room policy changes on legacy chat surfaces.

use rabbithole_server_core::{ChatService, ServerEvent, LOBBY};

/// Match native push visibility: lobby changes reach signed-in viewers, and
/// other rooms require current session membership, including after a kick.
/// Return the original room for protocol routing, with safe single-line text.
pub(crate) fn for_session<'a>(
    chat: &ChatService,
    session: u64,
    event: &'a ServerEvent,
) -> Option<(&'a str, String)> {
    let room = match event {
        ServerEvent::RoomMuted { room, .. } | ServerEvent::RoomSlowModeChanged { room, .. } => room,
        _ => return None,
    };
    if !room.eq_ignore_ascii_case(LOBBY) && !chat.is_member(room, session) {
        return None;
    }
    let place = single_line(room);
    let text = match event {
        ServerEvent::RoomMuted {
            screen_name,
            muted,
            duration_secs,
            ..
        } => {
            let who = single_line(screen_name);
            if !muted {
                format!("{who} was unmuted in {place}.")
            } else if let Some(seconds) = duration_secs {
                format!("{who} was muted in {place} for {}.", interval(*seconds))
            } else {
                format!("{who} was muted in {place} until unmuted.")
            }
        }
        ServerEvent::RoomSlowModeChanged { seconds: 0, .. } => {
            format!("Slow mode in {place} is off.")
        }
        ServerEvent::RoomSlowModeChanged { seconds, .. } => {
            format!(
                "Slow mode in {place}: one message every {}.",
                interval(*seconds)
            )
        }
        _ => unreachable!("only room policy changes passed the routing check"),
    };
    Some((room, text))
}

fn interval(seconds: u32) -> String {
    format!("{seconds} second{}", if seconds == 1 { "" } else { "s" })
}

fn single_line(text: &str) -> String {
    // Names are data, never terminal commands or extra protocol chat lines.
    text.chars().filter(|c| !c.is_control()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabbithole_server_core::EventBus;

    fn mute(room: &str, muted: bool, duration_secs: Option<u32>) -> ServerEvent {
        ServerEvent::RoomMuted {
            account: 5,
            screen_name: "pest".into(),
            room: room.into(),
            muted,
            duration_secs,
        }
    }

    #[test]
    fn policy_changes_say_the_target_duration_and_recovery() {
        let chat = ChatService::new(EventBus::default(), 1024);
        for (event, expected) in [
            (
                mute(LOBBY, true, None),
                "pest was muted in lobby until unmuted.",
            ),
            (
                mute(LOBBY, true, Some(60)),
                "pest was muted in lobby for 60 seconds.",
            ),
            (
                mute(LOBBY, true, Some(1)),
                "pest was muted in lobby for 1 second.",
            ),
            (mute(LOBBY, false, None), "pest was unmuted in lobby."),
            (
                ServerEvent::RoomSlowModeChanged {
                    room: LOBBY.into(),
                    seconds: 30,
                    by: "mo".into(),
                },
                "Slow mode in lobby: one message every 30 seconds.",
            ),
            (
                ServerEvent::RoomSlowModeChanged {
                    room: LOBBY.into(),
                    seconds: 1,
                    by: "mo".into(),
                },
                "Slow mode in lobby: one message every 1 second.",
            ),
            (
                ServerEvent::RoomSlowModeChanged {
                    room: LOBBY.into(),
                    seconds: 0,
                    by: "mo".into(),
                },
                "Slow mode in lobby is off.",
            ),
        ] {
            assert_eq!(
                for_session(&chat, 7, &event),
                Some((LOBBY, expected.into()))
            );
        }
        assert!(for_session(&chat, 7, &ServerEvent::Shutdown).is_none());
    }

    #[test]
    fn private_policy_notices_require_current_membership() {
        let chat = ChatService::new(EventBus::default(), 1024);
        chat.create("Quiet", "", "", true, 1, "owner", 7).unwrap();
        for event in [
            mute("quiet", true, None),
            ServerEvent::RoomSlowModeChanged {
                room: "quiet".into(),
                seconds: 10,
                by: "owner".into(),
            },
        ] {
            assert!(for_session(&chat, 7, &event).is_some());
            assert!(for_session(&chat, 8, &event).is_none());
        }
        chat.leave("Quiet", 7).unwrap();
        assert!(for_session(&chat, 7, &mute("quiet", true, None)).is_none());
        assert!(for_session(&chat, 8, &mute("LOBBY", true, None)).is_some());
    }

    #[test]
    fn untrusted_labels_cannot_emit_terminal_controls_or_extra_lines() {
        let chat = ChatService::new(EventBus::default(), 1024);
        let room = "Quiet\r\x1b\t";
        chat.create(room, "", "", true, 1, "owner", 7).unwrap();
        let event = ServerEvent::RoomMuted {
            account: 5,
            screen_name: "pe\x1bst\r\n\t\u{009b}".into(),
            room: room.into(),
            muted: true,
            duration_secs: None,
        };
        let (routed_room, text) = for_session(&chat, 7, &event).unwrap();
        assert_eq!(routed_room, room);
        assert_eq!(text, "pest was muted in Quiet until unmuted.");
        assert!(!text.chars().any(char::is_control));
    }
}
