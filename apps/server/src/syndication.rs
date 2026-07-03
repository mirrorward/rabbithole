//! RSS/Atom feed ingestion service (Wave 10): polls configured feeds and
//! posts fresh items to their mapped boards.
//!
//! The pure layers all live in [`rabbithole-legacy-syndication`](rabbithole_legacy_syndication):
//! feed parsing ([`syndication::parse`]), board mapping
//! ([`syndication::to_post_drafts`]), the seen-set partition
//! ([`SeenSet`]) and the clockless conditional-GET poll state machine
//! ([`PollState`]). This module is only the *transport + wiring* glue, in the
//! same spirit as [`crate::ftn`]:
//!
//! ```text
//!   config (syndication_feeds: url → board slug)
//!        │  every tick, for each due feed
//!        ▼
//!   minimal HTTP/1.1 GET  ──ETag/Last-Modified──▶  PollState::on_response
//!        │ 200 body (304 → reschedule, error → backoff)
//!        ▼
//!   parse → SeenSet.partition → to_post_drafts ──▶ BoardService.post
//!                                                   (gateway author seed, RBAC)
//! ```
//!
//! # The HTTP client
//!
//! Deliberately minimal — a hand-rolled HTTP/1.1 `GET` over a tokio
//! `TcpStream` (rustls for `https`), because feeds need exactly: a `Host`
//! header, conditional GET (`If-None-Match`/`If-Modified-Since` replayed from
//! the [`PollState`] validators), `Connection: close` framing, 3xx redirects
//! (capped at [`MAX_REDIRECTS`] hops), a response size cap
//! ([`MAX_BODY_BYTES`]) and a whole-fetch timeout. `Content-Length` and
//! `chunked` bodies are both handled; everything else about HTTP is out of
//! scope.
//!
//! **TLS roots decision:** `webpki-roots` was already in the workspace
//! dependency graph (sqlx's rustls stack pulls it), so per the Wave 10 plan it
//! is promoted to a direct dependency and `https` feeds verify against the
//! bundled Mozilla root set. No system cert store is consulted.
//!
//! # Dedup, twice
//!
//! - **Durable, per feed:** each feed's posted item ids ([`dedup_id`]s)
//!   persist to a line-per-id file under `<data_dir>/syndication/`, reloaded
//!   into a [`SeenSet`] at boot — a restart never re-posts old items.
//! - **Shared, cross-network:** ids also pass through the burrow-wide
//!   [`DedupStore`](rabbithole_server_core::DedupStore) gate
//!   (`SeenKey::Syndication`), the same subsystem federation uses, so a feed
//!   mirrored via two URLs cannot double-post either.
//!
//! Opt-in via config (`syndication_enabled`, default **off**) with the
//! `syndication_feeds` map (TOML-only, like `ftn_areas`). RBAC is respected:
//! the gateway posts only while a member-baseline subject holds `BOARD_POST`.
//!
//! ## Deliberately deferred
//!
//! - **Feed-declared TTLs**: [`PollState`] honors a `feed_ttl_secs` argument,
//!   but wiring RSS `<ttl>` / `sy:updatePeriod` out of the document into it is
//!   left for a later pass (`None` is passed today).
//! - **IPv6 literal hosts** in feed URLs (bracketed authorities) are not
//!   parsed; use a hostname.
//! - **Compressed responses**: no `Accept-Encoding` is sent, so servers must
//!   reply with identity bodies (they do when the header is absent).

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use rabbithole_legacy_syndication::{
    self as syndication, BoardMapping, Feed, PollConfig, PollDecision, PollState, PostDraft,
    SeenSet,
};
use rabbithole_server_core::{Caps, Role, SeenKey, ServerEvent, Subject};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;

use crate::Shared;

/// Synthetic gateway account id, matching the FTN gateway's convention: a
/// member-baseline pseudo-account so ACLs that gag members gag the gateway.
const SYNDICATION_GATEWAY_ACCOUNT: i64 = 0;

/// Response body cap: a feed document larger than this is refused.
pub const MAX_BODY_BYTES: usize = 1024 * 1024; // 1 MiB

