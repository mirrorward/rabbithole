//! The ⌘K command palette: fuzzy keyboard jump between the SPA's sections.
//!
//! This module is the **pure, DOM-free** half — a static catalog of the app's
//! destinations plus a total matcher — so the ranking is host-tested without a
//! browser. The overlay component (focus-trap, Escape, `⌘K`/`Ctrl-K` binding)
//! lives in [`crate::components`] and drives navigation off these results.

/// One reachable destination: a nav label, its route, a one-word hint shown on
/// the right, and alias terms the matcher also searches (so "members" finds
/// Directory, "music" finds Radio).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Section {
    pub label: &'static str,
    pub route: &'static str,
    pub hint: &'static str,
    pub aliases: &'static [&'static str],
}

/// Every jump target, in nav order. An empty query lists them all as-is.
pub const SECTIONS: &[Section] = &[
    Section {
        label: "Lobby",
        route: "/lobby",
        hint: "chat",
        aliases: &["chat", "home", "talk"],
    },
    Section {
        label: "Boards",
        route: "/boards",
        hint: "forums",
        aliases: &["forums", "messages", "threads", "bbs"],
    },
    Section {
        label: "DMs",
        route: "/dms",
        hint: "direct",
        aliases: &["direct", "mail", "private", "messages"],
    },
    Section {
        label: "Directory",
        route: "/directory",
        hint: "members",
        aliases: &["members", "users", "people", "who"],
    },
    Section {
        label: "Files",
        route: "/files",
        hint: "library",
        aliases: &["library", "downloads", "warez", "uploads"],
    },
    Section {
        label: "Radio",
        route: "/radio",
        hint: "stream",
        aliases: &["music", "stream", "tunes", "listen"],
    },
    Section {
        label: "Art",
        route: "/art",
        hint: "gallery",
        aliases: &["gallery", "ansi", "images"],
    },
    Section {
        label: "Admin",
        route: "/admin",
        hint: "operator",
        aliases: &["settings", "config", "operator", "moderate"],
    },
];

/// Rank of a section against a lowercased, non-empty query. Lower is better;
/// `None` means no match. A label prefix beats a label substring beats an
/// alias hit, so typing "d" surfaces Directory/DMs before it surfaces the
/// "downloads" alias of Files.
fn score(section: &Section, query: &str) -> Option<u8> {
    let label = section.label.to_ascii_lowercase();
    if label.starts_with(query) {
        return Some(0);
    }
    if label.contains(query) {
        return Some(1);
    }
    let alias_prefix = section
        .aliases
        .iter()
        .any(|a| a.to_ascii_lowercase().starts_with(query));
    if alias_prefix {
        return Some(2);
    }
    let alias_sub = section
        .aliases
        .iter()
        .any(|a| a.to_ascii_lowercase().contains(query));
    alias_sub.then_some(3)
}

/// Sections matching `query`, best first. An empty/whitespace query returns the
/// full catalog in nav order. Total: never panics, always defined.
pub fn palette_matches(query: &str) -> Vec<Section> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return SECTIONS.to_vec();
    }
    // Stable sort by score keeps nav order among equal-ranked hits.
    let mut scored: Vec<(u8, usize, Section)> = SECTIONS
        .iter()
        .enumerate()
        .filter_map(|(i, s)| score(s, &q).map(|r| (r, i, *s)))
        .collect();
    scored.sort_by_key(|(rank, idx, _)| (*rank, *idx));
    scored.into_iter().map(|(_, _, s)| s).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_lists_every_section_in_nav_order() {
        let all = palette_matches("");
        assert_eq!(all.len(), SECTIONS.len());
        assert_eq!(all[0].label, "Lobby");
        assert_eq!(all.last().unwrap().label, "Admin");
        // Whitespace is treated as empty.
        assert_eq!(palette_matches("   ").len(), SECTIONS.len());
    }

    #[test]
    fn label_prefix_outranks_substring_and_alias() {
        // "d" prefixes Directory and DMs (rank 0); it is also a substring of
        // nothing else, but an alias prefix of Files ("downloads"). Prefixes
        // come first, in nav order (DMs is defined before Directory... check).
        let hits = palette_matches("d");
        let labels: Vec<&str> = hits.iter().map(|s| s.label).collect();
        // Directory + DMs (label-prefix, rank 0) precede Files (alias "downloads").
        let d_pos = labels.iter().position(|l| *l == "Directory").unwrap();
        let dm_pos = labels.iter().position(|l| *l == "DMs").unwrap();
        let files_pos = labels.iter().position(|l| *l == "Files").unwrap();
        assert!(d_pos < files_pos && dm_pos < files_pos);
    }

    #[test]
    fn aliases_find_sections_by_synonym() {
        assert_eq!(palette_matches("members")[0].label, "Directory");
        assert_eq!(palette_matches("music")[0].label, "Radio");
        assert_eq!(palette_matches("gallery")[0].label, "Art");
        assert_eq!(palette_matches("settings")[0].label, "Admin");
    }

    #[test]
    fn is_case_insensitive_and_total() {
        assert_eq!(palette_matches("RADIO")[0].label, "Radio");
        assert_eq!(palette_matches("Lob")[0].label, "Lobby");
        // A query that matches nothing is empty, not a panic.
        assert!(palette_matches("zzzznope").is_empty());
    }

    #[test]
    fn substring_matches_mid_label() {
        // "ire" is inside "Directory" but prefixes nothing.
        let hits = palette_matches("ire");
        assert_eq!(hits[0].label, "Directory");
    }
}
