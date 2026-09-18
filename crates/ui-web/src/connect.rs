//! The connect window: what its burrow browser lists, and what it says.
//!
//! Hotline opened on a tracker list and a row of bookmarks. This is that
//! window, reinterpreted the way the design spec asks (§3, "Tracker +
//! bookmarks"): *Your burrows* over *Discover*, one dense list, and a status
//! strip that says where the listing came from and how many people are out
//! there tonight.
//!
//! Everything here is pure, so the rules that matter are host-tested rather
//! than hoped for: a fabricated sample row is never offered as a place to
//! connect to, a saved burrow is never listed twice, and the census counts
//! only what a source actually reported.

use crate::recent::RecentBurrow;
use crate::servers::{DirectoryServer, DirectorySource};

/// Which shelf a row sits on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shelf {
    /// A burrow you have signed into before, with the handle you used there.
    Yours { handle: String },
    /// A burrow a directory or a Looking Glass lists.
    Listed,
    /// A seeded demo burrow. Dev builds only.
    Demo,
}

/// One line of the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub shelf: Shelf,
    pub name: String,
    pub endpoint: String,
    pub description: String,
    /// People online, when a source reported it.
    pub users: Option<u32>,
    /// 24-hour uptime, when a source reported it.
    pub uptime: Option<u8>,
    /// `None` when nobody has said: a saved burrow that no directory lists.
    pub reachable: Option<bool>,
    pub listeners: Vec<String>,
}

impl Row {
    /// A key that changes whenever anything the row draws changes, so a
    /// refreshed listing re-renders the rows it touched and no others.
    pub fn key(&self) -> String {
        format!(
            "{:?}|{}|{}|{}|{:?}|{:?}|{:?}",
            self.shelf,
            self.endpoint,
            self.name,
            self.description,
            self.users,
            self.uptime,
            self.reachable
        )
    }
}

/// An endpoint without its scheme or trailing slash: what a person would call
/// the address.
pub fn host(endpoint: &str) -> String {
    endpoint
        .trim()
        .trim_start_matches("wss://")
        .trim_start_matches("ws://")
        .trim_end_matches('/')
        .to_string()
}

/// Do two endpoints name the same place? Scheme and case aside: a burrow
/// saved as `ws://` and listed as `wss://` is still one burrow, and listing
/// it on both shelves would be the browser contradicting itself.
pub fn same_place(a: &str, b: &str) -> bool {
    let (a, b) = (host(a), host(b));
    !a.is_empty() && a.eq_ignore_ascii_case(&b)
}

/// Is this the pseudo-endpoint of a seeded demo burrow?
pub fn is_demo(endpoint: &str) -> bool {
    endpoint.trim().starts_with("demo://")
}

/// *Your burrows*: every burrow you have signed into, newest first, wearing
/// the name and health a directory reports for it when one does. A burrow
/// nobody lists still belongs here, under its address.
pub fn yours(recent: &[RecentBurrow], listed: &[DirectoryServer]) -> Vec<Row> {
    recent
        .iter()
        .map(|saved| {
            let known = listed
                .iter()
                .find(|s| same_place(&s.endpoint, &saved.endpoint));
            Row {
                shelf: Shelf::Yours {
                    handle: saved.handle.clone(),
                },
                name: known
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| host(&saved.endpoint)),
                endpoint: saved.endpoint.clone(),
                description: known.map(|s| s.description.clone()).unwrap_or_default(),
                users: known.and_then(|s| s.users_online),
                uptime: known.and_then(|s| s.uptime_pct),
                reachable: known.map(|s| s.reachable),
                listeners: known.map(|s| s.listeners.clone()).unwrap_or_default(),
            }
        })
        .collect()
}

/// *Discover*: what the live source lists, in the directory's own ranking
/// (reachable first, then busiest, then by name), minus the places already on
/// your shelf.
///
/// The built-in sample is never offered. It exists so the in-app browser has
/// something to draw in development; on the first screen a person sees, a row
/// for `wss://warren.rabbithole.example` is the client inventing a place.
pub fn discover(
    listed: &[DirectoryServer],
    source: &DirectorySource,
    recent: &[RecentBurrow],
) -> Vec<Row> {
    if *source == DirectorySource::Seeded {
        return Vec::new();
    }
    crate::servers::browse(listed, "")
        .into_iter()
        .filter(|s| !is_demo(&s.endpoint))
        .filter(|s| !recent.iter().any(|r| same_place(&r.endpoint, &s.endpoint)))
        .map(|s| Row {
            shelf: Shelf::Listed,
            name: s.name,
            endpoint: s.endpoint,
            description: s.description,
            users: s.users_online,
            uptime: s.uptime_pct,
            reachable: Some(s.reachable),
            listeners: s.listeners,
        })
        .collect()
}

