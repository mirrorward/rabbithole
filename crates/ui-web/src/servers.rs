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
    /// Members currently online, when the source reports it. `None` where it
    /// doesn't — rabbithole.directory publishes uptime and listeners but no
    /// population, and rendering a confident "0 online" for "not reported"
    /// would be the directory lying on the source's behalf.
    pub users_online: Option<u32>,
    /// The protocols this burrow listens on (`quic`, `ws`, `telnet`…), when
    /// the source says. Empty when unknown.
    pub listeners: Vec<String>,
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
            .then(b.users_online.unwrap_or(0).cmp(&a.users_online.unwrap_or(0)))
            .then(a.name.cmp(&b.name))
    });
    out
}

/// Where a directory listing came from — shown to the user, because "who told
/// you this" is part of the answer when the answer is "who is out there".
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

/// The fallback tracker's host and status port. Its protocol is a
/// **line-oriented TCP** exchange (`INDEX` in, tab-separated rows out) — no
/// HTTP, so only the native shell can dial it; a browser tab has no TCP.
pub const TRACKER_HOST: &str = "tracker.rabbit.direct";
pub const TRACKER_STATUS_PORT: u16 = 4655;

/// Parse `rabbithole.directory`'s JSON into directory rows.
///
/// Hand-rolled rather than pulling serde_json's derive into the wasm bundle
/// for one endpoint: the shape is flat and every field is optional in
/// practice, since a directory that adds a field must not break older clients.
/// A burrow with no `wsUri` is skipped — this client dials WebSocket, and a
/// row you cannot connect to is a row that only wastes a click.
pub fn parse_directory_json(text: &str) -> Result<Vec<DirectoryServer>, String> {
    let burrows = text
        .split_once("\"burrows\"")
        .map(|(_, rest)| rest)
        .ok_or_else(|| "That directory reply has no burrows list.".to_string())?;
    let mut out = Vec::new();
    // Objects are flat (no nested braces except arrays), so splitting on `{`
    // is enough and cannot mis-nest.
    for chunk in burrows.split('{').skip(1) {
        let chunk = chunk.split('}').next().unwrap_or_default();
        let Some(endpoint) = json_str(chunk, "wsUri").filter(|u| !u.is_empty()) else {
            continue;
        };
        let name = json_str(chunk, "name").unwrap_or_else(|| endpoint.clone());
        out.push(DirectoryServer {
            name,
            endpoint,
            description: json_str(chunk, "description").unwrap_or_default(),
            // The directory publishes uptime and listeners, not population.
            users_online: None,
            listeners: json_str_array(chunk, "listeners"),
            uptime_pct: json_str(chunk, "uptime")
                .and_then(|u| u.trim_end_matches('%').parse::<f32>().ok())
                .map(|p| p.round().clamp(0.0, 100.0) as u8)
                .unwrap_or(0),
            reachable: json_str(chunk, "status").as_deref() == Some("online"),
        });
    }
    if out.is_empty() {
        return Err("The directory listed no burrows this client can dial.".to_string());
    }
    Ok(out)
}

/// Parse a tracker `INDEX` reply: one tab-separated row per live server,
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
            uptime_pct: f[4]
                .trim()
                .parse::<f32>()
                .ok()
                .map(|p| p.round().clamp(0.0, 100.0) as u8)
                .unwrap_or(0),
            reachable: true,
        });
    }
    if out.is_empty() {
        return Err("The tracker returned no servers.".to_string());
    }
    Ok(out)
}

