//! The Looking Glass **server browser**: a directory of public RabbitHole
//! servers a user can discover and connect to (PLAN §9 directory index).
//!
//! This module is the **pure, DOM-free** half — the row model plus a total
//! browse (filter + rank) function — so the ordering is host-tested. The real
//! directory data comes from a tracker's `INDEX`/`HEALTH` verbs; the SPA seeds
//! a [`sample_directory`] into `AppState` until that transport lands. The view
//! ([`ServerBrowser`](crate::components)) lives in [`crate::components`].

/// One directory entry: a public server and its latest health snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryServer {
    /// Human-facing server name.
    pub name: String,
    /// Connection endpoint (a `ws://`/`wss://` URL or `host:port`), what the
    /// login screen dials.
    pub endpoint: String,
    /// One-line description / theme of the server.
    pub description: String,
    /// Members currently online (from the last health observation).
    pub users_online: u32,
    /// 24-hour uptime, 0–100 %.
    pub uptime_pct: u8,
    /// Whether the tracker's most recent probe reached it.
    pub reachable: bool,
}

/// Browse the directory: keep entries matching `query` (case-insensitive
/// substring over name + description; empty = all), ranked for a "where should
/// I go" list — reachable servers first, then most-populated, then by name.
/// Total: never panics.
pub fn browse(servers: &[DirectoryServer], query: &str) -> Vec<DirectoryServer> {
    let q = query.trim().to_ascii_lowercase();
    let mut out: Vec<DirectoryServer> = servers
        .iter()
        .filter(|s| {
            q.is_empty()
                || s.name.to_ascii_lowercase().contains(&q)
                || s.description.to_ascii_lowercase().contains(&q)
        })
        .cloned()
        .collect();
    out.sort_by(|a, b| {
        b.reachable
            .cmp(&a.reachable)
            .then(b.users_online.cmp(&a.users_online))
            .then(a.name.cmp(&b.name))
    });
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
            users_online: users,
            uptime_pct: uptime,
            reachable,
        };
    vec![
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
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srv(name: &str, desc: &str, users: u32, uptime: u8, reachable: bool) -> DirectoryServer {
        DirectoryServer {
            name: name.into(),
            endpoint: format!("wss://{}.example", name.to_ascii_lowercase()),
            description: desc.into(),
            users_online: users,
            uptime_pct: uptime,
            reachable,
        }
    }

    #[test]
    fn ranks_reachable_then_populated_then_name() {
        let servers = vec![
            srv("Zeta", "quiet", 2, 90, true),
            srv("Down", "offline now", 99, 10, false),
            srv("Alpha", "busy hub", 40, 99, true),
            srv("Beta", "busy too", 40, 95, true),
        ];
        let order: Vec<String> = browse(&servers, "")
            .iter()
            .map(|s| s.name.clone())
            .collect();
        // Reachable first; among reachable, more users first; Alpha before Beta
        // on the name tiebreak at equal population; the unreachable one last.
        assert_eq!(order, ["Alpha", "Beta", "Zeta", "Down"]);
    }

    #[test]
    fn filter_matches_name_and_description_case_insensitively() {
        let servers = vec![
            srv("Warren", "cozy ANSI art bbs", 5, 100, true),
            srv("Hollow", "fast files hub", 8, 100, true),
        ];
        assert_eq!(browse(&servers, "ART").len(), 1);
        assert_eq!(browse(&servers, "art")[0].name, "Warren");
        assert_eq!(browse(&servers, "hub")[0].name, "Hollow");
        assert_eq!(browse(&servers, "   ").len(), 2, "blank = all");
        assert!(browse(&servers, "nope").is_empty());
    }

    #[test]
    fn uptime_label_clamps() {
        assert_eq!(uptime_label(98), "98% up");
        assert_eq!(uptime_label(200), "100% up");
    }
}
