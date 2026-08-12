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

/// Which nav a destination belongs to.
///
/// The rail picks a **scope**; the sidebar lists that scope's sections. A
/// burrow's sections (its lobby, its boards, its files) mean nothing while
/// you're looking at transfers across every burrow you're connected to, so
/// showing them there was just a list of wrong links.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Inside one burrow: the place you're connected to.
    Burrow,
    /// Across every burrow: the warren layer.
    Warren,
}

/// The sections of one burrow, in sidebar order.
///
/// Admin is deliberately absent: it only exists for operators, so the nav
/// renders it conditionally after this list rather than having every reader
/// filter it out.
pub const BURROW_SECTIONS: &[Section] = &[
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
];

/// The warren's own sections — everything that spans burrows rather than
/// belonging to one, in sidebar order.
pub const WARREN_SECTIONS: &[Section] = &[
    Section {
        label: "People",
        route: "/people",
        hint: "everyone",
        aliases: &["everyone", "friends", "roster", "contacts"],
    },
    Section {
        label: "Transfers",
        route: "/transfers",
        hint: "queue",
        aliases: &["downloads", "uploads", "queue", "progress"],
    },
    Section {
        label: "You",
        route: "/you",
        hint: "identity",
        aliases: &["identity", "key", "profile", "me", "account"],
    },
    Section {
        label: "Servers",
        route: "/servers",
        hint: "directory",
        aliases: &["directory", "looking glass", "browse", "hubs", "explore"],
    },
];

/// The operator console. Reachable by search for anyone who can see it, but
/// never part of a scope's list — see [`BURROW_SECTIONS`].
pub const ADMIN_SECTION: Section = Section {
    label: "Admin",
    route: "/admin",
    hint: "operator",
    aliases: &["settings", "config", "operator", "moderate"],
};

/// The scope a route belongs to. Anything not explicitly warren-level is a
/// burrow route, so a new burrow section gets the burrow sidebar by default
/// rather than silently getting the wrong one.
pub fn scope_of(path: &str) -> Scope {
    let path = path.trim_end_matches('/');
    // Sub-paths belong to their parent: /people/<seed> is a person page, still
    // a warren view, and must not sprout a burrow sidebar.
    if WARREN_SECTIONS
        .iter()
        .any(|s| path == s.route || path.starts_with(&format!("{}/", s.route)))
    {
        Scope::Warren
    } else {
        Scope::Burrow
    }
}

/// The sections the sidebar lists for a scope.
pub fn sections_for(scope: Scope) -> &'static [Section] {
    match scope {
        Scope::Burrow => BURROW_SECTIONS,
        Scope::Warren => WARREN_SECTIONS,
    }
}

/// Every jump target the palette searches: both scopes plus the operator
/// console. Built from the same lists the sidebars render, so a section can't
/// exist in a nav and be unreachable by search — or the reverse.
pub fn all_sections() -> Vec<Section> {
    BURROW_SECTIONS
        .iter()
        .chain(WARREN_SECTIONS)
        .copied()
        .chain(std::iter::once(ADMIN_SECTION))
        .collect()
}

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

/// The section a Cmd/Ctrl + digit shortcut jumps to.
///
/// `1` is the first row of the burrow sidebar — the only sidebar there is,
/// since the warren destinations are single screens with none. From a warren
/// view the shortcut therefore jumps back *into* the focused burrow, which is
/// also what returning to `1`–`9` means everywhere else. Out-of-range digits
/// and `0` do nothing rather than wrapping to something arbitrary.
pub fn section_for_digit(key: &str) -> Option<Section> {
    let n = key.parse::<usize>().ok()?;
    if n == 0 {
        return None;
    }
    BURROW_SECTIONS.get(n - 1).copied()
}

