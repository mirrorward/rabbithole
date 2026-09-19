//! Slice 2 of the native swarm backend: source discovery + the multi-source
//! download orchestration, Tauri-free so it's unit-testable. The later Tauri
//! command surface (Slice 4) just calls [`run_swarm_download`] and forwards a
//! progress `emit`; the ui-web `TransferBackend` (Slice 5) drives it over IPC.

#![cfg_attr(rustfmt, rustfmt_skip)]

use std::path::Path;

use rabbithole_core::{Client, ClientError};
use rabbithole_proto::swarm::SourceList;
use std::sync::Arc;

use rabbithole_proto::swarm::AdvertEntry;
use rabbithole_swarm::peer::PeerError;
use rabbithole_swarm::{
    fetch_swarm_from, BaoPiece, FetchReport, HaveMap, PeerSource, RangeSource, SeedStore,
    SourcePeer, UNIT_SIZE,
};

/// How long the burrow may take over one proved range before it is let go
/// as a source for this download.
const ORIGIN_ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// How soon a burrow still making a file's proofs is asked again: at first,
/// and at most.
const ORIGIN_BUSY_RETRY: std::time::Duration = std::time::Duration::from_millis(250);
const ORIGIN_BUSY_RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(2);
/// The slowest a burrow makes a file's proofs, for how long it is waited
/// for (a minute, and the file at that rate).
const ORIGIN_PROOF_RATE: u64 = 64 * 1024 * 1024;

/// Whether a burrow's software sends proved ranges (0.230 on). One that
/// does not is never asked, so a download it could not serve opens and
/// counts nothing there.
fn serves_proved_ranges(version: &str) -> bool {
    let mut parts = version.split('.').map(|p| p.parse::<u64>().ok());
    match (parts.next().flatten(), parts.next().flatten()) {
        (Some(major), Some(minor)) => (major, minor) >= (0, 230),
        _ => false,
    }
}

/// The burrow as one of a swarm fetch's sources: it holds the whole file and
/// sends each range with its proof (`ProvedRange`), which the fetch checks
/// against the root like any peer's. Its asks go through the download's own
/// session, one at a time, answered by the loop that drives the download. It
/// takes units like any other source (work-stealing), so it carries what the
/// peers do not hold and whatever it is quicker to.
struct Origin {
    asks: tokio::sync::mpsc::Sender<OriginAsk>,
}

/// What the burrow said to one ask.
enum OriginAnswer {
    Range(rabbithole_proto::transfer::ProvedRange),
    /// It is making the file's proofs: ask again shortly.
    Busy,
    /// Not from it, for this download.
    No,
}

struct OriginAsk {
    offset: u64,
    len: u32,
    reply: tokio::sync::oneshot::Sender<OriginAnswer>,
}

#[async_trait::async_trait]
impl RangeSource for Origin {
    fn label(&self) -> String {
        ORIGIN_SOURCE.to_string()
    }

    async fn have(&self) -> Result<Option<HaveMap>, PeerError> {
        Ok(None)
    }

    async fn bao(&self, offset: u64, len: u64) -> Result<Vec<BaoPiece>, PeerError> {
        // Its messages are smaller than a unit: the range comes in parts.
        let step = rabbithole_proto::transfer::PROVED_RANGE_MAX as u64;
        let mut pieces = Vec::new();
        let mut at = offset;
        let refused = || PeerError::Refused(rabbithole_swarm::peer::STATUS_BAD_REQUEST);
        while at < offset + len {
            let part = (offset + len - at).min(step);
            // A burrow making the file's proofs (it started when first
            // asked) is asked again, a little later each time; meanwhile the
            // peers carry on. The loop that drives the download decides when
            // it has waited long enough, and says so with `No`.
            let mut pause = ORIGIN_BUSY_RETRY;
            let range = loop {
                let (reply, answer) = tokio::sync::oneshot::channel();
                let ask = OriginAsk {
                    offset: at,
                    len: part as u32,
                    reply,
                };
                self.asks.send(ask).await.map_err(|_| refused())?;
                match answer.await {
                    Ok(OriginAnswer::Range(range)) => break range,
                    Ok(OriginAnswer::Busy) => {
                        tokio::time::sleep(pause).await;
                        pause = (pause * 2).min(ORIGIN_BUSY_RETRY_MAX);
                    }
                    _ => return Err(refused()),
                }
            };
            pieces.push(BaoPiece {
                offset: at,
                len: part,
                size: range.size,
                stream: range.stream,
            });
            // The last part of the file is shorter than asked for.
            if at + part >= range.size {
                break;
            }
            at += part;
        }
        Ok(pieces)
    }
}

