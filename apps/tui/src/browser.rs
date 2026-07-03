//! Server browser over a Looking Glass tracker's **status port**.
//!
//! The tracker (`apps/tracker`, lib `looking-glass`) serves a one-shot text
//! protocol on its native status port (classic 4655): one command line in,
//! a tab-separated reply out, connection closed. The verbs this view uses:
//!
//! - `INDEX` / `INDEX cat=<name>` — one line per live server, sorted by the
//!   tracker (signed-first, then observed uptime, then name — rendered **as
//!   served**, never re-sorted here):
//!   `name<TAB>ip:port<TAB>users<TAB>categories<TAB>uptime_24h<TAB>last_seen_secs<TAB>signed<TAB>key<TAB>gen`
//! - `CATEGORIES` — `name<TAB>live-count` per category (the filter row).
//! - `HEALTH <ip:port>` — a detail line
//!   (`ip:port<TAB>live=…<TAB>uptime_24h=…<TAB>first_seen_secs=…<TAB>last_seen_secs=…<TAB>flaps=…`)
//!   plus a `#`/`+`/`.` sparkline line (15-minute buckets, oldest first).
//! - Errors are one-line `ERR …` replies.
//!
//! ## Verifiable, not authoritative
//!
//! Everything numeric here is the **tracker's own local observation** —
//! uptime/flaps are what that one tracker saw, never a property the server
//! proved. The UI therefore labels uptime as *tracker-observed*. What a
//! client *can* verify is the signed descriptor behind a row: `INDEX`
//! carries the Ed25519 key prefix + generation so the full descriptor can be
//! fetched (tracker gossip `Want`) and checked offline. This view surfaces
//! the badge + key material; it does not itself fetch descriptors yet.
//!
//! ## Totality
//!
//! The parsers are **total and defensive**: malformed lines are skipped (an
//! all-garbage reply becomes an error, never a panic), `ERR` replies become
//! error strings, and the network edge ([`query`]) bounds connect time, read
//! time, and reply size. Network/parse failures surface in-pane with a retry
//! hint (`r`).

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Environment variable holding the default tracker address.
pub const TRACKER_ENV: &str = "RABBIT_TRACKER";

/// The tracker's classic native status port, appended when the user types a
/// bare host with no `:port`.
pub const STATUS_PORT: u16 = 4655;

/// How long a connect may take before it is reported as timed out.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// How long the write+read of one command/reply exchange may take in total.
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Reply size cap — the status port serves short directories; anything
/// bigger is garbage and gets truncated rather than buffered forever.
const MAX_RESPONSE: u64 = 512 * 1024;

// ---------------------------------------------------------------------------
// Wire shapes (parsed rows).
// ---------------------------------------------------------------------------

/// One parsed `INDEX` row (see the module docs for the column layout).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub name: String,
    /// `ip:port` as printed by the tracker (kept as text — it is display
    /// data and the `HEALTH` argument, not something we dial).
    pub addr: String,
    pub users: u64,
    pub categories: Vec<String>,
    /// Observed 24 h uptime as the tracker rendered it (e.g. `"100.0"`,
    /// a percent with one decimal). Validated numeric, kept verbatim.
    pub uptime_pct: String,
    pub last_seen_secs: u64,
    pub signed: bool,
    /// First 8 bytes of the verified server key (hex) for signed rows.
    pub key_prefix: Option<String>,
    /// Signed descriptor generation/attestation timestamp (unix ms, as text).
    pub generation: Option<String>,
}

/// One `CATEGORIES` row: category name plus its live-server count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryCount {
    pub name: String,
    pub count: u64,
}

/// Parsed `HEALTH <ip:port>` detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthDetail {
    pub addr: String,
    pub live: bool,
    /// Observed uptime percent, verbatim (see [`IndexEntry::uptime_pct`]).
    pub uptime_pct: String,
    pub first_seen_secs: u64,
    pub last_seen_secs: u64,
    pub flaps: u64,
    /// `#`/`+`/`.` per 15-minute bucket, oldest first (rendered verbatim).
    pub sparkline: String,
}

// ---------------------------------------------------------------------------
// Total parsers.
// ---------------------------------------------------------------------------

