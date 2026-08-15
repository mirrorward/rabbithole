//! Finding burrows: the client half of Looking Glass discovery.
//!
//! There are two places to ask who is out there, and they answer differently:
//!
//! * **`rabbithole.directory`** — HTTPS, `GET /api/burrows`, a JSON snapshot
//!   aggregated from every Looking Glass that publishes to it. The widest view,
//!   and the one with CORS open so a browser tab can read it directly.
//! * **A Looking Glass** — `tracker.rabbit.direct` by default, HTTPS
//!   `GET /api/burrows`. Narrower (one coordinator's own announces) and a
//!   *different shape*: a glass relays announced descriptors, so endpoints are
//!   a nested `endpoints: {quic, ws}` object rather than the directory's flat
//!   `wsUri` / `quicUri`, and it reports liveness rather than uptime.
//! * **A self-hosted tracker's status port** — the `looking-glass` binary in
//!   this workspace also serves a line-oriented TCP protocol on port 4655
//!   (`INDEX` in, tab-separated rows out). A browser tab has no TCP, so this
//!   one is native-only, and it is what a user gets when they name their own
//!   coordinator.
//!
//! So: directory first, the standard glass behind it, and the UI says which
//! answered — "who told you this" is part of the answer to "who is out there".
//!
//! # Why this is its own crate
//!
//! The wasm SPA and the terminal clients need exactly the same parsers.
//! The pure half here therefore has **no dependencies at all** and compiles for
//! both; the network edge is behind the `native` feature, since a browser tab
//! has no TCP and reaches the directory through `fetch` instead.
//!
//! # Totality
//!
//! The parsers never panic. A malformed tracker row is skipped rather than
//! failing the listing (a tracker that grows a column must not blank the
//! browser); a reply with nothing usable in it becomes an `Err` with something
//! a person can read.

#![forbid(unsafe_code)]

#[cfg(feature = "native")]
pub mod fetch;
pub mod json;

/// One directory entry: a public burrow and its latest health snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryServer {
    /// Human-facing burrow name.
    pub name: String,
    /// Connection endpoint (a `ws://`/`wss://` URL or `host:port`), what the
    /// login screen dials.
    pub endpoint: String,
    /// One-line description / theme of the burrow.
    pub description: String,
    /// Members currently online, when the source reports it. `None` where it
    /// doesn't — rabbithole.directory publishes uptime and listeners but no
    /// population, and rendering a confident "0 online" for "not reported"
    /// would be the client inventing a fact.
    pub users_online: Option<u32>,
    /// The protocols this burrow listens on (`quic`, `ws`, `telnet`…), when
    /// the source says. Empty when unknown.
    pub listeners: Vec<String>,
    /// 24-hour uptime, 0–100 %, when the source reports it. `None` where it
    /// doesn't — a Looking Glass publishes liveness, not a history, and
    /// rendering "0% up" for "not reported" would be the client inventing one.
    pub uptime_pct: Option<u8>,
    /// Whether the source's most recent probe reached it.
    pub reachable: bool,
}

/// Browse the directory: keep entries matching `query` (case-insensitive
/// substring over name + description; empty = all), ranked for a "where should
/// I go" list — reachable burrows first, then most-populated, then by name.
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
            .then(
                b.users_online
                    .unwrap_or(0)
                    .cmp(&a.users_online.unwrap_or(0)),
            )
            .then(a.name.cmp(&b.name))
    });
    out
}

/// Where a listing came from — shown to the user, because a narrower source
/// answering is a different answer, not the same one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectorySource {
    /// `rabbithole.directory` over HTTPS.
    Directory,
    /// A Looking Glass tracker's status port (the `INDEX` line protocol).
    Tracker,
    /// The built-in sample list — nothing reachable answered.
    Seeded,
}

