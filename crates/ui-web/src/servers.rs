//! The Looking Glass **server browser**: a directory of public RabbitHole
//! burrows a user can discover and connect to (PLAN §9 directory index).
//!
//! The row model, the browse (filter + rank) function and both source parsers
//! live in [`rabbithole_directory`] — shared verbatim with the terminal
//! clients, because two clients asking the same two services the same question
//! should not drift into two answers. This module re-exports them and keeps
//! what is genuinely the SPA's own: query-string encoding, the health-chip
//! label, and the dev sample list.
//!
//! The view ([`ServerBrowser`](crate::components)) lives in
//! [`crate::components`].

pub use rabbithole_directory::{
    browse, parse_directory_json, parse_glass_json, parse_tracker_index, DirectoryServer,
    DirectorySource, DIRECTORY_URL, TRACKER_HOST, TRACKER_STATUS_PORT, TRACKER_URL,
};

/// Percent-encode a value for a query string. Only the characters that would
/// otherwise break the URL — enough for endpoints, which are
/// `scheme://host:port` and nothing exotic.
pub fn encode_param(v: &str) -> String {
    let mut out = String::with_capacity(v.len() + 8);
    for b in v.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// A short, human `"98% up"` label for the health chip.
pub fn uptime_label(pct: u8) -> String {
    format!("{}% up", pct.min(100))
}

/// A sample Looking Glass directory for dev, so the server browser renders
/// without a live tracker. The real transport replaces this with `INDEX`/
/// `HEALTH` rows.
pub fn sample_directory() -> Vec<DirectoryServer> {
    let s =
        |name: &str, endpoint: &str, description: &str, users, uptime, reachable| DirectoryServer {
            name: name.into(),
            endpoint: endpoint.into(),
            description: description.into(),
            users_online: Some(users),
            listeners: Vec::new(),
            uptime_pct: Some(uptime),
            reachable,
        };
    #[allow(unused_mut)]
    let mut list = vec![
        s(
            "The Warren",
            "wss://warren.rabbithole.example",
            "Flagship hub — chat, boards, ANSI art gallery, and pirate radio.",
            214,
            99,
            true,
        ),
        s(
            "Down the Hole",
            "wss://hole.example:9000",
            "Retro BBS revival: CP437 art, door games, QWK mail.",
            63,
            98,
            true,
        ),
        s(
            "Briar Patch",
            "wss://briar.example",
            "Files-first warren with a fast swarm and NNTP bridge.",
            41,
            100,
            true,
        ),
        s(
            "Moonlit Burrow",
            "wss://moonlit.example",
            "Small, quiet, invite-only community. Night owls welcome.",
            7,
            92,
            true,
        ),
        s(
            "Thornfield",
            "wss://thornfield.example",
            "Federated art + music collective (currently rebooting).",
            0,
            34,
            false,
        ),
    ];
    // Dev builds list the seeded demo burrows first, so "+ a burrow" reaches
    // them without typing an address — the only way to exercise the warren
    // layer (switching places, per-burrow unread) without running two servers.
    #[cfg(feature = "demo")]
    {
        let demos: Vec<DirectoryServer> = crate::client::DEMO_BURROWS
            .iter()
            .map(|d| DirectoryServer {
                name: d.name.into(),
                endpoint: d.endpoint.into(),
                description: format!("Demo burrow \u{2014} {}", d.motd),
                users_online: Some(d.who.len() as u32),
                listeners: vec!["ws".to_string()],
                uptime_pct: Some(100),
                reachable: true,
            })
            .collect();
        list.splice(0..0, demos);
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_survive_the_query_string() {
        // The Looking Glass hands its pick to the connect screen through the
        // URL, so an endpoint's separators must not end the parameter.
        assert_eq!(
            encode_param("wss://warren.example:9000"),
            "wss%3A%2F%2Fwarren.example%3A9000"
        );
        assert_eq!(encode_param("demo://night-pool"), "demo%3A%2F%2Fnight-pool");
        assert_eq!(
            encode_param("a-b_c.d~e"),
            "a-b_c.d~e",
            "unreserved stays readable"
        );
    }

    #[test]
    fn uptime_label_clamps() {
        assert_eq!(uptime_label(98), "98% up");
        assert_eq!(uptime_label(200), "100% up");
    }
}