/// How a download shares what lands as it goes (the person opted in to
/// seeding): the store it serves from, this machine's own endpoint (never a
/// source for its own download), and the advert that goes out once the
/// first verified unit is in.
pub struct ShareAs {
    pub seeds: Arc<SeedStore>,
    pub own: [u8; 32],
    pub entry: AdvertEntry,
    pub ttl_secs: u32,
}

/// A download's lifecycle, surfaced to the caller and forwarded over Tauri IPC
/// to the ui-web Transfers manager as the swarm fills. The JSON shape (an
/// internally-tagged `kind` + snake_case fields) is the wire contract the wasm
/// SPA deserializes; it's locked by a host test (`serializes_to_the_wire_contract`).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SwarmEvent {
    /// Sources resolved; the fetch is starting.
    Opened { total_units: u64, source_count: usize },
    /// One verified unit landed from a source.
    Chunk {
        endpoint: String,
        offset: u64,
        done_units: u64,
        total_units: u64,
    },
    /// The fetch finished; `per_source` is the final (endpoint, units) split.
    Done {
        bytes: u64,
        per_source: Vec<(String, u64)>,
    },
    /// The fetch gave up, with the engine's own reason and how many sources
    /// were in play — the webview shows both, so a failed transfer can say
    /// what went wrong instead of only that it did.
    Failed { reason: String, sources_tried: usize },
}

/// Why a swarm download couldn't proceed.
#[derive(Debug)]
pub enum SwarmError {
    /// Talking to the origin server failed (find / ticket).
    Client(ClientError),
    /// The multi-source fetch failed after exhausting sources.
    Fetch(rabbithole_swarm::peer::PeerError),
    /// No peer advertises this content. `server_has` says whether the origin
    /// still holds it (so the caller can fall back to a single origin stream).
    NoPeerSources { server_has: bool },
}

impl std::fmt::Display for SwarmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SwarmError::Client(e) => write!(f, "server: {e}"),
            SwarmError::Fetch(e) => write!(f, "fetch: {e}"),
            // Said for a person: this is what the failed row shows.
            SwarmError::NoPeerSources { server_has: true } => write!(
                f,
                "nobody is sharing this file right now, and this download was set to peers only"
            ),
            SwarmError::NoPeerSources { server_has: false } => {
                write!(f, "nobody has this file right now, not even the burrow")
            }
        }
    }
}
impl std::error::Error for SwarmError {}

impl From<ClientError> for SwarmError {
    fn from(e: ClientError) -> Self {
        SwarmError::Client(e)
    }
}

/// Where the person asked a download to come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceMode {
    /// Peers when anyone has it, else the burrow itself. The default, and the
    /// only choice under which a download cannot fail merely because nobody
    /// happens to be seeding.
    #[default]
    Auto,
    /// Peers only: never pull from the burrow (it may be metered, or slow).
    PeersOnly,
    /// The burrow only: skip discovery and fetch straight from the origin.
    OriginOnly,
}

impl SourceMode {
    /// The webview's word for it. Anything unrecognised is the default.
    pub fn parse(word: &str) -> Self {
        match word {
            "peers" => SourceMode::PeersOnly,
            "origin" => SourceMode::OriginOnly,
            _ => SourceMode::Auto,
        }
    }
}

/// Which way a download goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Swarm,
    Origin,
}