impl DirectorySource {
    pub fn label(self) -> &'static str {
        match self {
            DirectorySource::Directory => "rabbithole.directory",
            DirectorySource::Tracker => "tracker.rabbit.direct",
            DirectorySource::Seeded => "built-in sample \u{2014} no directory reachable",
        }
    }
}

/// The directory's HTTPS endpoint. Returns `{ok, burrows: [...]}` with CORS
/// open, so the browser build can read it directly.
pub const DIRECTORY_URL: &str = "https://rabbithole.directory/api/burrows";

/// The standard Looking Glass, and the fallback when the directory is
/// unreachable.
pub const TRACKER_HOST: &str = "tracker.rabbit.direct";
/// The standard glass's HTTPS listing endpoint (a different shape from the
/// directory's — see [`parse_glass_json`]).
pub const TRACKER_URL: &str = "https://tracker.rabbit.direct/api/burrows";
/// A self-hosted `looking-glass`'s line-oriented TCP status port. Native only:
/// a browser tab has no TCP.
pub const TRACKER_STATUS_PORT: u16 = 4655;

/// Parse `rabbithole.directory`'s JSON into directory rows.
///
/// A burrow with no `wsUri` is skipped — this parser serves WebSocket clients,
/// and a row you cannot connect to is a row that only wastes a click. Native
/// callers that speak QUIC pass `["wsUri", "quicUri"]` to
/// [`parse_directory_json_with`] and keep those rows.
pub fn parse_directory_json(text: &str) -> Result<Vec<DirectoryServer>, String> {
    parse_directory_json_with(text, &["wsUri"])
}

/// Like [`parse_directory_json`], but choosing which URI fields count as a
/// dialable endpoint, in preference order.
pub fn parse_directory_json_with(
    text: &str,
    endpoint_fields: &[&str],
) -> Result<Vec<DirectoryServer>, String> {
    let doc = json::parse(text).map_err(|e| format!("That directory reply isn't JSON: {e}"))?;
    let rows = doc.arr_field("burrows");
    if rows.is_empty() && doc.get("burrows").is_none() {
        return Err("That directory reply has no burrows list.".to_string());
    }
    let out: Vec<DirectoryServer> = rows
        .iter()
        .filter_map(|row| {
            // The directory publishes flat `wsUri` / `quicUri` fields.
            let endpoint = endpoint_fields
                .iter()
                .find_map(|f| row.str_field(f).filter(|u| !u.is_empty()))?;
            Some(DirectoryServer {
                name: row.str_field("name").unwrap_or(endpoint).to_string(),
                endpoint: endpoint.to_string(),
                description: row.str_field("description").unwrap_or_default().to_string(),
                // The directory publishes uptime and listeners, not population.
                users_online: None,
                listeners: row.str_array_field("listeners"),
                uptime_pct: percent(row.str_field("uptime")),
                reachable: row.str_field("status") == Some("online"),
            })
        })
        .collect();
    if out.is_empty() {
        return Err("The directory listed no burrows this client can dial.".to_string());
    }
    Ok(out)
}