/// A `"name": "value"` string field from a flat JSON object body.
fn json_str(chunk: &str, name: &str) -> Option<String> {
    let key = format!("\"{name}\"");
    let after = &chunk[chunk.find(&key)? + key.len()..];
    let after = after.trim_start().strip_prefix(':')?.trim_start();
    let rest = after.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// A `"name": ["a", "b"]` array of strings.
fn json_str_array(chunk: &str, name: &str) -> Vec<String> {
    let key = format!("\"{name}\"");
    let Some(i) = chunk.find(&key) else {
        return Vec::new();
    };
    let after = &chunk[i + key.len()..];
    let Some(open) = after.find('[') else {
        return Vec::new();
    };
    let Some(close) = after[open..].find(']') else {
        return Vec::new();
    };
    after[open + 1..open + close]
        .split(',')
        .filter_map(|p| {
            let p = p.trim().trim_matches('"').trim();
            (!p.is_empty()).then(|| p.to_string())
        })
        .collect()
}

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
            uptime_pct: uptime,
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
                uptime_pct: 100,
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

    fn srv(name: &str, desc: &str, users: u32, uptime: u8, reachable: bool) -> DirectoryServer {
        DirectoryServer {
            name: name.into(),
            endpoint: format!("wss://{}.example", name.to_ascii_lowercase()),
            description: desc.into(),
            users_online: Some(users),
            listeners: Vec::new(),
            uptime_pct: uptime,
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
        // The quic-only burrow is skipped: this client dials WebSocket, and a
        // row you can't connect to only wastes a click.
        assert_eq!(rows.len(), 2);
        let a = &rows[0];
        assert_eq!(a.name, "alice@wonderland");
        assert_eq!(a.endpoint, "ws://wonderland.co:4654");
        assert_eq!(a.uptime_pct, 100, "99.8% rounds to 100");
        assert!(a.reachable, "status online");
        assert_eq!(a.listeners, vec!["quic", "ws", "telnet", "hotline"]);
        // The directory publishes no population; saying "0 online" would be
        // the client inventing a fact.
        assert_eq!(a.users_online, None);
        assert!(!rows[1].reachable, "status offline");
    }

    #[test]
    fn a_directory_reply_we_cannot_use_says_so() {
        assert!(parse_directory_json("{}").is_err(), "no burrows list");
        assert!(parse_directory_json("").is_err());
        // Well-formed but nothing dialable.
        let quic_only = r#"{"burrows":[{"name":"x","quicUri":"quic://x:1"}]}"#;
        assert!(parse_directory_json(quic_only).is_err());
    }

    #[test]
    fn tracker_index_rows_parse_and_short_rows_are_skipped() {
        // The documented column layout (apps/tracker): name, addr, users,
        // categories, uptime, last_seen, signed, key, gen.
        let reply = "The Warren\t10.0.0.1:4654\t42\tchat,files\t99.5\t12\tyes\tdeadbeef\t1786\n                     Night Pool\t10.0.0.2:4654\t7\t\t88.0\t30\tno\t-\t-\n                     truncated\trow\n";
        let rows = parse_tracker_index(reply).expect("parses");
        assert_eq!(rows.len(), 2, "the short row is skipped, not fatal");
        assert_eq!(rows[0].name, "The Warren");
        assert_eq!(rows[0].endpoint, "ws://10.0.0.1:4654", "dialable as ws");
        assert_eq!(rows[0].users_online, Some(42), "the tracker DOES count users");
        assert_eq!(rows[0].uptime_pct, 100);
        assert_eq!(rows[1].uptime_pct, 88);
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
        // "Who told you this" is part of the answer to "who is out there".
        assert!(DirectorySource::Directory.label().contains("rabbithole.directory"));
        assert!(DirectorySource::Tracker.label().contains("tracker.rabbit.direct"));
        assert!(DirectorySource::Seeded.label().contains("sample"));
    }

    #[test]
    fn endpoints_survive_the_query_string() {
        // The Looking Glass hands its pick to the connect screen through the
        // URL, so an endpoint's separators must not end the parameter.
        assert_eq!(
            encode_param("wss://warren.example:9000"),
            "wss%3A%2F%2Fwarren.example%3A9000"
        );
        assert_eq!(encode_param("demo://night-pool"), "demo%3A%2F%2Fnight-pool");
        assert_eq!(encode_param("a-b_c.d~e"), "a-b_c.d~e", "unreserved stays readable");
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