/// Extra allowance over [`MAX_BODY_BYTES`] for the status line + headers
/// (and chunked-framing overhead) when reading the raw response.
const MAX_RESPONSE_BYTES: usize = MAX_BODY_BYTES + 64 * 1024;

/// Maximum 3xx redirect hops followed per fetch.
pub const MAX_REDIRECTS: usize = 3;

/// Whole-fetch timeout (connect + TLS + request + response).
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Scheduler cadence: how often the background task checks which feeds are
/// due. Cheap (a clock compare per feed), so it can be much shorter than the
/// poll interval itself.
const TICK_SECS: u64 = 15;

/// Spawn the background feed-ingest service. Call only when
/// `syndication_enabled` is set; the task ends on [`ServerEvent::Shutdown`].
/// `state_dir` holds the durable per-feed seen files (created if missing).
pub fn spawn_syndication(shared: Arc<Shared>, state_dir: PathBuf) -> JoinHandle<()> {
    tokio::spawn(async move {
        match SyndicationService::new(shared, state_dir).await {
            Ok(mut svc) => svc.run().await,
            Err(e) => tracing::error!("syndication service failed to start: {e:#}"),
        }
    })
}

/// One configured feed's live state: its poll schedule, validators, and the
/// durable seen-set of item ids already posted.
struct FeedRuntime {
    url: String,
    mapping: BoardMapping,
    poll: PollState,
    seen: SeenSet,
    seen_path: PathBuf,
}

/// The feed-ingest service. The fetch loop is kept testable: tests construct
/// one with [`SyndicationService::new`] and drive [`poll_due`] directly with
/// a chosen clock instead of spawning [`run`].
///
/// [`poll_due`]: SyndicationService::poll_due
/// [`run`]: SyndicationService::run
pub struct SyndicationService {
    shared: Arc<Shared>,
    poll_cfg: PollConfig,
    fetch_timeout: Duration,
    feeds: Vec<FeedRuntime>,
}

impl SyndicationService {
    /// Build from the live config snapshot: one [`FeedRuntime`] per
    /// `syndication_feeds` entry (sorted by URL for determinism), each due
    /// immediately, with its durable seen file loaded from `state_dir`.
    pub async fn new(shared: Arc<Shared>, state_dir: PathBuf) -> Result<SyndicationService> {
        let (mut entries, base_secs) = {
            let cfg = shared.config.read();
            let entries: Vec<(String, String)> = cfg
                .syndication_feeds
                .iter()
                .map(|(u, s)| (u.clone(), s.clone()))
                .collect();
            (entries, cfg.syndication_poll_secs)
        };
        entries.sort();
        let poll_cfg = PollConfig {
            base_interval_secs: base_secs.max(1),
            ..PollConfig::default()
        };
        tokio::fs::create_dir_all(&state_dir).await?;
        let now = unix_now();
        let mut feeds = Vec::with_capacity(entries.len());
        for (url, slug) in entries {
            let seen_path = state_dir.join(format!("seen-{}.txt", feed_key(&url)));
            let seen = load_seen(&seen_path).await;
            feeds.push(FeedRuntime {
                mapping: BoardMapping::new(slug),
                poll: PollState::initial(now),
                url,
                seen,
                seen_path,
            });
        }
        // Seed a stats row per feed so the admin monitor lists configured
        // feeds before their first poll.
        for f in &feeds {
            shared.stats.feed_register(&f.url);
        }
        Ok(SyndicationService {
            shared,
            poll_cfg,
            fetch_timeout: FETCH_TIMEOUT,
            feeds,
        })
    }

    /// Number of configured feeds.
    pub fn feed_count(&self) -> usize {
        self.feeds.len()
    }