/// Parse a **Looking Glass** `/api/burrows` reply.
///
/// A different shape from the directory's, not a variant of it: a glass row
/// carries the announced descriptor more or less verbatim, so its endpoints
/// are a nested `endpoints: {quic, ws}` object rather than flat `wsUri` /
/// `quicUri` fields, and it publishes no uptime at all. `endpoint_kinds` are
/// the keys inside `endpoints`, in preference order (`["ws", "quic"]`).
pub fn parse_glass_json(
    text: &str,
    endpoint_kinds: &[&str],
) -> Result<Vec<DirectoryServer>, String> {
    let doc = json::parse(text).map_err(|e| format!("That tracker reply isn't JSON: {e}"))?;
    let rows = doc.arr_field("burrows");
    if rows.is_empty() && doc.get("burrows").is_none() {
        return Err("That tracker reply has no burrows list.".to_string());
    }
    let out: Vec<DirectoryServer> = rows
        .iter()
        .filter_map(|row| {
            let endpoints = row.get("endpoints")?;
            let endpoint = endpoint_kinds
                .iter()
                .find_map(|k| endpoints.str_field(k).filter(|u| !u.is_empty()))?;
            Some(DirectoryServer {
                name: row.str_field("name").unwrap_or(endpoint).to_string(),
                endpoint: endpoint.to_string(),
                // A glass row's blurb lives on the descriptor it relayed.
                description: row
                    .str_field("description")
                    .or_else(|| {
                        row.get("descriptor")
                            .and_then(|d| d.str_field("description"))
                    })
                    .unwrap_or_default()
                    .to_string(),
                users_online: None,
                listeners: row.str_array_field("listeners"),
                // A glass reports liveness, not a percentage. Claiming 0% or
                // 100% because it answered would be inventing a history.
                uptime_pct: None,
                reachable: row.str_field("status") == Some("online"),
            })
        })
        .collect();
    if out.is_empty() {
        return Err("The tracker listed no burrows this client can dial.".to_string());
    }
    Ok(out)
}

/// Does this burrow's `/.well-known/rabbithole/server` descriptor ask not to
/// be listed?
///
/// Discovery here is gossip: whoever visits a burrow can pass it along. A
/// burrow that doesn't want that says so inside its own **signed** descriptor,
/// as a `noindex` feature tag, so the wish is attributable to the burrow and
/// survives the retelling rather than depending on everyone who saw it.
///
/// Anything that would publish a burrow onward — a tracker relaying gossip, a
/// client sharing a discovery — should check this first. Note the asymmetry:
/// this governs *advertising*, not access. Someone who was handed the address
/// can still connect; the burrow simply isn't added to a public list.
///
/// A reply that can't be read is **not** treated as consent. An unreachable or
/// malformed descriptor returns `false` only because there is nothing to
/// honor; callers that can wait should retry rather than publish on the
/// strength of a failed fetch.
pub fn noindex_in_descriptor(descriptor_json: &str) -> bool {
    let Ok(doc) = json::parse(descriptor_json) else {
        return false;
    };
    // The document is `{body: {...}, signature: ...}`; older or flattened
    // shapes may put features at the top level.
    let features = doc
        .get("body")
        .map(|b| b.str_array_field("features"))
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| doc.str_array_field("features"));
    features.iter().any(|f| f == "noindex")
}

/// A `"99.8%"`-style uptime string as a 0–100 percentage. Absent or
/// unparseable is `None` rather than a invented 0 — the row still lists.
fn percent(raw: Option<&str>) -> Option<u8> {
    raw.and_then(|u| u.trim().trim_end_matches('%').parse::<f32>().ok())
        .map(|p| p.round().clamp(0.0, 100.0) as u8)
}

