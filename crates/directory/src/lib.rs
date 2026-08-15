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
//!   coordinator. Beside a federating burrow (`just up`) the status port
//!   moves to **5497** so it does not collide with `federation_addr`; a
//!   loopback host without a port uses that local port, every other bare
//!   host still gets 4655.
//!
//! So: directory first, the standard glass behind it, and the UI says which
//! answered — "who told you this" is part of the answer to "who is out there".
//! A live source that answers `[]` is not the last word: the next live
//! source is asked, because an empty directory must not hide a glass that
//! has listings. Sample rows appear only when nothing answered.
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectorySource {
    /// `rabbithole.directory` over HTTPS.
    Directory,
    /// A Looking Glass. The string is the coordinator that answered (host,
    /// and a non-default port when the user named one), not a hardcoded
    /// flagship hostname — a self-hosted glass is a different answer.
    Tracker(String),
    /// The built-in sample list — nothing reachable answered.
    Seeded,
}

impl DirectorySource {
    /// The standard Looking Glass, `tracker.rabbit.direct`.
    pub fn standard_glass() -> Self {
        DirectorySource::Tracker(TRACKER_HOST.to_string())
    }

    /// A Looking Glass the user named. Scheme, path, and the default status
    /// port are stripped so the label is the coordinator, not the URL we dialed.
    pub fn looking_glass(entry: &str) -> Self {
        DirectorySource::Tracker(tracker_label(entry))
    }

    pub fn label(&self) -> &str {
        match self {
            DirectorySource::Directory => "rabbithole.directory",
            DirectorySource::Tracker(host) => host,
            DirectorySource::Seeded => "built-in sample \u{2014} no directory reachable",
        }
    }
}