/// Decide the route once discovery has answered. Pure, so the rule is tested
/// rather than hoped for: a download fails for want of sources only when the
/// person asked for peers and there are none, or when nobody at all has it.
pub fn choose_route(
    mode: SourceMode,
    peers: usize,
    server_has: bool,
    origin_reachable: bool,
) -> Result<Route, SwarmError> {
    let origin_ok = server_has && origin_reachable;
    match mode {
        SourceMode::OriginOnly if origin_ok => Ok(Route::Origin),
        SourceMode::OriginOnly => Err(SwarmError::NoPeerSources { server_has }),
        SourceMode::PeersOnly if peers > 0 => Ok(Route::Swarm),
        SourceMode::PeersOnly => Err(SwarmError::NoPeerSources { server_has }),
        SourceMode::Auto if peers > 0 => Ok(Route::Swarm),
        SourceMode::Auto if origin_ok => Ok(Route::Origin),
        SourceMode::Auto => Err(SwarmError::NoPeerSources { server_has }),
    }
}

/// How many whole swarm units `len` bytes of a `size`-byte file amount to,
/// counting the short final unit once the file is complete.
pub fn units_done(len: u64, size: u64) -> u64 {
    if size > 0 && len >= size {
        size.div_ceil(UNIT_SIZE)
    } else {
        len / UNIT_SIZE
    }
}

/// What the Transfers row calls the burrow when it is the only source.
pub const ORIGIN_SOURCE: &str = "the burrow";

/// Turn a server's [`SourceList`] into the fetchable peers: only entries that
/// registered BOTH a peer-wire endpoint and a cert fingerprint can be dialed;
/// coordinator-only entries (origin fallback) are dropped. Pure — unit-tested.
pub fn sources_from_list(list: &SourceList) -> Vec<SourcePeer> {
    sources_except(list, None)
}

/// [`sources_from_list`] without this machine's own endpoint (`own`, its
/// certificate fingerprint): a person sharing a download as it comes in is
/// never a source for it.
pub fn sources_except(list: &SourceList, own: Option<[u8; 32]>) -> Vec<SourcePeer> {
    list.sources
        .iter()
        .filter_map(|s| {
            Some(SourcePeer {
                endpoint: s.endpoint.clone()?,
                cert_fp: s.cert_fp?,
            })
        })
        .filter(|s| Some(s.cert_fp) != own)
        .collect()
}

/// The content's size from a source list: the origin's copy when it has one,
/// else the largest advertised size. `0` if genuinely unknown.
fn size_from_list(list: &SourceList) -> u64 {
    if list.server_has && list.server_size > 0 {
        return list.server_size;
    }
    list.sources.iter().map(|s| s.size).max().unwrap_or(0)
}