    /// The scheduler loop: poll due feeds on a fixed tick, stop on shutdown.
    pub async fn run(&mut self) {
        use tokio::sync::broadcast::error::RecvError;
        let mut rx = self.shared.bus.subscribe();
        let mut tick = tokio::time::interval(Duration::from_secs(TICK_SECS));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    let posted = self.poll_due(unix_now()).await;
                    if posted > 0 {
                        tracing::info!(posted, "syndication: fresh feed items posted");
                    }
                }
                ev = rx.recv() => match ev {
                    Ok(ServerEvent::Shutdown) | Err(RecvError::Closed) => break,
                    Ok(_) => {}
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!(missed = n, "syndication loop lagged behind the bus");
                    }
                },
            }
        }
    }

    /// Poll every feed that is due at `now` (unix seconds); returns how many
    /// board posts were created. Exposed so tests can drive the loop with a
    /// deterministic clock.
    pub async fn poll_due(&mut self, now: i64) -> usize {
        let mut posted = 0;
        for idx in 0..self.feeds.len() {
            if self.feeds[idx].poll.is_due(now) {
                posted += self.poll_feed(idx, now).await;
            }
        }
        posted
    }

    /// Fetch one feed, advance its [`PollState`], and ingest a fresh body.
    async fn poll_feed(&mut self, idx: usize, now: i64) -> usize {
        let (url, etag, last_modified) = {
            let f = &self.feeds[idx];
            (
                f.url.clone(),
                f.poll.etag.clone(),
                f.poll.last_modified.clone(),
            )
        };
        let fetched = http_get(
            &url,
            etag.as_deref(),
            last_modified.as_deref(),
            self.fetch_timeout,
        )
        .await;
        let stamp = chrono::Utc::now().timestamp_millis();
        let resp = match fetched {
            Ok(resp) => resp,
            Err(e) => {
                let f = &mut self.feeds[idx];
                let (next, _) = f.poll.on_transport_error(&self.poll_cfg, None, now);
                f.poll = next;
                self.shared.stats.feed_poll(&url, stamp, "error");
                tracing::warn!(feed = %url, failures = f.poll.failures, "syndication fetch failed: {e:#}");
                return 0;
            }
        };
        let (next, decision) = self.feeds[idx].poll.on_response(
            &self.poll_cfg,
            resp.status,
            resp.etag.as_deref(),
            resp.last_modified.as_deref(),
            None, // feed-declared TTL wiring is deferred (see module docs)
            now,
        );
        self.feeds[idx].poll = next;
        match decision {
            PollDecision::Modified => {
                self.shared.stats.feed_poll(&url, stamp, "ok");
                self.ingest(idx, &resp.body).await
            }
            PollDecision::NotModified => {
                self.shared.stats.feed_poll(&url, stamp, "not_modified");
                tracing::debug!(feed = %url, "syndication: not modified");
                0
            }
            PollDecision::Failed => {
                self.shared.stats.feed_poll(&url, stamp, "error");
                tracing::warn!(feed = %url, status = resp.status, "syndication fetch error status");
                0
            }
        }
    }

    /// Parse a fresh body, drop items already seen, and post the rest to the
    /// mapped board. Ids are recorded (memory + durable file + shared dedup
    /// gate) only for items actually posted, so a missing board or a revoked
    /// capability never permanently swallows an item.
    async fn ingest(&mut self, idx: usize, body: &[u8]) -> usize {
        let text = String::from_utf8_lossy(body);
        let parsed = match syndication::parse(&text) {
            Ok(feed) => feed,
            Err(e) => {
                tracing::warn!(feed = %self.feeds[idx].url, "syndication: unparseable body: {e}");
                return 0;
            }
        };
        let fresh = Feed {
            items: self.feeds[idx].seen.partition(&parsed.items).fresh,
            ..parsed
        };
        if fresh.items.is_empty() {
            return 0;
        }
        let drafts = syndication::to_post_drafts(&fresh, &self.feeds[idx].mapping);
        let seen_count = drafts.len() as u64;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut posted = 0usize;
        let mut dupes = 0u64;
        let mut new_ids: Vec<String> = Vec::new();
        for draft in &drafts {
            let key = SeenKey::Syndication(draft.dedup_id.clone());
            if self.shared.dedup.seen(&key) {
                // Another ingest path already handled this item; remember it
                // here too so future polls skip the re-parse churn.
                dupes += 1;
                new_ids.push(draft.dedup_id.clone());
                continue;
            }
            match post_draft(&self.shared, draft).await {
                Ok(true) => {
                    self.shared.dedup.check_and_record(key, now_ms);
                    new_ids.push(draft.dedup_id.clone());
                    posted += 1;
                }
                Ok(false) => {} // policy drop: retry on a later poll
                Err(e) => {
                    tracing::warn!(feed = %self.feeds[idx].url, "syndication post failed: {e:#}");
                }
            }
        }
        if !new_ids.is_empty() {
            let f = &mut self.feeds[idx];
            for id in &new_ids {
                f.seen.insert(id.clone());
            }
            if let Err(e) = append_seen(&f.seen_path, &new_ids).await {
                tracing::warn!(file = %f.seen_path.display(), "syndication: seen file append failed: {e}");
            }
        }
        self.shared
            .stats
            .feed_ingest(&self.feeds[idx].url, seen_count, posted as u64, dupes);
        posted
    }
}