/// The seeded demo burrows, for dev builds. Empty in a shipped build, which
/// has no demo to join.
pub fn demos() -> Vec<Row> {
    #[cfg(feature = "demo")]
    {
        crate::client::DEMO_BURROWS
            .iter()
            .map(|d| Row {
                shelf: Shelf::Demo,
                name: d.name.to_string(),
                endpoint: d.endpoint.to_string(),
                description: d.motd.to_string(),
                users: Some(d.who.len() as u32),
                uptime: None,
                reachable: Some(true),
                listeners: Vec::new(),
            })
            .collect()
    }
    #[cfg(not(feature = "demo"))]
    Vec::new()
}

/// Keep the rows a filter matches: a case-insensitive substring over the
/// name, the description, the address, and (on your shelf) the handle.
pub fn filter(rows: Vec<Row>, query: &str) -> Vec<Row> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return rows;
    }
    rows.into_iter()
        .filter(|r| {
            let handle = match &r.shelf {
                Shelf::Yours { handle } => handle.as_str(),
                _ => "",
            };
            [
                r.name.as_str(),
                r.description.as_str(),
                &host(&r.endpoint),
                handle,
            ]
            .iter()
            .any(|field| field.to_ascii_lowercase().contains(&q))
        })
        .collect()
}

/// The status strip under the list. Says what is true and nothing more:
/// a count of people appears only when some source reported one.
pub fn status_line(loading: bool, source: &DirectorySource, listed: &[DirectoryServer]) -> String {
    if loading {
        return "Asking the looking glass\u{2026}".to_string();
    }
    if *source == DirectorySource::Seeded {
        return "No directory answered.".to_string();
    }
    let burrows = listed.iter().filter(|s| !is_demo(&s.endpoint)).count();
    if burrows == 0 {
        return "No burrows are listed right now.".to_string();
    }
    let people: Option<u32> = listed
        .iter()
        .filter(|s| s.reachable)
        .filter_map(|s| s.users_online)
        .fold(None, |sum, n| Some(sum.unwrap_or(0) + n));
    let burrows = if burrows == 1 {
        "1 burrow".to_string()
    } else {
        format!("{burrows} burrows")
    };
    match people {
        Some(1) => format!("{burrows}, 1 person online"),
        Some(n) => format!("{burrows}, {n} people online"),
        None => burrows,
    }
}