/// A tracker `ERR …` reply, if that is what `text` is. Real `ERR` lines
/// never contain tabs, so a server that *named itself* "ERR …" (tab-framed
/// columns) is not mistaken for one.
fn tracker_err(text: &str) -> Option<String> {
    let first = text.lines().next()?.trim();
    if first.contains('\t') || !first.starts_with("ERR") {
        return None;
    }
    let msg = first.trim_start_matches("ERR").trim();
    Some(if msg.is_empty() {
        "tracker error".to_string()
    } else {
        format!("tracker: {msg}")
    })
}

/// Parse one `INDEX` line. `None` for anything that does not match the
/// nine-column shape — the caller skips such lines rather than panicking.
pub fn parse_index_line(line: &str) -> Option<IndexEntry> {
    let cols: Vec<&str> = line.split('\t').collect();
    if cols.len() != 9 {
        return None;
    }
    let name = cols[0].trim();
    let addr = cols[1].trim();
    if name.is_empty() || addr.is_empty() {
        return None;
    }
    let users: u64 = cols[2].trim().parse().ok()?;
    let categories = parse_categories_field(cols[3]);
    let uptime_pct = cols[4].trim();
    // Validate numeric (the tracker prints a percent like "97.5") but keep
    // the tracker's own rendering verbatim.
    uptime_pct.parse::<f64>().ok()?;
    let last_seen_secs: u64 = cols[5].trim().parse().ok()?;
    let signed = match cols[6].trim() {
        "yes" => true,
        "no" => false,
        _ => return None,
    };
    let key = cols[7].trim();
    let generation = cols[8].trim();
    Some(IndexEntry {
        name: name.to_string(),
        addr: addr.to_string(),
        users,
        categories,
        uptime_pct: uptime_pct.to_string(),
        last_seen_secs,
        signed,
        key_prefix: (key != "-" && !key.is_empty()).then(|| key.to_string()),
        generation: (generation != "-" && !generation.is_empty()).then(|| generation.to_string()),
    })
}

/// The comma-joined category column (`-` means none).
fn parse_categories_field(field: &str) -> Vec<String> {
    let field = field.trim();
    if field.is_empty() || field == "-" {
        return Vec::new();
    }
    field
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Parse a full `INDEX` reply. Empty reply = empty directory. Malformed
/// lines are skipped; if *nothing* parsed but the reply was non-empty the
/// whole thing is reported as unrecognized (so garbage is visible, not
/// silently an empty list).
pub fn parse_index(text: &str) -> Result<Vec<IndexEntry>, String> {
    if let Some(err) = tracker_err(text) {
        return Err(err);
    }
    let mut rows = Vec::new();
    let mut bad = 0usize;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match parse_index_line(line) {
            Some(row) => rows.push(row),
            None => bad += 1,
        }
    }
    if rows.is_empty() && bad > 0 {
        return Err(format!("unrecognized INDEX reply ({bad} malformed lines)"));
    }
    Ok(rows)
}

/// Parse a `CATEGORIES` reply (`name<TAB>count` per line). Same skipping
/// rules as [`parse_index`].
pub fn parse_categories(text: &str) -> Result<Vec<CategoryCount>, String> {
    if let Some(err) = tracker_err(text) {
        return Err(err);
    }
    let mut out = Vec::new();
    let mut bad = 0usize;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut cols = line.split('\t');
        let parsed = match (cols.next(), cols.next(), cols.next()) {
            (Some(name), Some(count), None) => {
                let name = name.trim();
                count
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .filter(|_| !name.is_empty())
                    .map(|count| CategoryCount {
                        name: name.to_string(),
                        count,
                    })
            }
            _ => None,
        };
        match parsed {
            Some(row) => out.push(row),
            None => bad += 1,
        }
    }
    if out.is_empty() && bad > 0 {
        return Err(format!(
            "unrecognized CATEGORIES reply ({bad} malformed lines)"
        ));
    }
    Ok(out)
}

