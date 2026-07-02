//! Chat service. Wave 1: a single public lobby with in-memory scrollback.
//!
//! Lines are published on the event bus; each connected session's pump
//! turns bus events into `ChatMessage` pushes. Persistence and multiple
//! rooms arrive in Wave 2.

use std::collections::VecDeque;

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

pub struct ChatService {
    bus: EventBus,
    history: RwLock<VecDeque<ChatLine>>,
    /// Maximum accepted line length (config-mirrored).
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
}

impl ChatService {
    pub fn new(bus: EventBus, max_len: usize) -> Self {
        Self {
            bus,
            history: RwLock::default(),
            max_len,
        }
    }

    /// Validate and broadcast a line. Returns the stamped line.
    pub fn send(&self, room: &str, from: &str, text: &str) -> Result<ChatLine, ChatError> {
        if room != LOBBY {
            return Err(ChatError::NoSuchRoom(room.to_string()));
        }
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
        let line = ChatLine {
            room: room.to_string(),
            from: from.to_string(),
            text: text.to_string(),
            at_unix_ms: chrono::Utc::now().timestamp_millis(),
        };
        {
            let mut h = self.history.write();
            if h.len() == SCROLLBACK {
                h.pop_front();
            }
            h.push_back(line.clone());
        }
        self.bus.publish(ServerEvent::Chat {
            room: line.room.clone(),
            from: line.from.clone(),
            text: line.text.clone(),
        });
        Ok(line)
    }

    /// Most recent `limit` lines, oldest first.
    pub fn history(&self, room: &str, limit: usize) -> Result<Vec<ChatLine>, ChatError> {
        if room != LOBBY {
            return Err(ChatError::NoSuchRoom(room.to_string()));
        }
        let h = self.history.read();
        let skip = h.len().saturating_sub(limit);
        Ok(h.iter().skip(skip).cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_validates_and_records() {
        let chat = ChatService::new(EventBus::default(), 32);
        assert!(matches!(
            chat.send("nowhere", "a", "hi"),
            Err(ChatError::NoSuchRoom(_))
        ));
        assert!(matches!(
            chat.send(LOBBY, "a", "   "),
            Err(ChatError::Empty)
        ));
        assert!(matches!(
            chat.send(LOBBY, "a", &"x".repeat(33)),
            Err(ChatError::TooLong { len: 33, max: 32 })
        ));

        chat.send(LOBBY, "alice", "hello  ").unwrap();
        let h = chat.history(LOBBY, 10).unwrap();
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].text, "hello"); // trailing whitespace trimmed
    }

    #[test]
    fn history_caps_and_orders() {
        let chat = ChatService::new(EventBus::default(), 64);
        for i in 0..600 {
            chat.send(LOBBY, "bot", &format!("line {i}")).unwrap();
        }
        let h = chat.history(LOBBY, 3).unwrap();
        assert_eq!(h.len(), 3);
        assert_eq!(h[2].text, "line 599"); // newest last
    }
}