/// Post one draft into its mapped board (mirrors `ftn::deliver_echomail`):
/// RBAC gate, board must exist and be postable, gateway author seed, board
/// event on the bus. `Ok(false)` = dropped by policy (retryable), `Ok(true)`
/// = posted.
async fn post_draft(shared: &Arc<Shared>, draft: &PostDraft) -> Result<bool> {
    if !shared
        .perms
        .allows(&gateway_subject(), "board", Caps::BOARD_POST)
    {
        tracing::warn!("syndication: gateway lacks BOARD_POST; item not posted");
        return Ok(false);
    }
    match shared.boards.board(&draft.board).await {
        Ok(Some(b)) if b.kind == 2 => {}
        _ => {
            tracing::warn!(board = %draft.board, "syndication: target board missing or not postable");
            return Ok(false);
        }
    }
    let author = format!("{}@rss", non_empty(&draft.author, "Feed"));
    let seed = syndication_author_seed(shared, &format!("{}|{}", draft.board, draft.author));
    let subject = non_empty(&draft.subject, "(untitled)");
    let now = chrono::Utc::now().timestamp_millis();
    match shared
        .boards
        .post(
            &draft.board,
            None,
            &author,
            &seed,
            &subject,
            &draft.body,
            "text/plain",
            now,
        )
        .await
    {
        Ok(row) => {
            shared.bus.publish(ServerEvent::BoardPost {
                board: row.board_slug.clone(),
                id: row.event_id,
                root: row.root_id,
            });
            Ok(true)
        }
        Err(e) => {
            tracing::warn!("syndication: board post failed: {e}");
            Ok(false)
        }
    }
}

/// A member-baseline subject for the gateway's RBAC checks (same convention
/// as the FTN gateway): an ACL that revokes member posting gags the feed too.
fn gateway_subject() -> Subject {
    Subject {
        account_id: SYNDICATION_GATEWAY_ACCOUNT,
        role: Role::User,
        class_id: None,
        class_mask: 0,
        grant_mask: 0,
        revoke_mask: 0,
    }
}

/// A stable author signing seed for feed-authored board posts, namespaced
/// away from native per-account seeds and the FTN gateway seed.
fn syndication_author_seed(shared: &Shared, key: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rabbithole-syndication-author-seed-v1");
    hasher.update(&shared.server_signing_seed);
    hasher.update(key.as_bytes());
    *hasher.finalize().as_bytes()
}

/// `s` if non-blank, else `fallback` — as an owned `String`.
fn non_empty(s: &str, fallback: &str) -> String {
    if s.trim().is_empty() {
        fallback.to_string()
    } else {
        s.to_string()
    }
}

/// Current unix time in seconds (the poll state machine's clock unit).
fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Short stable file-name key for a feed URL.
fn feed_key(url: &str) -> String {
    hex::encode(&blake3::hash(url.as_bytes()).as_bytes()[..8])
}

/// Load a durable seen file (one item id per line); missing file = empty set.
async fn load_seen(path: &Path) -> SeenSet {
    match tokio::fs::read_to_string(path).await {
        Ok(text) => SeenSet::from_ids(text.lines().map(str::trim).filter(|l| !l.is_empty())),
        Err(_) => SeenSet::new(),
    }
}

/// Append newly-posted item ids to the durable seen file.
async fn append_seen(path: &Path, ids: &[String]) -> std::io::Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    let mut block = String::with_capacity(ids.iter().map(|i| i.len() + 1).sum());
    for id in ids {
        block.push_str(id);
        block.push('\n');
    }
    file.write_all(block.as_bytes()).await?;
    file.flush().await
}

// ---------------------------------------------------------------------------
// Minimal HTTP/1.1 GET client (tokio TcpStream + rustls for https)
// ---------------------------------------------------------------------------