/// Parse a tracker `INDEX` reply: one tab-separated row per live burrow,
/// `name\tip:port\tusers\tcategories\tuptime\tlast_seen\tsigned\tkey\tgen`
/// (see `apps/tracker`). Short rows are skipped rather than failing the whole
/// listing — a tracker that grows a column must not blank the browser.
pub fn parse_tracker_index(text: &str) -> Result<Vec<DirectoryServer>, String> {
    if let Some(first) = text.lines().next() {
        let first = first.trim();
        if !first.contains('\t') && first.starts_with("ERR") {
            let msg = first.trim_start_matches("ERR").trim();
            return Err(if msg.is_empty() {
                "tracker error".to_string()
            } else {
                format!("tracker: {msg}")
            });
        }
    }
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 6 {
            continue;
        }
        let addr = f[1].trim();
        if addr.is_empty() {
            continue;
        }
        out.push(DirectoryServer {
            name: f[0].trim().to_string(),
            // The tracker prints a dialable host:port; this client speaks
            // WebSocket, so that is the scheme it gets.
            endpoint: format!("ws://{addr}"),
            description: String::new(),
            users_online: f[2].trim().parse::<u32>().ok(),
            listeners: Vec::new(),
            uptime_pct: percent(Some(f[4])),
            reachable: true,
        });
    }
    if out.is_empty() {
        return Err("The tracker returned no servers.".to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn srv(name: &str, desc: &str, users: u32, uptime: u8, reachable: bool) -> DirectoryServer {
        DirectoryServer {
            name: name.into(),
            endpoint: format!("wss://{}.example", name.to_ascii_lowercase()),
            description: desc.into(),
            users_online: Some(users),
            listeners: Vec::new(),
            uptime_pct: Some(uptime),
            reachable,
        }
    }

    /// A trimmed copy of a real `rabbithole.directory` reply (fetched
    /// 2026-08-12), so these tests fail if the shape we parse drifts from the
    /// shape it serves.
    const REAL_DIRECTORY: &str = r#"{"ok":true,"version":5,"now":1786524774226,"durable":true,
      "burrows":[
        {"name":"alice@wonderland","sysop":"Alice Liddell","status":"online","uptime":"99.8%",
         "sparkline":[99.8,100],"description":"The flagship central sanctuary Burrow.",
         "listeners":["quic","ws","telnet","hotline"],
         "identity":"7d6cf4a1","plan":"Project: routing.","quicUri":"quic://wonderland.co:4653",
         "wsUri":"ws://wonderland.co:4654","seed":true,"live":false},
        {"name":"chesire@woods","sysop":"Cheshire Cat","status":"offline","uptime":"91.5%",
         "sparkline":[100],"description":"Cryptic boards.","listeners":["quic","ws"],
         "identity":"9f8e7d6c","plan":"Ephemeral.","quicUri":"quic://c.example:4653",
         "wsUri":"ws://chesire-woods.org:4654","seed":true,"live":false},
        {"name":"quic-only@nowhere","status":"online","uptime":"50%","listeners":["quic"],
         "quicUri":"quic://nowhere.example:4653"}
      ],"glasses":[],"log":[]}"#;

    #[test]
    fn the_directory_reply_maps_to_rows_this_client_can_dial() {
        let rows = parse_directory_json(REAL_DIRECTORY).expect("parses");
        // The quic-only burrow is skipped: a WebSocket client can't connect to
        // it, and a row you can't use only wastes a click.
        assert_eq!(rows.len(), 2);
        let a = &rows[0];
        assert_eq!(a.name, "alice@wonderland");
        assert_eq!(a.endpoint, "ws://wonderland.co:4654");
        assert_eq!(a.uptime_pct, Some(100), "99.8% rounds to 100");
        assert!(a.reachable, "status online");
        assert_eq!(a.listeners, vec!["quic", "ws", "telnet", "hotline"]);
        // The directory publishes no population; saying "0 online" would be
        // the client inventing a fact.
        assert_eq!(a.users_online, None);
        assert!(!rows[1].reachable, "status offline");
    }

    #[test]
    fn a_quic_speaking_client_keeps_the_rows_a_browser_has_to_skip() {
        let rows =
            parse_directory_json_with(REAL_DIRECTORY, &["wsUri", "quicUri"]).expect("parses");
        assert_eq!(rows.len(), 3, "the quic-only burrow is dialable natively");
        assert_eq!(rows[2].endpoint, "quic://nowhere.example:4653");
        assert_eq!(
            rows[0].endpoint, "ws://wonderland.co:4654",
            "preference order holds: ws first where both exist"
        );
    }

    /// The Looking Glass's own `/api/burrows` shape (`api/burrows.mjs` in the
    /// glass.rabbit.direct project): nested `endpoints`, a relayed
    /// `descriptor`, liveness instead of uptime. Deliberately *not* the
    /// directory's shape — the two services answer differently.
    const REAL_GLASS: &str = r#"{"ok":true,"updatedAt":1786533368707,"total":2,"online":1,
      "burrows":[
        {"slug":"wonderland","url":"https://wonderland.glass.rabbit.direct",
         "name":"alice@wonderland","publicKey":"7d6c","sysop":"Alice Liddell",
         "listeners":["quic","ws","telnet"],
         "endpoints":{"quic":"quic://wonderland.co:4653","ws":"ws://wonderland.co:4654"},
         "status":"online","lastSeen":1786533300000,"firstSeen":1786000000000,
         "descriptor":{"name":"alice@wonderland","description":"The flagship.",
           "endpoints":{"quic":"quic://wonderland.co:4653"}},
         "signature":"ab","source":"tracker"},
        {"slug":"woods","url":"https://woods.glass.rabbit.direct","name":"chesire@woods",
         "publicKey":"9f8e","listeners":["quic"],
         "endpoints":{"quic":"quic://c.example:4653"},
         "status":"offline","descriptor":{"description":"Cryptic boards."},
         "source":"tracker"}
      ]}"#;

    #[test]
    fn the_glass_reply_is_a_different_shape_and_parses_as_one() {
        // A WebSocket client: only the burrow with a `ws` endpoint.
        let rows = parse_glass_json(REAL_GLASS, &["ws"]).expect("parses");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "alice@wonderland");
        assert_eq!(rows[0].endpoint, "ws://wonderland.co:4654");
        assert!(rows[0].reachable);
        assert_eq!(rows[0].listeners, vec!["quic", "ws", "telnet"]);
        // The blurb lives on the relayed descriptor, one level in — exactly
        // the nesting the old brace-splitting parser could not reach.
        assert_eq!(rows[0].description, "The flagship.");
        // A glass reports liveness, not a history. Claiming 0% or 100%
        // because it answered would be inventing one.
        assert_eq!(rows[0].uptime_pct, None);
        assert_eq!(rows[0].users_online, None);

        // A QUIC-speaking client gets both, ws preferred where both exist.
        let rows = parse_glass_json(REAL_GLASS, &["ws", "quic"]).expect("parses");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].endpoint, "ws://wonderland.co:4654");
        assert_eq!(rows[1].endpoint, "quic://c.example:4653");
        assert!(!rows[1].reachable, "status offline");
        assert_eq!(rows[1].description, "Cryptic boards.");
    }

    #[test]
    fn the_two_services_are_not_interchangeable() {
        // Reading a glass reply with the directory's parser finds no flat
        // `wsUri`, and vice versa. Each says so instead of silently
        // returning an empty listing that looks like "nobody is out there".
        assert!(parse_directory_json(REAL_GLASS).is_err());
        assert!(parse_glass_json(REAL_DIRECTORY, &["ws", "quic"]).is_err());
    }

    #[test]
    fn an_empty_but_valid_glass_listing_is_distinguished_from_a_broken_one() {
        // The live tracker really does serve this when nobody has announced.
        let empty = r#"{"ok":true,"updatedAt":1786533368707,"total":0,"online":0,"burrows":[]}"#;
        let err = parse_glass_json(empty, &["ws"]).unwrap_err();
        assert!(err.contains("no burrows this client can dial"), "{err}");
        // Whereas a reply with no burrows *field* is a different problem.
        let broken = parse_glass_json(r#"{"ok":false,"error":"store_not_provisioned"}"#, &["ws"])
            .unwrap_err();
        assert!(broken.contains("no burrows list"), "{broken}");
    }

    #[test]
    fn a_burrow_can_ask_not_to_be_listed_and_the_ask_travels_with_its_signature() {
        // The document a burrow serves at /.well-known/rabbithole/server with
        // `announce_enabled = false` — field names and feature list copied
        // from a live burrow's reply, so this fails if the shape drifts.
        let opted_out = r#"{"body":{"server_key":[1,2],"name":"Wonderland","origin":"wonderland",
            "addresses":["quic://wonderland.example:4893"],
            "features":["boards","chat","dm","files","swarm","noindex","guest"],
            "issued_at":1786536378515},"sig":[9,9]}"#;
        assert!(noindex_in_descriptor(opted_out));

        let listed = opted_out.replace(r#""noindex","#, "");
        assert!(!noindex_in_descriptor(&listed));
    }

    #[test]
    fn an_unreadable_descriptor_is_not_treated_as_consent_to_list() {
        // These return false because there is nothing to honor, not because
        // the burrow agreed. A caller that can wait should retry rather than
        // publish on the strength of a failed fetch — which is why the doc
        // comment says so and this test pins the distinction.
        for nothing in ["", "not json", "{}", r#"{"body":{}}"#] {
            assert!(!noindex_in_descriptor(nothing), "{nothing:?}");
        }
        // A flattened shape still gets honored: the tag is what matters, not
        // where a future descriptor version happens to put it.
        assert!(noindex_in_descriptor(r#"{"features":["chat","noindex"]}"#));
        // And a burrow whose *name* is "noindex" has not opted out.
        assert!(!noindex_in_descriptor(
            r#"{"body":{"name":"noindex","features":["chat"]}}"#
        ));
    }

    #[test]
    fn a_directory_reply_we_cannot_use_says_so() {
        assert!(parse_directory_json("{}").is_err(), "no burrows list");
        assert!(parse_directory_json("").is_err());
        // Well-formed but nothing dialable.
        let quic_only = r#"{"burrows":[{"name":"x","quicUri":"quic://x:1"}]}"#;
        assert!(parse_directory_json(quic_only).is_err());
        // A missing uptime is "not reported", not a confident 0%.
        let no_up = r#"{"burrows":[{"name":"x","wsUri":"ws://x:1","status":"online"}]}"#;
        assert_eq!(parse_directory_json(no_up).unwrap()[0].uptime_pct, None);
    }

    #[test]
    fn tracker_index_rows_parse_and_short_rows_are_skipped() {
        // The documented column layout (apps/tracker): name, addr, users,
        // categories, uptime, last_seen, signed, key, gen.
        let reply = "The Warren\t10.0.0.1:4654\t42\tchat,files\t99.5\t12\tyes\tdeadbeef\t1786\nNight Pool\t10.0.0.2:4654\t7\t\t88.0\t30\tno\t-\t-\ntruncated\trow\n";
        let rows = parse_tracker_index(reply).expect("parses");
        assert_eq!(rows.len(), 2, "the short row is skipped, not fatal");
        assert_eq!(rows[0].name, "The Warren");
        assert_eq!(rows[0].endpoint, "ws://10.0.0.1:4654", "dialable as ws");
        assert_eq!(
            rows[0].users_online,
            Some(42),
            "the tracker DOES count users"
        );
        assert_eq!(rows[0].uptime_pct, Some(100));
        assert_eq!(rows[1].uptime_pct, Some(88));
    }

    #[test]
    fn a_tracker_error_line_is_reported_not_parsed_as_a_server() {
        let err = parse_tracker_index("ERR unknown command\n").unwrap_err();
        assert!(err.contains("unknown command"), "{err}");
        // A server that named itself "ERR …" is tab-framed and stays a server.
        let rows = parse_tracker_index("ERR Lounge\t1.2.3.4:1\t3\t\t100\t1\tno\t-\t-\n")
            .expect("tab-framed rows are data");
        assert_eq!(rows[0].name, "ERR Lounge");
        assert!(parse_tracker_index("").is_err(), "empty is not a listing");
    }

    #[test]
    fn the_two_sources_are_labelled_for_the_user() {
        // A narrower source answering is a different answer, not the same one.
        assert!(DirectorySource::Directory
            .label()
            .contains("rabbithole.directory"));
        assert!(DirectorySource::Tracker
            .label()
            .contains("tracker.rabbit.direct"));
        assert!(DirectorySource::Seeded.label().contains("sample"));
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
}