/// Parse a `HEALTH <ip:port>` reply: the six-field detail line plus the
/// sparkline line. `ERR …` and truncated/malformed replies become `Err`.
pub fn parse_health(text: &str) -> Result<HealthDetail, String> {
    if let Some(err) = tracker_err(text) {
        return Err(err);
    }
    let mut lines = text.lines();
    let head = lines.next().ok_or("empty HEALTH reply")?;
    let cols: Vec<&str> = head.split('\t').collect();
    if cols.len() != 6 {
        return Err(format!(
            "malformed HEALTH header ({} fields, expected 6)",
            cols.len()
        ));
    }
    let addr = cols[0].trim();
    if addr.is_empty() {
        return Err("malformed HEALTH header (empty address)".into());
    }
    let live = match cols[1].strip_prefix("live=").map(str::trim) {
        Some("yes") => true,
        Some("no") => false,
        _ => return Err("malformed HEALTH header (live=)".into()),
    };
    let uptime_pct = cols[2]
        .strip_prefix("uptime_24h=")
        .map(str::trim)
        .filter(|v| v.parse::<f64>().is_ok())
        .ok_or("malformed HEALTH header (uptime_24h=)")?;
    let first_seen_secs = parse_prefixed_u64(cols[3], "first_seen_secs=")
        .ok_or("malformed HEALTH header (first_seen_secs=)")?;
    let last_seen_secs = parse_prefixed_u64(cols[4], "last_seen_secs=")
        .ok_or("malformed HEALTH header (last_seen_secs=)")?;
    let flaps = parse_prefixed_u64(cols[5], "flaps=").ok_or("malformed HEALTH header (flaps=)")?;
    let sparkline = lines.next().map(str::trim).unwrap_or_default();
    if sparkline.is_empty() {
        return Err("truncated HEALTH reply (missing sparkline)".into());
    }
    Ok(HealthDetail {
        addr: addr.to_string(),
        live,
        uptime_pct: uptime_pct.to_string(),
        first_seen_secs,
        last_seen_secs,
        flaps,
        sparkline: sparkline.to_string(),
    })
}

fn parse_prefixed_u64(field: &str, prefix: &str) -> Option<u64> {
    field.strip_prefix(prefix)?.trim().parse().ok()
}

/// Normalize a typed tracker address: trim, and append the classic status
/// port when no `:` is present (bare hostname/IPv4). Anything already
/// containing a `:` — including bracketed IPv6 — is passed through and left
/// to the connector to accept or reject. `None` for blank input.
pub fn normalize_tracker_addr(input: &str) -> Option<String> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    if s.contains(':') {
        Some(s.to_string())
    } else {
        Some(format!("{s}:{STATUS_PORT}"))
    }
}

// ---------------------------------------------------------------------------
// View state (pure — unit-tested without sockets).
// ---------------------------------------------------------------------------

/// Payload of a completed `INDEX` (+ best-effort `CATEGORIES`) fetch:
/// `None` categories means that follow-up fetch failed and the previous
/// list should be kept.
pub type IndexPayload = (Vec<IndexEntry>, Option<Vec<CategoryCount>>);

/// What a background fetch task produced.
#[derive(Debug)]
pub enum Outcome {
    Index(Result<IndexPayload, String>),
    Health(Result<HealthDetail, String>),
}

/// A completed fetch, tagged with the request sequence number so stale
/// replies (superseded by a newer refresh) are dropped.
#[derive(Debug)]
pub struct Fetched {
    pub seq: u64,
    pub outcome: Outcome,
}

/// The server-browser view state. All transitions are synchronous and pure;
/// the async edges live in [`query`] plus the spawn glue in `main`.
#[derive(Debug, Default)]
pub struct BrowserState {
    /// The address line being edited (prefilled from `$RABBIT_TRACKER`).
    pub addr_input: String,
    /// Whether the address line currently captures keystrokes.
    pub editing_addr: bool,
    /// The connected (normalized) tracker address, once committed.
    pub addr: Option<String>,
    /// `INDEX` rows exactly as served (tracker sort order preserved).
    pub rows: Vec<IndexEntry>,
    /// `CATEGORIES` rows for the filter cycle.
    pub categories: Vec<CategoryCount>,
    /// Active category filter (`None` = all).
    pub filter: Option<String>,
    /// Selected row index (clamped against `rows`).
    pub selected: usize,
    /// The last fetched `HEALTH` detail pane, if any.
    pub health: Option<HealthDetail>,
    /// The in-pane error banner (network or parse), with retry via `r`.
    pub error: Option<String>,
    /// Whether a fetch is in flight.
    pub loading: bool,
    seq: u64,
}