/// Discover who has `root` (on the connected server + its swarm peers), then
/// fetch it multi-source into `dest`, every 16 KiB Bao-block verified against
/// `root`. Returns the per-source unit report. If no peer advertises it, returns
/// [`SwarmError::NoPeerSources`] so the caller can fall back to an origin stream.
///
/// `size` is the caller's known size (from the file node's blob metadata); `0`
/// means "derive it from the source list".
///
/// With `origin` (the file's node on the burrow, when the burrow holds it),
/// the burrow is one of the sources too, unit by unit beside the peers:
/// whatever they do not hold comes from it, each unit proved like theirs.
#[allow(clippy::too_many_arguments)]
pub async fn run_swarm_download(
    client: &mut Client,
    root: [u8; 32],
    size: u64,
    dest: &Path,
    max_sources: usize,
    share: Option<ShareAs>,
    origin: Option<i64>,
    mut emit: impl FnMut(SwarmEvent),
) -> Result<FetchReport, SwarmError> {
    // Partial seeds too: this fetch asks each source which part it holds.
    let list = client.swarm_find_all(root).await?;
    let mut sources = sources_except(&list, share.as_ref().map(|s| s.own));
    // The engine runs one worker per source, so the source count IS the
    // parallelism. Capped by the user's setting rather than hardcoded: on a
    // metered or narrow link, eight simultaneous peers is a cost, not a gift.
    if max_sources > 0 && sources.len() > max_sources {
        sources.truncate(max_sources);
    }
    if sources.is_empty() {
        return Err(SwarmError::NoPeerSources {
            server_has: list.server_has,
        });
    }
    let size = if size > 0 { size } else { size_from_list(&list) };
    // A resolved size of 0 means we couldn't determine the file's length (the
    // origin doesn't hold it and every advert reports 0). Proceeding would let
    // the scheduler treat it as an empty transfer — and, worse, historically
    // wipe any partial — so fail loudly rather than destroy data or hang the UI.
    if size == 0 {
        return Err(SwarmError::NoPeerSources {
            server_has: list.server_has,
        });
    }
    let ticket = client.swarm_ticket(root).await?;
    let mut all: Vec<Arc<dyn RangeSource>> = sources
        .iter()
        .map(|p| {
            Arc::new(PeerSource {
                endpoint: p.endpoint.clone(),
                cert_fp: p.cert_fp,
                token: ticket.token.clone(),
                root,
            }) as Arc<dyn RangeSource>
        })
        .collect();
    // The burrow joins when it holds the file, sends proved ranges, and the
    // person has not asked for peers only.
    let (asks_tx, mut asks) = tokio::sync::mpsc::channel::<OriginAsk>(2);
    let origin = origin
        .filter(|_| list.server_has && serves_proved_ranges(&client.server.server_version));
    if origin.is_some() {
        all.push(Arc::new(Origin { asks: asks_tx }));
    } else {
        drop(asks_tx);
    }
    // How long a burrow making the file's proofs is waited for: a minute,
    // and the file read at the slowest rate a burrow makes them. Past that
    // it is let go, and the peers carry the download.
    let origin_busy_until = tokio::time::Instant::now()
        + std::time::Duration::from_secs(60 + size / ORIGIN_PROOF_RATE);
    // Opened when the burrow is first asked for a range, so a download it
    // never serves opens (and counts) nothing there, and closed as soon as
    // it is let go.
    let mut origin_ticket: Option<rabbithole_proto::transfer::TransferTicket> = None;
    let mut origin_gone = false;
    let total_units = size.div_ceil(UNIT_SIZE);
    emit(SwarmEvent::Opened {
        total_units,
        source_count: all.len(),
    });

    // Run the fetch on its own task and drain live progress. The channel closes
    // (recv -> None) when the fetch drops the last sender, i.e. when it finishes.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let dest_owned = dest.to_path_buf();
    // The advert for what lands goes out with the first verified unit, and
    // again before the burrow's grant lapses, for as long as units land.
    let advert = share.as_ref().map(|s| (s.entry.clone(), s.ttl_secs));
    let mut advertised: Option<(std::time::Instant, u64)> = None;
    let seeds = share.map(|s| s.seeds);
    // Opted in: every unit that lands is offered on at once, with its
    // proof, and the whole file when it is done.
    let fetch = tokio::spawn(async move {
        fetch_swarm_from(&all, root, size, &dest_owned, Some(tx), seeds).await
    });
    loop {
        let u = tokio::select! {
            // The fetch's end first: an ask still queued behind it has no
            // one waiting for it.
            biased;
            unit = rx.recv() => match unit {
                Some(u) => u,
                None => break,
            },
            Some(ask) = asks.recv() => {
                let mut reply = ask.reply;
                if reply.is_closed() {
                    continue;
                }
                if origin_ticket.is_none() && !origin_gone {
                    if let Some(node_id) = origin {
                        // Bounded like an ask: a burrow that has gone quiet
                        // must not hold up the peers' download.
                        origin_ticket = tokio::time::timeout(
                            ORIGIN_ASK_TIMEOUT,
                            client.download_ticket(node_id),
                        )
                        .await
                        .ok()
                        .and_then(Result::ok);
                    }
                    origin_gone = origin_ticket.is_none();
                }
                let answer = match &origin_ticket {
                    Some(ticket) => {
                        // Progress keeps flowing while the burrow answers,
                        // and a burrow that does not answer in time is let go.
                        let request = client.proved_range(ticket.transfer_id, ask.offset, ask.len);
                        tokio::pin!(request);
                        let deadline = tokio::time::sleep(ORIGIN_ASK_TIMEOUT);
                        tokio::pin!(deadline);
                        loop {
                            tokio::select! {
                                answer = &mut request => break Some(match answer {
                                    Ok(range) => OriginAnswer::Range(range),
                                    // Still making the file's proofs: asked
                                    // again, until it has had long enough.
                                    Err(ClientError::Refused(rabbithole_proto::ErrorCode::Unavailable))
                                        if tokio::time::Instant::now() < origin_busy_until =>
                                    {
                                        OriginAnswer::Busy
                                    }
                                    Err(_) => OriginAnswer::No,
                                }),
                                _ = &mut deadline => break Some(OriginAnswer::No),
                                // The fetch no longer wants it.
                                _ = reply.closed() => break None,
                                Some(u) = rx.recv() => emit(SwarmEvent::Chunk {
                                    endpoint: u.endpoint,
                                    offset: u.offset,
                                    done_units: u.done_units,
                                    total_units: u.total_units,
                                }),
                            }
                        }
                    }
                    None => Some(OriginAnswer::No),
                };
                // The fetch hears first, then the burrow is let go: its
                // ticket closes now, not when the peers finish.
                let let_go = matches!(answer, Some(OriginAnswer::No));
                if let Some(answer) = answer {
                    let _ = reply.send(answer);
                }
                if let_go {
                    origin_gone = true;
                    if let Some(ticket) = origin_ticket.take() {
                        let _ = tokio::time::timeout(
                            ORIGIN_ASK_TIMEOUT,
                            client.close_transfer(ticket.transfer_id),
                        )
                        .await;
                    }
                }
                continue;
            }
        };
        if let Some((entry, ttl)) = &advert {
            let due = advertised.is_none_or(|(at, after)| at.elapsed().as_secs() >= after);
            if due {
                // Found by peers that can use part of a file; a burrow that
                // predates that is not told until the file is whole.
                let granted = client
                    .swarm_advertise_partial(vec![entry.clone()], *ttl)
                    .await
                    .ok()
                    .flatten()
                    .map(|ack| ack.ttl_secs)
                    .unwrap_or(*ttl);
                advertised = Some((
                    std::time::Instant::now(),
                    crate::seeding::reannounce_after(granted),
                ));
            }
        }
        emit(SwarmEvent::Chunk {
            endpoint: u.endpoint,
            offset: u.offset,
            done_units: u.done_units,
            total_units: u.total_units,
        });
    }
    if let Some(ticket) = &origin_ticket {
        let _ = tokio::time::timeout(
            ORIGIN_ASK_TIMEOUT,
            client.close_transfer(ticket.transfer_id),
        )
        .await;
    }
    let report = fetch
        .await
        .map_err(|e| SwarmError::Fetch(rabbithole_swarm::peer::PeerError::Verify(e.to_string())))?
        .map_err(SwarmError::Fetch)?;
    emit(SwarmEvent::Done {
        bytes: report.bytes,
        per_source: report.per_source.clone(),
    });
    Ok(report)
}