/// What the poll loop needs from a fetch: the status, the caching validators,
/// and the (already de-framed) body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchResponse {
    pub status: u16,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub body: Vec<u8>,
}

/// A parsed feed URL. Only `http` and `https` schemes are accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedUrl {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    /// Absolute path + query, always starting with `/`.
    pub path: String,
}

impl FeedUrl {
    /// Parse `http://host[:port]/path` / `https://…`. No IPv6 literals.
    pub fn parse(url: &str) -> Result<FeedUrl> {
        let (tls, rest) = if let Some(r) = url.strip_prefix("https://") {
            (true, r)
        } else if let Some(r) = url.strip_prefix("http://") {
            (false, r)
        } else {
            bail!("unsupported feed url scheme: {url}");
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        if authority.is_empty() {
            bail!("feed url has no host: {url}");
        }
        let default_port = if tls { 443 } else { 80 };
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (
                h.to_string(),
                p.parse::<u16>()
                    .map_err(|_| anyhow!("bad port in feed url: {url}"))?,
            ),
            _ => (authority.to_string(), default_port),
        };
        if host.is_empty() {
            bail!("feed url has no host: {url}");
        }
        Ok(FeedUrl {
            tls,
            host,
            port,
            path: path.to_string(),
        })
    }

    /// The `Host` header value: the port is included only when non-default.
    pub fn host_header(&self) -> String {
        let default_port = if self.tls { 443 } else { 80 };
        if self.port == default_port {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// Resolve a `Location` header against the current URL: absolute URLs are
/// re-parsed, `/rooted` paths keep host+scheme, and bare relative paths join
/// onto the current path's directory.
fn resolve_location(current: &FeedUrl, location: &str) -> Result<FeedUrl> {
    let loc = location.trim();
    if loc.starts_with("http://") || loc.starts_with("https://") {
        return FeedUrl::parse(loc);
    }
    let mut next = current.clone();
    if let Some(rest) = loc.strip_prefix("//") {
        let scheme = if current.tls { "https://" } else { "http://" };
        return FeedUrl::parse(&format!("{scheme}{rest}"));
    }
    if loc.starts_with('/') {
        next.path = loc.to_string();
        return Ok(next);
    }
    if loc.is_empty() {
        bail!("empty redirect location");
    }
    let dir_end = current.path.rfind('/').unwrap_or(0);
    next.path = format!("{}/{}", &current.path[..dir_end], loc);
    Ok(next)
}

/// Fetch `url` with a minimal conditional HTTP/1.1 GET, following up to
/// [`MAX_REDIRECTS`] redirect hops, under `timeout` overall. Transport-level
/// problems (connect/TLS/framing/size-cap) are `Err`; any HTTP status —
/// including an unresolvable 3xx — comes back as a [`FetchResponse`] for
/// [`PollState::on_response`] to judge.
pub async fn http_get(
    url: &str,
    if_none_match: Option<&str>,
    if_modified_since: Option<&str>,
    timeout: Duration,
) -> Result<FetchResponse> {
    tokio::time::timeout(
        timeout,
        http_get_inner(url, if_none_match, if_modified_since),
    )
    .await
    .map_err(|_| anyhow!("feed fetch timed out after {timeout:?}"))?
}

async fn http_get_inner(
    url: &str,
    if_none_match: Option<&str>,
    if_modified_since: Option<&str>,
) -> Result<FetchResponse> {
    let mut target = FeedUrl::parse(url)?;
    let mut hops = 0usize;
    loop {
        let request = build_request(&target, if_none_match, if_modified_since);
        let raw = exchange(&target, request.as_bytes()).await?;
        let resp = parse_http_response(&raw)?;
        if matches!(resp.status, 301 | 302 | 303 | 307 | 308) && hops < MAX_REDIRECTS {
            if let Some(loc) = header(&resp.headers, "location") {
                if let Ok(next) = resolve_location(&target, loc) {
                    target = next;
                    hops += 1;
                    continue;
                }
            }
        }
        return Ok(FetchResponse {
            status: resp.status,
            etag: header(&resp.headers, "etag").map(str::to_string),
            last_modified: header(&resp.headers, "last-modified").map(str::to_string),
            body: resp.body,
        });
    }
}

/// Serialize the request head. `Connection: close` keeps framing simple: the
/// response ends at EOF (with `Content-Length`/chunked handled when present).
fn build_request(
    target: &FeedUrl,
    if_none_match: Option<&str>,
    if_modified_since: Option<&str>,
) -> String {
    let mut req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: rabbithole-burrow/{} (+syndication)\r\nAccept: application/rss+xml, application/atom+xml, application/xml;q=0.9, text/xml;q=0.8, */*;q=0.5\r\nConnection: close\r\n",
        target.path,
        target.host_header(),
        env!("CARGO_PKG_VERSION"),
    );
    if let Some(v) = if_none_match {
        req.push_str("If-None-Match: ");
        req.push_str(v);
        req.push_str("\r\n");
    }
    if let Some(v) = if_modified_since {
        req.push_str("If-Modified-Since: ");
        req.push_str(v);
        req.push_str("\r\n");
    }
    req.push_str("\r\n");
    req
}

/// Connect, send the request, and read the raw response to EOF (size-capped).
async fn exchange(target: &FeedUrl, request: &[u8]) -> Result<Vec<u8>> {
    let tcp = TcpStream::connect((target.host.as_str(), target.port))
        .await
        .map_err(|e| anyhow!("connect {}:{}: {e}", target.host, target.port))?;
    if target.tls {
        tls_exchange(tcp, &target.host, request).await
    } else {
        plain_exchange(tcp, request).await
    }
}

/// Plain-`http` request/response over the socket, then a graceful shutdown.
async fn plain_exchange(mut tcp: TcpStream, request: &[u8]) -> Result<Vec<u8>> {
    tcp.write_all(request).await?;
    let mut response = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = tcp.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        response.extend_from_slice(&buf[..n]);
        if response.len() > MAX_RESPONSE_BYTES {
            bail!("response exceeds the {MAX_BODY_BYTES}-byte feed cap");
        }
    }
    let _ = tcp.shutdown().await;
    Ok(response)
}

/// The shared rustls client config: webpki (Mozilla) roots, no client auth.
fn tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            Arc::new(
                rustls::ClientConfig::builder()
                    .with_root_certificates(roots)
                    .with_no_client_auth(),
            )
        })
        .clone()
}

/// `https` request/response: drive a sans-IO [`rustls::ClientConnection`]
/// over the tokio socket (no tokio-rustls in the workspace graph, and this is
/// the only TLS-over-TCP client). Ends with a close_notify + socket shutdown.
async fn tls_exchange(mut tcp: TcpStream, host: &str, request: &[u8]) -> Result<Vec<u8>> {
    let name = rustls_pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| anyhow!("invalid TLS server name: {host}"))?;
    let mut conn = rustls::ClientConnection::new(tls_config(), name)?;
    std::io::Write::write_all(&mut conn.writer(), request)?;

    let mut response = Vec::new();
    let mut net_in = [0u8; 16 * 1024];
    'session: loop {
        // Flush handshake/app data rustls wants on the wire.
        while conn.wants_write() {
            let mut out = Vec::new();
            conn.write_tls(&mut out)?;
            if out.is_empty() {
                break;
            }
            tcp.write_all(&out).await?;
        }
        let n = tcp.read(&mut net_in).await?;
        let eof = n == 0;
        let mut slice = &net_in[..n];
        while !slice.is_empty() {
            match conn.read_tls(&mut slice) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) => return Err(anyhow!("tls read: {e}")),
            }
        }
        let state = conn
            .process_new_packets()
            .map_err(|e| anyhow!("tls: {e}"))?;
        // Drain whatever plaintext became available.
        loop {
            let mut buf = [0u8; 16 * 1024];
            match std::io::Read::read(&mut conn.reader(), &mut buf) {
                Ok(0) => break 'session, // clean close_notify
                Ok(m) => {
                    response.extend_from_slice(&buf[..m]);
                    if response.len() > MAX_RESPONSE_BYTES {
                        bail!("response exceeds the {MAX_BODY_BYTES}-byte feed cap");
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break 'session,
                Err(e) => return Err(e.into()),
            }
        }
        if state.peer_has_closed() || eof {
            break;
        }
    }
    // Graceful close: close_notify, flush, then shut the socket down.
    conn.send_close_notify();
    while conn.wants_write() {
        let mut out = Vec::new();
        if conn.write_tls(&mut out).is_err() || out.is_empty() {
            break;
        }
        if tcp.write_all(&out).await.is_err() {
            break;
        }
    }
    let _ = tcp.shutdown().await;
    Ok(response)
}

/// A raw response split into status/headers/body, with transfer framing
/// (`Content-Length` / `chunked`) already applied to the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    /// Lowercased header names, trimmed values, in wire order.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// First value of `name` (lowercase) among parsed headers.
pub fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

/// Parse an HTTP/1.x response read to EOF. Enforces [`MAX_BODY_BYTES`] on the
/// de-framed body and rejects truncated `Content-Length`/chunked bodies (a
/// cut-off feed must back off, not half-post).
pub fn parse_http_response(raw: &[u8]) -> Result<HttpResponse> {
    let head_end = find_subslice(raw, b"\r\n\r\n").ok_or_else(|| anyhow!("no response head"))?;
    let head = String::from_utf8_lossy(&raw[..head_end]);
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let mut parts = status_line.split_whitespace();
    let proto = parts.next().unwrap_or_default();
    if !proto.starts_with("HTTP/1.") {
        bail!("not an HTTP/1.x response: {status_line:?}");
    }
    let status: u16 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("bad status line: {status_line:?}"))?;
    let mut headers = Vec::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    let rest = &raw[head_end + 4..];
    let chunked = header(&headers, "transfer-encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    let body = if chunked {
        decode_chunked(rest)?
    } else if let Some(len) =
        header(&headers, "content-length").and_then(|v| v.parse::<usize>().ok())
    {
        if rest.len() < len {
            bail!("truncated body: got {} of {len} bytes", rest.len());
        }
        rest[..len].to_vec()
    } else {
        rest.to_vec()
    };
    if body.len() > MAX_BODY_BYTES {
        bail!("body exceeds the {MAX_BODY_BYTES}-byte feed cap");
    }
    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

/// Decode a complete `Transfer-Encoding: chunked` body. Trailers are ignored;
/// truncation or malformed framing is an error.
pub fn decode_chunked(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    loop {
        let line_end = find_subslice(&data[pos..], b"\r\n")
            .map(|i| pos + i)
            .ok_or_else(|| anyhow!("truncated chunk size line"))?;
        let size_text = String::from_utf8_lossy(&data[pos..line_end]);
        let size_hex = size_text.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|_| anyhow!("bad chunk size: {size_hex:?}"))?;
        pos = line_end + 2;
        if size == 0 {
            return Ok(out); // final chunk; any trailers are ignored
        }
        let end = pos
            .checked_add(size)
            .filter(|&e| e <= data.len())
            .ok_or_else(|| anyhow!("truncated chunk data"))?;
        out.extend_from_slice(&data[pos..end]);
        if out.len() > MAX_BODY_BYTES {
            bail!("body exceeds the {MAX_BODY_BYTES}-byte feed cap");
        }
        pos = end;
        if data.get(pos..pos + 2) != Some(&b"\r\n"[..]) {
            bail!("missing chunk terminator");
        }
        pos += 2;
    }
}

/// First index of `needle` in `haystack`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_parsing_schemes_ports_paths() {
        let u = FeedUrl::parse("http://warren.example/feed.xml").unwrap();
        assert_eq!(
            u,
            FeedUrl {
                tls: false,
                host: "warren.example".into(),
                port: 80,
                path: "/feed.xml".into()
            }
        );
        assert_eq!(u.host_header(), "warren.example");

        let u = FeedUrl::parse("https://warren.example/a/b?c=d").unwrap();
        assert!(u.tls);
        assert_eq!(u.port, 443);
        assert_eq!(u.path, "/a/b?c=d");

        let u = FeedUrl::parse("http://127.0.0.1:8080").unwrap();
        assert_eq!((u.port, u.path.as_str()), (8080, "/"));
        assert_eq!(u.host_header(), "127.0.0.1:8080");

        assert!(FeedUrl::parse("ftp://x/").is_err());
        assert!(FeedUrl::parse("http:///nohost").is_err());
        assert!(FeedUrl::parse("http://host:99999/").is_err());
    }

    #[test]
    fn location_resolution() {
        let cur = FeedUrl::parse("https://warren.example:8443/feeds/all.xml").unwrap();
        // Absolute.
        let n = resolve_location(&cur, "http://other.example/f.xml").unwrap();
        assert_eq!(
            (n.tls, n.host.as_str(), n.port),
            (false, "other.example", 80)
        );
        // Scheme-relative keeps https.
        let n = resolve_location(&cur, "//mirror.example/f.xml").unwrap();
        assert!(n.tls);
        assert_eq!(n.host, "mirror.example");
        // Rooted path keeps host and port.
        let n = resolve_location(&cur, "/new/feed.xml").unwrap();
        assert_eq!((n.host.as_str(), n.port), ("warren.example", 8443));
        assert_eq!(n.path, "/new/feed.xml");
        // Relative joins the directory.
        let n = resolve_location(&cur, "latest.xml").unwrap();
        assert_eq!(n.path, "/feeds/latest.xml");
        assert!(resolve_location(&cur, "").is_err());
    }

    #[test]
    fn request_carries_conditional_headers() {
        let u = FeedUrl::parse("http://h.example/f.xml").unwrap();
        let bare = build_request(&u, None, None);
        assert!(bare.starts_with("GET /f.xml HTTP/1.1\r\n"));
        assert!(bare.contains("Host: h.example\r\n"));
        assert!(bare.contains("Connection: close\r\n"));
        assert!(!bare.contains("If-None-Match"));
        assert!(bare.ends_with("\r\n\r\n"));

        let cond = build_request(&u, Some("\"v1\""), Some("Wed, 02 Jul 2003 05:00:00 GMT"));
        assert!(cond.contains("If-None-Match: \"v1\"\r\n"));
        assert!(cond.contains("If-Modified-Since: Wed, 02 Jul 2003 05:00:00 GMT\r\n"));
    }

    #[test]
    fn response_parsing_content_length_and_extra_bytes() {
        let raw = b"HTTP/1.1 200 OK\r\nETag: \"e1\"\r\nContent-Length: 5\r\n\r\nhellojunk";
        let r = parse_http_response(raw).unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(header(&r.headers, "etag"), Some("\"e1\""));
        assert_eq!(r.body, b"hello");

        // Truncated content-length is an error, not a half body.
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 50\r\n\r\nshort";
        assert!(parse_http_response(raw).is_err());
    }

    #[test]
    fn response_parsing_304_and_no_length() {
        let raw = b"HTTP/1.1 304 Not Modified\r\nEtag: \"e2\"\r\nLast-Modified: yesterday\r\n\r\n";
        let r = parse_http_response(raw).unwrap();
        assert_eq!(r.status, 304);
        assert_eq!(header(&r.headers, "last-modified"), Some("yesterday"));
        assert!(r.body.is_empty());

        // No framing headers: body runs to EOF.
        let raw = b"HTTP/1.0 200 OK\r\n\r\nall the rest";
        assert_eq!(parse_http_response(raw).unwrap().body, b"all the rest");

        assert!(parse_http_response(b"SIP/2.0 200 OK\r\n\r\n").is_err());
        assert!(parse_http_response(b"HTTP/1.1 200").is_err(), "no head end");
    }

    #[test]
    fn chunked_bodies_decode_and_reject_truncation() {
        let raw =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\n\r\n";
        let r = parse_http_response(raw).unwrap();
        assert_eq!(r.body, b"hello world");

        assert!(decode_chunked(b"5\r\nhel").is_err(), "cut mid-chunk");
        assert!(decode_chunked(b"zz\r\n").is_err(), "bad size");
        assert!(decode_chunked(b"5\r\nhelloXX").is_err(), "bad terminator");
        assert_eq!(decode_chunked(b"0\r\n\r\n").unwrap(), b"");
    }

    #[test]
    fn feed_key_is_stable_and_short() {
        let a = feed_key("http://a.example/feed");
        assert_eq!(a.len(), 16);
        assert_eq!(a, feed_key("http://a.example/feed"));
        assert_ne!(a, feed_key("http://b.example/feed"));
    }
}