/// The primary button names where it is going when the address is a place
/// the browser knows, the way a Mac's default button names its action.
pub fn connect_label(endpoint: &str, rows: &[Row]) -> String {
    rows.iter()
        .find(|r| r.endpoint == endpoint || same_place(&r.endpoint, endpoint))
        .map(|r| format!("Connect to {}", r.name))
        .unwrap_or_else(|| "Connect".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(name: &str, endpoint: &str, users: Option<u32>, reachable: bool) -> DirectoryServer {
        DirectoryServer {
            name: name.into(),
            endpoint: endpoint.into(),
            description: format!("{name} is a place"),
            users_online: users,
            listeners: vec!["ws".into()],
            uptime_pct: Some(99),
            reachable,
        }
    }

    fn saved(endpoint: &str, handle: &str) -> RecentBurrow {
        RecentBurrow {
            endpoint: endpoint.into(),
            handle: handle.into(),
            token: None,
        }
    }

    #[test]
    fn a_saved_burrow_wears_the_directory_name_or_its_own_address() {
        let glass = [listed("The Warren", "wss://warren.example", Some(12), true)];
        let rows = yours(
            &[
                saved("ws://WARREN.example/", "alice"),
                saved("ws://localhost:4654", "test"),
            ],
            &glass,
        );
        assert_eq!(rows[0].name, "The Warren", "scheme, case and slash aside");
        assert_eq!(rows[0].users, Some(12));
        assert_eq!(rows[0].reachable, Some(true));
        assert_eq!(
            rows[0].shelf,
            Shelf::Yours {
                handle: "alice".into()
            }
        );
        // Nobody lists a burrow on this machine. It is still yours, and what
        // is not known about it stays unknown rather than reading "offline".
        assert_eq!(rows[1].name, "localhost:4654");
        assert_eq!(rows[1].reachable, None);
        assert_eq!(rows[1].users, None);
    }

    #[test]
    fn the_built_in_sample_is_never_offered_as_a_place() {
        let sample = crate::servers::sample_directory();
        assert!(!sample.is_empty());
        assert!(discover(&sample, &DirectorySource::Seeded, &[]).is_empty());
    }

    #[test]
    fn discover_keeps_the_ranking_and_skips_what_is_already_yours() {
        let glass = [
            listed("Quiet", "wss://quiet.example", Some(2), true),
            listed("Down", "wss://down.example", Some(90), false),
            listed("Busy", "wss://busy.example", Some(40), true),
            listed("Mine", "wss://mine.example", Some(7), true),
            listed("Pretend", "demo://pretend", Some(3), true),
        ];
        let rows = discover(
            &glass,
            &DirectorySource::Directory,
            &[saved("ws://mine.example", "me")],
        );
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["Busy", "Quiet", "Down"]);
        assert_eq!(rows[2].reachable, Some(false));
    }

    #[test]
    fn the_filter_reads_names_descriptions_addresses_and_handles() {
        let glass = [listed("The Warren", "wss://warren.example", None, true)];
        let all = || {
            let mut rows = yours(&[saved("ws://localhost:4654", "Bramble")], &glass);
            rows.extend(discover(&glass, &DirectorySource::Directory, &[]));
            rows
        };
        assert_eq!(filter(all(), "  ").len(), 2, "blank keeps everything");
        assert_eq!(filter(all(), "WARREN")[0].name, "The Warren");
        assert_eq!(filter(all(), "4654")[0].name, "localhost:4654");
        assert_eq!(filter(all(), "bramble")[0].name, "localhost:4654");
        assert_eq!(filter(all(), "is a place")[0].name, "The Warren");
        assert!(filter(all(), "nowhere").is_empty());
    }

    #[test]
    fn the_status_line_counts_only_what_was_reported() {
        let live = DirectorySource::Directory;
        assert_eq!(
            status_line(true, &live, &[]),
            "Asking the looking glass\u{2026}"
        );
        assert_eq!(
            status_line(
                false,
                &DirectorySource::Seeded,
                &crate::servers::sample_directory()
            ),
            "No directory answered."
        );
        assert_eq!(
            status_line(false, &live, &[]),
            "No burrows are listed right now."
        );
        // A source that publishes no population gets no invented "0 online".
        assert_eq!(
            status_line(false, &live, &[listed("A", "wss://a.example", None, true)]),
            "1 burrow"
        );
        // An unreachable burrow's last count is not people online now.
        let glass = [
            listed("A", "wss://a.example", Some(1), true),
            listed("B", "wss://b.example", Some(50), false),
        ];
        assert_eq!(
            status_line(false, &live, &glass),
            "2 burrows, 1 person online"
        );
        let glass = [
            listed("A", "wss://a.example", Some(12), true),
            listed("B", "wss://b.example", Some(30), true),
            listed("C", "wss://c.example", None, true),
        ];
        assert_eq!(
            status_line(false, &live, &glass),
            "3 burrows, 42 people online"
        );
    }

    #[test]
    fn the_button_names_where_it_is_going() {
        let glass = [listed("The Warren", "wss://warren.example", None, true)];
        let rows = discover(&glass, &DirectorySource::Directory, &[]);
        assert_eq!(
            connect_label("wss://warren.example", &rows),
            "Connect to The Warren"
        );
        assert_eq!(
            connect_label("ws://warren.example/", &rows),
            "Connect to The Warren"
        );
        assert_eq!(connect_label("ws://localhost:4654", &rows), "Connect");
        assert_eq!(connect_label("", &rows), "Connect");
    }

    #[test]
    fn demo_endpoints_are_recognised_and_shipped_builds_have_none() {
        assert!(is_demo("demo://the-warren"));
        assert!(!is_demo("ws://localhost:4654"));
        assert!(!is_demo("wss://demo.example"));
        if cfg!(feature = "demo") {
            assert!(demos().iter().all(|r| is_demo(&r.endpoint)));
            assert!(!demos().is_empty());
        } else {
            assert!(demos().is_empty());
        }
    }
}