/// One download, as asked for.
#[derive(Debug, Clone, Copy)]
pub struct Wanted {
    /// The content's blake3 root (its blob id).
    pub root: [u8; 32],
    /// Its size when known; 0 to derive it from the source list.
    pub size: u64,
    /// The file's node on the origin, which is what the origin is asked for.
    pub node_id: Option<i64>,
    /// How many peers a swarm fetch may use at once.
    pub max_sources: usize,
    pub mode: SourceMode,
}

/// Download `root` the way the person asked: from peers, from the burrow, or
/// (the default) from peers when there are any and the burrow when there are
/// not. Before this, a download in the app failed outright whenever nobody
/// happened to be seeding the file, which on most burrows is always.
///
/// `node_id` is the file's node on the origin; without it the origin cannot be
/// asked, and the download is peers-only whatever was chosen.
pub async fn run_download(
    client: &mut Client,
    want: &Wanted,
    dest: &Path,
    emit: impl FnMut(SwarmEvent),
) -> Result<Route, SwarmError> {
    run_download_sharing(client, want, dest, None, emit).await
}

/// [`run_download`], offering what lands to this burrow's swarm through
/// `share` as it goes (the person opted in to seeding), not only once the
/// file is whole.
pub async fn run_download_sharing(
    client: &mut Client,
    want: &Wanted,
    dest: &Path,
    share: Option<ShareAs>,
    mut emit: impl FnMut(SwarmEvent),
) -> Result<Route, SwarmError> {
    let Wanted {
        root,
        size,
        node_id,
        max_sources,
        mode,
    } = *want;
    let (peers, server_has) = if mode == SourceMode::OriginOnly {
        (0, true) // the origin is asked directly; it answers for itself
    } else {
        let list = client.swarm_find_all(root).await?;
        let own = share.as_ref().map(|s| s.own);
        (sources_except(&list, own).len(), list.server_has)
    };
    let seeds = share.as_ref().map(|s| s.seeds.clone());
    match choose_route(mode, peers, server_has, node_id.is_some())? {
        Route::Swarm => {
            // Unless the person asked for peers only, the burrow is a source
            // alongside them.
            let origin = (mode == SourceMode::Auto && server_has)
                .then_some(node_id)
                .flatten();
            match run_swarm_download(
                client,
                root,
                size,
                dest,
                max_sources,
                share,
                origin,
                &mut emit,
            )
            .await
            {
                Ok(_) => Ok(Route::Swarm),
                // The peers could not give the whole file (gone, or holding
                // only parts of it): by default the burrow sends it instead.
                // Not when the trouble is on this machine (a full disk): the
                // verified progress stays for a retry.
                Err(e)
                    if !is_local(&e)
                        && mode == SourceMode::Auto
                        && server_has
                        && node_id.is_some() =>
                {
                    let node_id = node_id.expect("checked");
                    // What was offered in part is no longer here to serve.
                    if seeds.as_ref().is_some_and(|s| !s.holds_whole(&root)) {
                        let _ = client.swarm_withdraw(vec![root]).await;
                    }
                    run_origin_download(client, node_id, size, dest, &mut emit).await?;
                    check_origin_copy(dest, root)?;
                    Ok(Route::Origin)
                }
                Err(e) => Err(e),
            }
        }
        Route::Origin => {
            let node_id = node_id.expect("choose_route requires a reachable origin");
            run_origin_download(client, node_id, size, dest, &mut emit).await?;
            check_origin_copy(dest, root)?;
            Ok(Route::Origin)
        }
    }
}