impl BrowserState {
    /// Fresh state; `env_addr` (from `$RABBIT_TRACKER`) prefille the address
    /// input, which starts in editing mode until an address is committed.
    pub fn new(env_addr: Option<String>) -> Self {
        Self {
            addr_input: env_addr.map(|s| s.trim().to_string()).unwrap_or_default(),
            editing_addr: true,
            ..Self::default()
        }
    }

    /// Start a new request: bumps the sequence number (invalidating any
    /// in-flight reply), marks loading, clears the error banner.
    pub fn begin(&mut self) -> u64 {
        self.seq = self.seq.wrapping_add(1);
        self.loading = true;
        self.error = None;
        self.seq
    }

    /// Fold a completed fetch in. Stale sequence numbers are ignored.
    pub fn apply(&mut self, fetched: Fetched) {
        if fetched.seq != self.seq {
            return;
        }
        self.loading = false;
        match fetched.outcome {
            Outcome::Index(Ok((rows, categories))) => {
                self.rows = rows;
                if let Some(categories) = categories {
                    self.categories = categories;
                }
                self.selected = self.selected.min(self.rows.len().saturating_sub(1));
                self.health = None;
                self.error = None;
            }
            Outcome::Index(Err(err)) => self.error = Some(err),
            Outcome::Health(Ok(detail)) => {
                self.health = Some(detail);
                self.error = None;
            }
            Outcome::Health(Err(err)) => self.error = Some(format!("health: {err}")),
        }
    }

    /// Advance the category filter: all → first → … → last → all.
    pub fn cycle_filter(&mut self) {
        self.filter = match &self.filter {
            None => self.categories.first().map(|c| c.name.clone()),
            Some(current) => {
                match self.categories.iter().position(|c| &c.name == current) {
                    Some(i) if i + 1 < self.categories.len() => {
                        Some(self.categories[i + 1].name.clone())
                    }
                    // Last category (or one that vanished) wraps to "all".
                    _ => None,
                }
            }
        };
    }

    /// Move the selection one row down (`true`) or up (`false`), clamped.
    pub fn move_selection(&mut self, down: bool) {
        let len = self.rows.len();
        if len == 0 {
            return;
        }
        let sel = self.selected.min(len - 1);
        self.selected = if down {
            (sel + 1).min(len - 1)
        } else {
            sel.saturating_sub(1)
        };
    }

    /// The selected row, if any.
    pub fn selected_row(&self) -> Option<&IndexEntry> {
        self.rows
            .get(self.selected.min(self.rows.len().saturating_sub(1)))
    }
}

// ---------------------------------------------------------------------------
// Table rendering helpers (pure).
// ---------------------------------------------------------------------------

/// The fixed table header. `obs%` is deliberately terse — the pane title
/// spells out that uptime is tracker-observed, not authoritative.
pub fn table_header() -> String {
    format!(
        "{:<18} {:<21} {:>5} {:>6} {:>6}  {:<3} {}",
        "name", "addr", "users", "obs%", "seen", "sig", "categories"
    )
}

/// One table row, columns aligned with [`table_header`].
pub fn format_row(entry: &IndexEntry) -> String {
    format!(
        "{:<18.18} {:<21.21} {:>5} {:>6.6} {:>5}s  {:<3} {}",
        entry.name,
        entry.addr,
        entry.users,
        entry.uptime_pct,
        entry.last_seen_secs,
        if entry.signed { "✓" } else { "-" },
        entry.categories.join(",")
    )
}

/// One-line verification detail for the selected row: the signed badge is
/// backed by key prefix + generation so the descriptor can be fetched and
/// checked offline; unsigned rows say so.
pub fn selection_detail(entry: &IndexEntry) -> String {
    match (&entry.key_prefix, &entry.generation) {
        (Some(key), Some(generation)) => format!(
            "sel {}: signed ✓ key={key} gen={generation} — descriptor verifiable offline",
            entry.name
        ),
        _ => format!("sel {}: unsigned — nothing to verify", entry.name),
    }
}

// ---------------------------------------------------------------------------
// The network edge: one command line in, whole reply out, bounded.
// ---------------------------------------------------------------------------