/// Human label for a Looking Glass entry: scheme and path stripped, the
/// default status port omitted so `https://glass.example/api/burrows` and
/// `glass.example:4655` both read as `glass.example`.
pub fn tracker_label(entry: &str) -> String {
    let e = entry.trim();
    let e = e
        .strip_prefix("https://")
        .or_else(|| e.strip_prefix("http://"))
        .unwrap_or(e);
    let e = e.split('/').next().unwrap_or(e);
    let suffix = format!(":{TRACKER_STATUS_PORT}");
    match e.strip_suffix(suffix.as_str()) {
        Some(host) if !host.is_empty() => host.to_string(),
        _ => e.to_string(),
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
/// a browser tab has no TCP. This is the **public** default; a loopback
/// host without a port uses [`LOCAL_STATUS_PORT`] instead, matching `just up`.
pub const TRACKER_STATUS_PORT: u16 = 4655;

/// Status port `just up` / `just tracker` bind so INDEX does not sit on the
/// burrow's `federation_addr` (4655). Public looking-glass stays on 4655.
pub const LOCAL_STATUS_PORT: u16 = 5497;

/// A live discovery answer plus which source produced it.
///
/// [`pick_live_listing`] is the policy: a successful empty listing is not
/// exclusive — a later source with rows wins — and the sample is not this
/// type's job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveListing {
    pub servers: Vec<DirectoryServer>,
    pub source: DirectorySource,
    /// Why a wider source was not used, when a narrower one answered.
    pub fallback_reason: Option<String>,
}

/// Combine sequential live discovery attempts.
///
/// A source that answered with rows wins, even if an earlier source answered
/// empty. An empty answer is still live: if every source is empty (or later
/// ones failed), the first empty listing is kept. Only when nothing answered
/// at all is this `None` — the caller may then show a sample.
pub fn pick_live_listing<I>(answers: I) -> Option<LiveListing>
where
    I: IntoIterator<Item = Result<(Vec<DirectoryServer>, DirectorySource), String>>,
{
    let mut first_empty: Option<DirectorySource> = None;
    let mut first_error: Option<String> = None;
    for answer in answers {
        match answer {
            Ok((servers, source)) if !servers.is_empty() => {
                let fallback_reason = if let Some(empty) = &first_empty {
                    Some(format!("{} listed nobody", empty.label()))
                } else {
                    first_error.clone()
                };
                return Some(LiveListing {
                    servers,
                    source,
                    fallback_reason,
                });
            }
            Ok((_, source)) => {
                if first_empty.is_none() {
                    first_empty = Some(source);
                }
            }
            Err(e) => {
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
        }
    }
    first_empty.map(|source| LiveListing {
        servers: Vec::new(),
        source,
        fallback_reason: first_error,
    })
}

/// Split an HTTP authority into `(host, port)`.
///
/// The host is unbracketed (`::1`, `example.com`) so TLS and `TcpStream`
/// can use it directly. IPv6 literals may arrive as `[::1]` or `[::1]:8443`;
/// splitting on the last colon would eat a hextet.
pub fn split_authority(authority: &str, default_port: u16) -> Result<(String, u16), String> {
    let a = authority.trim();
    if a.is_empty() {
        return Err("empty host".to_string());
    }
    if let Some(rest) = a.strip_prefix('[') {
        let close = rest
            .find(']')
            .ok_or_else(|| format!("{a} is a broken IPv6 literal"))?;
        let host = &rest[..close];
        if host.is_empty() {
            return Err("empty IPv6 host".to_string());
        }
        let after = &rest[close + 1..];
        if after.is_empty() {
            return Ok((host.to_string(), default_port));
        }
        let port = after
            .strip_prefix(':')
            .ok_or_else(|| format!("{a} has junk after the IPv6 literal"))?;
        let port: u16 = port.parse().map_err(|_| format!("bad port in {a}"))?;
        return Ok((host.to_string(), port));
    }
    if a.matches(':').count() == 1 {
        if let Some((h, p)) = a.rsplit_once(':') {
            if !h.is_empty() && !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) {
                let port: u16 = p.parse().map_err(|_| format!("bad port in {a}"))?;
                return Ok((h.to_string(), port));
            }
        }
    }
    Ok((a.to_string(), default_port))
}

/// `Host` header value: bracket IPv6, include a non-default port.
pub fn host_header(host: &str, port: u16, default_port: u16) -> String {
    let name = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    if port == default_port {
        name
    } else {
        format!("{name}:{port}")
    }
}

/// Loopback names and addresses: `localhost`, `127.0.0.0/8`, `::1`.
pub fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().trim_matches(|c| c == '[' || c == ']');
    if h.eq_ignore_ascii_case("localhost") {
        return true;
    }
    h.parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

/// Status port for a bare host: 5497 on loopback (`just up`), 4655 elsewhere.
pub fn default_status_port(host: &str) -> u16 {
    if is_loopback_host(host) {
        LOCAL_STATUS_PORT
    } else {
        TRACKER_STATUS_PORT
    }
}

/// Normalize a tracker entry to a dialable `host:port` (bracketed IPv6).
///
/// A URL's scheme colon is not a port. A bare public host gets 4655; a bare
/// loopback host gets 5497 so `just up` and a typed `localhost` agree.
pub fn status_addr(entry: &str) -> String {
    let e = host_port_of(entry);
    if has_explicit_status_port(e) {
        return e.to_string();
    }
    let host = e.trim_matches(|c| c == '[' || c == ']');
    let port = default_status_port(host);
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Strip a scheme and path so `https://glass.example:8443/api/burrows` becomes
/// `glass.example:8443`.
fn host_port_of(entry: &str) -> &str {
    let e = entry.trim();
    let e = e
        .strip_prefix("https://")
        .or_else(|| e.strip_prefix("http://"))
        .unwrap_or(e);
    e.split('/').next().unwrap_or(e)
}

fn has_explicit_status_port(e: &str) -> bool {
    if e.starts_with('[') {
        e.rfind("]:").is_some()
    } else {
        e.matches(':').count() == 1
    }
}

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
    let rows = listed_burrows(&doc, "directory")?;
    if rows.is_empty() {
        // The directory answered and said nobody is listed. That is a
        // listing, not a fetch failure — substituting a sample would claim
        // otherwise.
        return Ok(Vec::new());
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
    let rows = listed_burrows(&doc, "tracker")?;
    if rows.is_empty() {
        // A glass with nobody announced serves `burrows: []`. Keep that
        // answer; falling through to a seeded sample would look like
        // "somebody is out there".
        return Ok(Vec::new());
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
        // No parseable rows. An INDEX with no lines is a live empty listing
        // (the glass serves that when nobody has announced). Lines we could
        // not read are a broken reply, not "nobody".
        if text.lines().all(|l| l.trim().is_empty()) {
            return Ok(Vec::new());
        }
        return Err("The tracker returned no servers.".to_string());
    }
    Ok(out)
}

/// The `burrows` array from a directory or glass JSON document.
///
/// Missing or non-array is a broken reply. Present-and-empty is a successful
/// listing of nobody — the parser must not invent rows. Discovery
/// ([`pick_live_listing`]) may still consult a later source.
fn listed_burrows<'a>(doc: &'a json::Json, kind: &str) -> Result<&'a [json::Json], String> {
    match doc.get("burrows") {
        Some(json::Json::Arr(items)) => Ok(items),
        _ => Err(format!("That {kind} reply has no burrows list.")),
    }
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
        // That is "nobody is out there", not "we could not ask".
        let empty = r#"{"ok":true,"updatedAt":1786533368707,"total":0,"online":0,"burrows":[]}"#;
        let rows = parse_glass_json(empty, &["ws"]).expect("empty is a listing");
        assert!(rows.is_empty(), "keep the empty answer");
        assert!(parse_directory_json(r#"{"ok":true,"burrows":[]}"#)
            .expect("empty directory is a listing")
            .is_empty());
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
        assert!(
            parse_tracker_index("").unwrap().is_empty(),
            "an empty INDEX is a listing of nobody, not a fetch failure"
        );
    }

    #[test]
    fn the_two_sources_are_labelled_for_the_user() {
        // A narrower source answering is a different answer, not the same one.
        assert!(DirectorySource::Directory
            .label()
            .contains("rabbithole.directory"));
        assert_eq!(
            DirectorySource::standard_glass().label(),
            "tracker.rabbit.direct"
        );
        assert_eq!(
            DirectorySource::looking_glass("https://glass.example:8443/api/burrows").label(),
            "glass.example:8443",
            "a named coordinator is labelled as itself, not the flagship host"
        );
        assert_eq!(
            DirectorySource::looking_glass("tracker.rabbit.direct:4655").label(),
            "tracker.rabbit.direct",
            "the default status port is not part of the name"
        );
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

    fn row(name: &str) -> DirectoryServer {
        srv(name, "", 0, 100, true)
    }

    #[test]
    fn an_empty_live_source_does_not_hide_a_later_one_with_rows() {
        // rabbithole.directory answering `[]` used to be treated as the last
        // word, so a glass with listings never got asked.
        let empty_dir = Ok((Vec::new(), DirectorySource::Directory));
        let glass = Ok((vec![row("Warren")], DirectorySource::standard_glass()));
        let picked = pick_live_listing([empty_dir, glass]).expect("glass has rows");
        assert_eq!(picked.servers[0].name, "Warren");
        assert_eq!(picked.source.label(), "tracker.rabbit.direct");
        assert!(
            picked
                .fallback_reason
                .as_deref()
                .unwrap_or("")
                .contains("rabbithole.directory"),
            "{:?}",
            picked.fallback_reason
        );
    }

    #[test]
    fn every_live_source_empty_stays_empty_not_a_sample() {
        let picked = pick_live_listing([
            Ok((Vec::new(), DirectorySource::Directory)),
            Ok((Vec::new(), DirectorySource::standard_glass())),
        ])
        .expect("empty is still a live answer");
        assert!(picked.servers.is_empty());
        assert_eq!(picked.source, DirectorySource::Directory);
        assert!(picked.fallback_reason.is_none());
    }

    #[test]
    fn nothing_answered_is_none_so_the_caller_may_show_a_sample() {
        assert!(
            pick_live_listing([Err("directory down".into()), Err("glass down".into()),]).is_none()
        );
    }

    #[test]
    fn a_failed_directory_and_an_empty_glass_keep_the_empty_glass() {
        let picked = pick_live_listing([
            Err("directory down".into()),
            Ok((Vec::new(), DirectorySource::standard_glass())),
        ])
        .expect("empty glass is live");
        assert!(picked.servers.is_empty());
        assert_eq!(picked.source.label(), "tracker.rabbit.direct");
        assert_eq!(picked.fallback_reason.as_deref(), Some("directory down"));
    }

    #[test]
    fn ipv6_authorities_keep_their_hextets() {
        assert_eq!(split_authority("[::1]", 443).unwrap(), ("::1".into(), 443));
        assert_eq!(
            split_authority("[::1]:8443", 443).unwrap(),
            ("::1".into(), 8443)
        );
        assert_eq!(
            split_authority("[2001:db8::1]", 443).unwrap(),
            ("2001:db8::1".into(), 443)
        );
        assert_eq!(
            split_authority("glass.example:8443", 443).unwrap(),
            ("glass.example".into(), 8443)
        );
        assert!(split_authority("[::1", 443).is_err());
        assert_eq!(host_header("::1", 443, 443), "[::1]");
        assert_eq!(host_header("::1", 8443, 443), "[::1]:8443");
        assert_eq!(host_header("glass.example", 443, 443), "glass.example");
    }

    #[test]
    fn loopback_bare_hosts_use_the_local_stack_port() {
        assert_eq!(status_addr("localhost"), "localhost:5497");
        assert_eq!(status_addr("127.0.0.1"), "127.0.0.1:5497");
        assert_eq!(status_addr("::1"), "[::1]:5497");
        assert_eq!(status_addr("[::1]"), "[::1]:5497");
        assert_eq!(
            status_addr("tracker.rabbit.direct"),
            "tracker.rabbit.direct:4655"
        );
        assert_eq!(status_addr("[::1]:4655"), "[::1]:4655");
        assert_eq!(
            status_addr("https://tracker.rabbit.direct/api/burrows"),
            "tracker.rabbit.direct:4655"
        );
    }
}