/// Whether a download failed on this machine (writing the file), not at the
/// sources.
fn is_local(e: &SwarmError) -> bool {
    matches!(e, SwarmError::Fetch(rabbithole_swarm::peer::PeerError::Io(_)))
}

/// The origin verified the file against *its* ticket. Check it against what
/// was asked for: a node id is only a number, and the content hash is the
/// identity.
fn check_origin_copy(dest: &Path, root: [u8; 32]) -> Result<(), SwarmError> {
    let got = Client::hash_file(dest).map(|(root, _)| root).ok();
    if got != Some(root) {
        let _ = std::fs::remove_file(dest);
        return Err(SwarmError::Fetch(rabbithole_swarm::peer::PeerError::Verify(
            "the burrow sent a different file than the one asked for".to_string(),
        )));
    }
    Ok(())
}

/// Fetch a file straight from the burrow (the ticketed, resumable, blake3-
/// verified transfer every client uses), reporting progress in the same
/// events a swarm fetch does so the Transfers row cannot tell the difference.
async fn run_origin_download(
    client: &mut Client,
    node_id: i64,
    size: u64,
    dest: &Path,
    emit: &mut impl FnMut(SwarmEvent),
) -> Result<u64, SwarmError> {
    // A swarm attempt leaves units at arbitrary offsets plus a `.rhstate`. The
    // origin transfer resumes by *length*, which would mistake such a file
    // for a contiguous partial and fail its final hash check. Start clean.
    // A swarm attempt that landed nothing leaves no `.rhstate`, but it did
    // pre-size the file (zeros) and start its proofs file: either sidecar
    // marks the file as the swarm's, not a contiguous partial of the burrow's.
    let state = rabbithole_swarm::scheduler::rhstate_path(dest);
    let proofs = rabbithole_swarm::proofs_path(dest);
    if state.exists() || proofs.exists() {
        let _ = std::fs::remove_file(&state);
        let _ = std::fs::remove_file(&proofs);
        let _ = std::fs::remove_file(dest);
    }
    let total_units = size.div_ceil(UNIT_SIZE).max(1);
    emit(SwarmEvent::Opened {
        total_units,
        source_count: 1,
    });
    let mut reported = 0u64;
    let mut report_up_to = |units: u64, emit: &mut dyn FnMut(SwarmEvent)| {
        while reported < units.min(total_units) {
            emit(SwarmEvent::Chunk {
                endpoint: ORIGIN_SOURCE.to_string(),
                offset: reported * UNIT_SIZE,
                done_units: reported + 1,
                total_units,
            });
            reported += 1;
        }
    };
    let bytes = {
        let transfer = client.transfer_download(node_id, dest);
        tokio::pin!(transfer);
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(200));
        loop {
            tokio::select! {
                done = &mut transfer => break done?,
                _ = tick.tick() => {
                    let len = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
                    // Hold the last unit back: it is only "done" once verified.
                    report_up_to(units_done(len, 0).min(total_units.saturating_sub(1)), emit);
                }
            }
        }
    };
    report_up_to(total_units, emit);
    emit(SwarmEvent::Done {
        bytes,
        per_source: vec![(ORIGIN_SOURCE.to_string(), total_units)],
    });
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabbithole_proto::swarm::{SourceInfo, SourceList};

    #[test]
    fn only_a_burrow_that_sends_proved_ranges_is_asked_for_them() {
        assert!(serves_proved_ranges("0.230.0"));
        assert!(serves_proved_ranges("0.231.4-dev"));
        assert!(serves_proved_ranges("1.0.0"));
        assert!(!serves_proved_ranges("0.229.0"));
        assert!(!serves_proved_ranges("0.99.9"));
        assert!(!serves_proved_ranges(""));
        assert!(!serves_proved_ranges("custom"));
    }

    #[test]
    fn a_download_fails_for_want_of_sources_only_when_it_must() {
        use Route::*;
        use SourceMode::*;
        let route = |m, peers, has, reach| choose_route(m, peers, has, reach).ok();
        // The default: peers when there are any, else the burrow.
        assert_eq!(route(Auto, 3, true, true), Some(Swarm));
        assert_eq!(route(Auto, 0, true, true), Some(Origin), "nobody seeding is not a failure");
        assert_eq!(route(Auto, 0, false, true), None, "nobody has it at all");
        assert_eq!(route(Auto, 0, true, false), None, "no node id: the origin cannot be asked");
        // Peers only means it.
        assert_eq!(route(PeersOnly, 2, true, true), Some(Swarm));
        assert_eq!(route(PeersOnly, 0, true, true), None);
        // The burrow only skips the peers even when there are some.
        assert_eq!(route(OriginOnly, 5, true, true), Some(Origin));
        assert_eq!(route(OriginOnly, 5, false, true), None);
        // And the failure says something a person can act on.
        let why = choose_route(PeersOnly, 0, true, true).unwrap_err().to_string();
        assert!(why.contains("peers only"), "{why}");
        assert_eq!(SourceMode::parse("peers"), PeersOnly);
        assert_eq!(SourceMode::parse("origin"), OriginOnly);
        assert_eq!(SourceMode::parse("auto"), Auto);
        assert_eq!(SourceMode::parse("nonsense"), Auto);
    }

    #[test]
    fn progress_counts_whole_units_and_the_short_last_one_only_when_complete() {
        assert_eq!(units_done(0, 3 * UNIT_SIZE), 0);
        assert_eq!(units_done(UNIT_SIZE - 1, 3 * UNIT_SIZE), 0);
        assert_eq!(units_done(UNIT_SIZE, 3 * UNIT_SIZE), 1);
        let size = 2 * UNIT_SIZE + 10;
        assert_eq!(units_done(2 * UNIT_SIZE + 5, size), 2, "the tail is not done yet");
        assert_eq!(units_done(size, size), 3, "complete: the short unit counts");
        assert_eq!(units_done(700, 700), 1, "a small file is one unit");
    }

    /// A peer-wire source: `SourceInfo::new` sets no contact; `with_endpoint`
    /// sets both endpoint + fingerprint together (the server only ever
    /// registers both, via `PeerContact`, or neither = coordinator-only).
    fn peer(name: &str, endpoint: &str, cert_fp: [u8; 32], size: u64) -> SourceInfo {
        SourceInfo::new(name, size, "f", "application/octet-stream").with_endpoint(endpoint, cert_fp)
    }
    fn coordinator_only(name: &str, size: u64) -> SourceInfo {
        SourceInfo::new(name, size, "f", "application/octet-stream")
    }

    #[test]
    fn only_dialable_peers_become_sources() {
        let list = SourceList::new(
            [7; 32],
            true,
            4096,
            vec![
                peer("alice", "127.0.0.1:5000", [1; 32], 4096),
                // No peer-wire contact registered -> origin/coordinator only -> dropped.
                coordinator_only("bob", 4096),
            ],
        );
        let sources = sources_from_list(&list);
        assert_eq!(sources.len(), 1, "only the dialable peer");
        assert_eq!(sources[0].endpoint, "127.0.0.1:5000");
        assert_eq!(sources[0].cert_fp, [1; 32]);
        // Size prefers the origin's copy.
        assert_eq!(size_from_list(&list), 4096);
    }

    #[test]
    fn no_dialable_peers_yields_empty_sources() {
        let list = SourceList::new([7; 32], true, 4096, vec![coordinator_only("bob", 4096)]);
        assert!(sources_from_list(&list).is_empty(), "coordinator-only -> origin fallback");
    }

    #[test]
    fn serializes_to_the_wire_contract() {
        // The exact JSON the ui-web SPA deserializes. If this changes, the
        // wasm-side `SwarmEvent` mirror + `swarm_event_to_file_events` must too.
        let chunk = SwarmEvent::Chunk {
            endpoint: "127.0.0.1:9000".into(),
            offset: 1048576,
            done_units: 2,
            total_units: 4,
        };
        let v: serde_json::Value = serde_json::to_value(&chunk).unwrap();
        assert_eq!(v["kind"], "chunk");
        assert_eq!(v["endpoint"], "127.0.0.1:9000");
        assert_eq!(v["offset"], 1048576);
        assert_eq!(v["done_units"], 2);
        assert_eq!(v["total_units"], 4);

        let opened = SwarmEvent::Opened {
            total_units: 4,
            source_count: 3,
        };
        assert_eq!(serde_json::to_value(&opened).unwrap()["kind"], "opened");

        let done = SwarmEvent::Done {
            bytes: 4_000_000,
            per_source: vec![("127.0.0.1:9000".into(), 3), ("127.0.0.1:9001".into(), 1)],
        };
        let dv = serde_json::to_value(&done).unwrap();
        assert_eq!(dv["kind"], "done");
        assert_eq!(dv["bytes"], 4_000_000);
        assert_eq!(dv["per_source"][0][1], 3);
    }

    #[test]
    fn size_falls_back_to_largest_advert_without_origin() {
        let list = SourceList::new(
            [7; 32],
            false,
            0,
            vec![
                peer("a", "127.0.0.1:1", [1; 32], 100),
                peer("b", "127.0.0.1:2", [2; 32], 900),
            ],
        );
        assert_eq!(size_from_list(&list), 900);
    }
}