/// Issue one status-port command (`INDEX`, `CATEGORIES`, `HEALTH ip:port`)
/// against `addr` and return the raw reply text. Connect, total I/O time
/// and reply size are all bounded; every failure becomes a display string
/// with the address in it (shown in-pane with a retry hint).
pub async fn query(addr: &str, command: &str) -> Result<String, String> {
    let mut stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| format!("connect to {addr} timed out"))?
        .map_err(|err| format!("connect to {addr} failed: {err}"))?;
    let exchange = async {
        stream.write_all(command.as_bytes()).await?;
        stream.write_all(b"\n").await?;
        let mut buf = Vec::new();
        (&mut stream)
            .take(MAX_RESPONSE)
            .read_to_end(&mut buf)
            .await?;
        Ok::<Vec<u8>, std::io::Error>(buf)
    };
    let buf = tokio::time::timeout(IO_TIMEOUT, exchange)
        .await
        .map_err(|_| format!("{addr} did not answer in time"))?
        .map_err(|err| format!("i/o with {addr} failed: {err}"))?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Vectors copied from the tracker's actual output format (see
    // `apps/tracker/src/service.rs` tests — `index_lines_…`,
    // `status_response_…` — and `health.rs` sparkline tests).
    const SIGNED_ROW: &str =
        "Wonderland\t10.0.0.1:5500\t12\tchat\t100.0\t0\tyes\tab00000000000001\t1700000000000";
    const UNSIGNED_ROW: &str = "Plain\t10.0.0.2:5510\t3\t-\t100.0\t0\tno\t-\t-";
    const HEALTH_OK: &str =
        "10.0.0.1:5500\tlive=yes\tuptime_24h=100.0\tfirst_seen_secs=0\tlast_seen_secs=0\tflaps=0\n#\n";

    #[test]
    fn parse_index_line_signed_vector() {
        let row = parse_index_line(SIGNED_ROW).unwrap();
        assert_eq!(row.name, "Wonderland");
        assert_eq!(row.addr, "10.0.0.1:5500");
        assert_eq!(row.users, 12);
        assert_eq!(row.categories, vec!["chat".to_string()]);
        assert_eq!(row.uptime_pct, "100.0");
        assert_eq!(row.last_seen_secs, 0);
        assert!(row.signed);
        assert_eq!(row.key_prefix.as_deref(), Some("ab00000000000001"));
        assert_eq!(row.generation.as_deref(), Some("1700000000000"));
    }

    #[test]
    fn parse_index_line_unsigned_vector() {
        let row = parse_index_line(UNSIGNED_ROW).unwrap();
        assert_eq!(row.name, "Plain");
        assert!(!row.signed);
        assert!(row.categories.is_empty());
        assert_eq!(row.key_prefix, None);
        assert_eq!(row.generation, None);
    }

    #[test]
    fn parse_index_line_multi_category() {
        let row =
            parse_index_line("Hub\t10.0.0.3:5520\t7\tchat,files\t87.5\t42\tno\t-\t-").unwrap();
        assert_eq!(
            row.categories,
            vec!["chat".to_string(), "files".to_string()]
        );
        assert_eq!(row.uptime_pct, "87.5");
        assert_eq!(row.last_seen_secs, 42);
    }

    #[test]
    fn parse_index_line_rejects_malformed() {
        // Wrong column count (8 and 10).
        assert_eq!(
            parse_index_line("A\t1.2.3.4:1\t1\t-\t100.0\t0\tno\t-"),
            None
        );
        assert_eq!(
            parse_index_line("A\t1.2.3.4:1\t1\t-\t100.0\t0\tno\t-\t-\textra"),
            None
        );
        // Non-numeric users / uptime / last_seen.
        assert_eq!(
            parse_index_line("A\t1.2.3.4:1\tmany\t-\t100.0\t0\tno\t-\t-"),
            None
        );
        assert_eq!(
            parse_index_line("A\t1.2.3.4:1\t1\t-\t1o0.o\t0\tno\t-\t-"),
            None
        );
        assert_eq!(
            parse_index_line("A\t1.2.3.4:1\t1\t-\t100.0\tsoon\tno\t-\t-"),
            None
        );
        // Unknown signed flag, empty name/addr.
        assert_eq!(
            parse_index_line("A\t1.2.3.4:1\t1\t-\t100.0\t0\tmaybe\t-\t-"),
            None
        );
        assert_eq!(
            parse_index_line("\t1.2.3.4:1\t1\t-\t100.0\t0\tno\t-\t-"),
            None
        );
        assert_eq!(parse_index_line("A\t\t1\t-\t100.0\t0\tno\t-\t-"), None);
        assert_eq!(parse_index_line(""), None);
    }

    #[test]
    fn parse_index_keeps_good_rows_and_flags_garbage() {
        // Both real vectors, served in tracker order (signed first).
        let text = format!("{SIGNED_ROW}\n{UNSIGNED_ROW}\n");
        let rows = parse_index(&text).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "Wonderland");
        assert_eq!(rows[1].name, "Plain");

        // Empty reply = empty directory (a real state, not an error).
        assert_eq!(parse_index("").unwrap(), Vec::new());

        // A malformed line among good ones is skipped.
        let mixed = format!("{SIGNED_ROW}\nnot a row\n");
        assert_eq!(parse_index(&mixed).unwrap().len(), 1);

        // All-garbage is an error, never a silent empty list.
        let err = parse_index("<html>nope</html>\n").unwrap_err();
        assert!(err.contains("unrecognized"), "got: {err}");

        // ERR replies pass the tracker's message through.
        let err = parse_index("ERR unknown command\n").unwrap_err();
        assert!(err.contains("unknown command"), "got: {err}");
    }

    #[test]
    fn parse_categories_vectors() {
        let cats = parse_categories("chat\t2\nfiles\t1\n").unwrap();
        assert_eq!(
            cats,
            vec![
                CategoryCount {
                    name: "chat".into(),
                    count: 2
                },
                CategoryCount {
                    name: "files".into(),
                    count: 1
                },
            ]
        );
        assert_eq!(parse_categories("").unwrap(), Vec::new());
        // Malformed lines skipped alongside good ones; all-garbage errors.
        assert_eq!(parse_categories("chat\t2\njunk line\n").unwrap().len(), 1);
        assert!(parse_categories("junk\tmany\n").is_err());
        assert!(parse_categories("ERR unknown command\n").is_err());
    }

    #[test]
    fn parse_health_ok_vectors() {
        let detail = parse_health(HEALTH_OK).unwrap();
        assert_eq!(detail.addr, "10.0.0.1:5500");
        assert!(detail.live);
        assert_eq!(detail.uptime_pct, "100.0");
        assert_eq!(detail.first_seen_secs, 0);
        assert_eq!(detail.last_seen_secs, 0);
        assert_eq!(detail.flaps, 0);
        assert_eq!(detail.sparkline, "#");

        // A flapping, currently-dead server with a longer sparkline
        // (sparkline shapes from the tracker's health tests: "+....", "+.").
        let text = "10.0.0.9:99\tlive=no\tuptime_24h=6.6\tfirst_seen_secs=86400\t\
                    last_seen_secs=1200\tflaps=2\n+....\n";
        let detail = parse_health(text).unwrap();
        assert!(!detail.live);
        assert_eq!(detail.uptime_pct, "6.6");
        assert_eq!(detail.first_seen_secs, 86_400);
        assert_eq!(detail.last_seen_secs, 1_200);
        assert_eq!(detail.flaps, 2);
        assert_eq!(detail.sparkline, "+....");
    }

    #[test]
    fn parse_health_rejects_err_and_malformed() {
        // The tracker's actual error replies.
        assert!(parse_health("ERR bad address\n")
            .unwrap_err()
            .contains("bad address"));
        assert!(parse_health("ERR unknown server\n")
            .unwrap_err()
            .contains("unknown server"));
        // Truncated: header without the sparkline line.
        let truncated = HEALTH_OK.lines().next().unwrap().to_string();
        assert!(parse_health(&truncated).unwrap_err().contains("sparkline"));
        // Wrong field count / broken key=value fields / bad numbers.
        assert!(parse_health("10.0.0.1:5500\tlive=yes\n#\n").is_err());
        let swapped = "10.0.0.1:5500\tuptime_24h=1.0\tlive=yes\tfirst_seen_secs=0\t\
                       last_seen_secs=0\tflaps=0\n#\n";
        assert!(parse_health(swapped).is_err());
        let bad_flaps = "10.0.0.1:5500\tlive=yes\tuptime_24h=1.0\tfirst_seen_secs=0\t\
                         last_seen_secs=0\tflaps=lots\n#\n";
        assert!(parse_health(bad_flaps).is_err());
        assert!(parse_health("").is_err());
    }

    #[test]
    fn normalize_tracker_addr_appends_default_port() {
        assert_eq!(
            normalize_tracker_addr("tracker.example").as_deref(),
            Some("tracker.example:4655")
        );
        assert_eq!(
            normalize_tracker_addr(" 10.0.0.1:9999 ").as_deref(),
            Some("10.0.0.1:9999")
        );
        assert_eq!(
            normalize_tracker_addr("[::1]:4655").as_deref(),
            Some("[::1]:4655")
        );
        assert_eq!(normalize_tracker_addr("   "), None);
        assert_eq!(normalize_tracker_addr(""), None);
    }

    fn row(name: &str) -> IndexEntry {
        parse_index_line(&format!("{name}\t10.0.0.1:5500\t1\t-\t100.0\t0\tno\t-\t-")).unwrap()
    }

    #[test]
    fn state_seq_gates_stale_replies() {
        let mut state = BrowserState::new(Some(" tracker.example:4655 ".into()));
        assert_eq!(state.addr_input, "tracker.example:4655");
        assert!(state.editing_addr);

        let stale = state.begin();
        let fresh = state.begin();
        assert!(state.loading);

        // The stale reply (superseded by the second begin) is dropped.
        state.apply(Fetched {
            seq: stale,
            outcome: Outcome::Index(Ok((vec![row("Stale")], None))),
        });
        assert!(state.rows.is_empty());
        assert!(state.loading);

        state.apply(Fetched {
            seq: fresh,
            outcome: Outcome::Index(Ok((vec![row("Fresh")], None))),
        });
        assert_eq!(state.rows.len(), 1);
        assert_eq!(state.rows[0].name, "Fresh");
        assert!(!state.loading);
    }

    #[test]
    fn state_index_apply_clamps_selection_and_clears_health() {
        let mut state = BrowserState::default();
        let seq = state.begin();
        state.apply(Fetched {
            seq,
            outcome: Outcome::Index(Ok((vec![row("A"), row("B"), row("C")], None))),
        });
        state.selected = 2;
        state.health = Some(parse_health(HEALTH_OK).unwrap());

        let seq = state.begin();
        state.apply(Fetched {
            seq,
            outcome: Outcome::Index(Ok((vec![row("A")], None))),
        });
        assert_eq!(state.selected, 0);
        assert!(state.health.is_none());
        assert_eq!(state.selected_row().unwrap().name, "A");
    }

    #[test]
    fn state_errors_keep_rows_and_surface_banner() {
        let mut state = BrowserState::default();
        let seq = state.begin();
        state.apply(Fetched {
            seq,
            outcome: Outcome::Index(Ok((vec![row("A")], Some(Vec::new())))),
        });

        let seq = state.begin();
        state.apply(Fetched {
            seq,
            outcome: Outcome::Index(Err("connect to h:1 timed out".into())),
        });
        // Stale-but-visible rows stay; the banner carries the failure.
        assert_eq!(state.rows.len(), 1);
        assert!(state.error.as_deref().unwrap().contains("timed out"));

        let seq = state.begin();
        state.apply(Fetched {
            seq,
            outcome: Outcome::Health(Err("tracker: unknown server".into())),
        });
        assert!(state.error.as_deref().unwrap().starts_with("health:"));
    }

    #[test]
    fn state_cycle_filter_wraps_through_categories() {
        let mut state = BrowserState::default();
        // No categories: cycling stays on "all".
        state.cycle_filter();
        assert_eq!(state.filter, None);

        state.categories = vec![
            CategoryCount {
                name: "chat".into(),
                count: 2,
            },
            CategoryCount {
                name: "files".into(),
                count: 1,
            },
        ];
        state.cycle_filter();
        assert_eq!(state.filter.as_deref(), Some("chat"));
        state.cycle_filter();
        assert_eq!(state.filter.as_deref(), Some("files"));
        state.cycle_filter();
        assert_eq!(state.filter, None);

        // A vanished category wraps back to "all" instead of panicking.
        state.filter = Some("gone".into());
        state.cycle_filter();
        assert_eq!(state.filter, None);
    }

    #[test]
    fn state_categories_none_keeps_previous_list() {
        let mut state = BrowserState::default();
        let seq = state.begin();
        state.apply(Fetched {
            seq,
            outcome: Outcome::Index(Ok((
                vec![row("A")],
                Some(vec![CategoryCount {
                    name: "chat".into(),
                    count: 1,
                }]),
            ))),
        });
        assert_eq!(state.categories.len(), 1);

        // CATEGORIES fetch failed on refresh: the old list survives.
        let seq = state.begin();
        state.apply(Fetched {
            seq,
            outcome: Outcome::Index(Ok((vec![row("A")], None))),
        });
        assert_eq!(state.categories.len(), 1);
    }

    #[test]
    fn state_selection_moves_and_clamps() {
        let mut state = BrowserState::default();
        state.move_selection(true); // empty: no-op, no panic
        assert_eq!(state.selected, 0);
        assert!(state.selected_row().is_none());

        state.rows = vec![row("A"), row("B")];
        state.move_selection(true);
        assert_eq!(state.selected, 1);
        state.move_selection(true); // clamped at the end
        assert_eq!(state.selected, 1);
        state.move_selection(false);
        assert_eq!(state.selected, 0);
        state.move_selection(false); // clamped at the top
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn table_rows_align_and_carry_the_badges() {
        let header = table_header();
        assert!(header.contains("obs%"), "got: {header}");

        let signed = format_row(&parse_index_line(SIGNED_ROW).unwrap());
        assert!(signed.contains("Wonderland"));
        assert!(signed.contains("✓"));
        assert!(signed.contains("100.0"));
        assert!(signed.contains("chat"));

        let unsigned = format_row(&parse_index_line(UNSIGNED_ROW).unwrap());
        assert!(unsigned.contains("Plain"));
        assert!(!unsigned.contains('✓'));

        let detail = selection_detail(&parse_index_line(SIGNED_ROW).unwrap());
        assert!(detail.contains("key=ab00000000000001"));
        assert!(detail.contains("gen=1700000000000"));
        let detail = selection_detail(&parse_index_line(UNSIGNED_ROW).unwrap());
        assert!(detail.contains("unsigned"));
    }

    /// End-to-end smoke against an **embedded** Looking Glass (the lib API,
    /// no tracker binary): real listener on 127.0.0.1:0, real `query`, real
    /// parsers. Ignored by default; run with
    /// `cargo test -p rabbit-tui -- --ignored`.
    #[tokio::test]
    #[ignore = "binds a localhost listener (embedded looking-glass); run with -- --ignored"]
    async fn smoke_embedded_looking_glass_index_and_health() {
        use looking_glass::service::run_status_tcp;
        use looking_glass::{Registry, ServerEntry, DEFAULT_TTL};
        use std::sync::Arc;

        let registry = Arc::new(Registry::new(DEFAULT_TTL));
        registry.register_unsigned(ServerEntry::unsigned(
            "Warren",
            "smoke fixture",
            ([127, 0, 0, 1], 5500).into(),
            4,
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let tracker = listener.local_addr().unwrap().to_string();
        tokio::spawn(run_status_tcp(listener, Arc::clone(&registry)));

        let index = query(&tracker, "INDEX").await.expect("INDEX reply");
        let rows = parse_index(&index).expect("parse INDEX");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Warren");
        assert_eq!(rows[0].users, 4);
        assert!(!rows[0].signed);

        let health = query(&tracker, &format!("HEALTH {}", rows[0].addr))
            .await
            .expect("HEALTH reply");
        let detail = parse_health(&health).expect("parse HEALTH");
        assert!(detail.live);
        assert_eq!(detail.flaps, 0);

        let cats = query(&tracker, "CATEGORIES")
            .await
            .expect("CATEGORIES reply");
        assert!(parse_categories(&cats)
            .expect("parse CATEGORIES")
            .is_empty());

        // Garbage still gets a total, parseable answer.
        let err = query(&tracker, "FROLIC").await.expect("ERR reply");
        assert!(parse_index(&err).is_err());
    }
}