/// Sections matching `query`, best first. An empty/whitespace query returns the
/// full catalog in nav order. Total: never panics, always defined.
pub fn palette_matches(query: &str) -> Vec<Section> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return all_sections();
    }
    // Stable sort by score keeps nav order among equal-ranked hits.
    let mut scored: Vec<(u8, usize, Section)> = all_sections()
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
    fn digit_shortcuts_follow_the_burrow_sidebar() {
        // Cmd-1 is the first row of the burrow sidebar -- the only sidebar
        // there is. The shortcut is only learnable if those two agree.
        assert_eq!(section_for_digit("1").unwrap().route, BURROW_SECTIONS[0].route);
        assert_eq!(section_for_digit("1").unwrap().label, "Lobby");
        assert_eq!(section_for_digit("2").unwrap().route, BURROW_SECTIONS[1].route);
        // Nothing silly at the edges: no wrap-around, no Cmd-0.
        assert_eq!(section_for_digit("0"), None);
        assert_eq!(
            section_for_digit(&format!("{}", BURROW_SECTIONS.len() + 1)),
            None
        );
        assert_eq!(section_for_digit("k"), None);
        assert_eq!(section_for_digit(""), None);
    }

    #[test]
    fn every_route_belongs_to_exactly_one_scope() {
        // The sidebar is chosen by this function, so a route that lands in the
        // wrong scope shows a whole nav of links that don't apply.
        for s in WARREN_SECTIONS {
            assert_eq!(scope_of(s.route), Scope::Warren, "{}", s.route);
        }
        for s in BURROW_SECTIONS {
            assert_eq!(scope_of(s.route), Scope::Burrow, "{}", s.route);
        }
        // Sub-routes stay with their parent, and a trailing slash is the same
        // route -- both arrive from the router.
        assert_eq!(scope_of("/boards/general"), Scope::Burrow);
        assert_eq!(scope_of("/transfers/"), Scope::Warren);
        // The person page is a warren view: no burrow sidebar beside it.
        assert_eq!(scope_of("/people/abc123"), Scope::Warren);
        // ...but a route that merely starts with the same letters is not.
        assert_eq!(scope_of("/peoplezoo"), Scope::Burrow);
        // Admin is an operator view inside a burrow, and an unknown route
        // defaults to the burrow sidebar rather than the warren one.
        assert_eq!(scope_of(ADMIN_SECTION.route), Scope::Burrow);
        assert_eq!(scope_of("/wishing-well"), Scope::Burrow);
    }

    #[test]
    fn the_search_catalog_is_exactly_what_the_navs_render() {
        // Built from the same lists, so a section can't be in a nav but
        // unreachable by search, or searchable but in no nav.
        let all = all_sections();
        assert_eq!(all.len(), BURROW_SECTIONS.len() + WARREN_SECTIONS.len() + 1);
        for s in BURROW_SECTIONS.iter().chain(WARREN_SECTIONS) {
            assert!(all.iter().any(|a| a.route == s.route), "{} missing", s.route);
        }
        assert!(all.iter().any(|a| a.route == ADMIN_SECTION.route));
        // No route appears twice, or the palette would list it twice.
        let mut routes: Vec<&str> = all.iter().map(|s| s.route).collect();
        routes.sort_unstable();
        let before = routes.len();
        routes.dedup();
        assert_eq!(routes.len(), before, "a route is in two navs");
    }

    #[test]
    fn empty_query_lists_every_section_in_nav_order() {
        let all = palette_matches("");
        assert_eq!(all.len(), all_sections().len());
        assert_eq!(all[0].label, "Lobby");
        assert_eq!(all.last().unwrap().label, "Admin");
        // The warren destinations are searchable too -- before this they were
        // rail-only, so Cmd-K couldn't reach People, Transfers or You at all.
        for label in ["People", "Transfers", "You"] {
            assert!(all.iter().any(|s| s.label == label), "{label} unreachable");
        }
        // Whitespace is treated as empty.
        assert_eq!(palette_matches("   ").len(), all_sections().len());
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
