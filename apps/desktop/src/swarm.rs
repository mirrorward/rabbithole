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
use rabbithole_swarm::{
    fetch_swarm_from, FetchReport, PeerSource, RangeSource, SeedStore,
    SourcePeer, UNIT_SIZE,
};

/// A burrow as a source of a download's units, and the rules for which of
/// the person's other burrows may be asked: shared with the command line,
/// so both ask the same way (see [`rabbithole_swarm::burrow`]).
pub use rabbithole_swarm::burrow::{
    confirm_helpers, other_burrows as other_burrows_rule, serves_proved_ranges, AskOthers,
    BurrowLink, BurrowSource, Session, ASK_TIMEOUT, OTHER_BURROWS_MAX, SESSION_WAIT,
};

/// Which of the app's other sessions a download may ask, by their place in
/// `sessions`: the shared rule, told whether the person left the choice of
/// sources to the app.
pub fn other_burrows(
    mode: SourceMode,
    sessions: &[(String, [u8; 32], String)],
    origin_endpoint: &str,
    origin_key: [u8; 32],
    max: usize,
) -> Vec<usize> {
    let ask = if mode == SourceMode::Auto {
        AskOthers::Yes
    } else {
        AskOthers::No
    };
    other_burrows_rule(ask, sessions, origin_endpoint, origin_key, max)
}

/// A spawned fetch that stops when this is dropped. A `JoinHandle` on its
/// own does not: dropping it lets the task run on, which for a download the
/// person stopped means it carries on writing the file it was told to stop
/// writing.
struct FetchTask(
    tokio::task::JoinHandle<Result<FetchReport, rabbithole_swarm::peer::PeerError>>,
);

impl Drop for FetchTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Gives back every ticket these burrows hold when it is dropped, however
/// that happens — including a download dropped where it stands, which is
/// what stopping one does. A burrow must not hold a transfer slot for a
/// download that is over.
struct TicketsBack(Vec<Arc<BurrowSource>>);

impl Drop for TicketsBack {
    fn drop(&mut self) {
        let burrows: Vec<Arc<BurrowSource>> =
            std::mem::take(&mut self.0).into_iter().collect();
        if burrows.is_empty() {
            return;
        }
        // Closing takes a session and a round trip, which a `Drop` cannot
        // wait for: it goes on in its own task, bounded by its own
        // timeouts. Outside a runtime (shutdown) there is nothing to do —
        // the session is going with it.
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                for burrow in burrows {
                    burrow.close().await;
                }
            });
        }
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
    /// what went wrong instead of only that it did. `retryable` is false
    /// when trying again cannot help (nobody has the file at all), so the
    /// row does not offer a Retry that is certain to fail.
    Failed {
        reason: String,
        sources_tried: usize,
        retryable: bool,
    },
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
    /// The person stopped it.
    Cancelled,
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
            SwarmError::Cancelled => write!(f, "Stopped."),
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
/// `helpers` are other burrows that have already said they hold the content
/// (see [`confirm_helpers`]); they take units the same way, and what they
/// send is never offered back to this burrow's swarm.
#[allow(clippy::too_many_arguments)]
pub async fn run_swarm_download(
    origin_session: &Session,
    root: [u8; 32],
    size: u64,
    dest: &Path,
    max_sources: usize,
    share: Option<ShareAs>,
    origin: Option<i64>,
    helpers: Vec<Arc<BurrowSource>>,
    // Where the burrow's own ticket is left when the fetch fails, so a
    // fallback to its stream is the same download rather than another.
    kept: &mut Option<rabbithole_proto::transfer::TransferTicket>,
    mut emit: impl FnMut(SwarmEvent),
) -> Result<FetchReport, SwarmError> {
    let outcome = swarm_download(
        origin_session,
        root,
        size,
        dest,
        max_sources,
        share,
        origin,
        &helpers,
        kept,
        &mut emit,
    )
    .await;
    // However this ended, every burrow that was asked gives its ticket
    // back: none holds a transfer slot for a download that is over.
    for helper in &helpers {
        helper.close().await;
    }
    outcome
}

#[allow(clippy::too_many_arguments)]
async fn swarm_download(
    origin_session: &Session,
    root: [u8; 32],
    size: u64,
    dest: &Path,
    max_sources: usize,
    share: Option<ShareAs>,
    origin: Option<i64>,
    helpers: &[Arc<BurrowSource>],
    kept: &mut Option<rabbithole_proto::transfer::TransferTicket>,
    emit: &mut impl FnMut(SwarmEvent),
) -> Result<FetchReport, SwarmError> {
    // Partial seeds too: this fetch asks each source which part it holds.
    let (list, ticket, serves_ranges) = {
        let mut client = origin_session.lock().await;
        let list = client.swarm_find_all(root).await?;
        let ticket = client.swarm_ticket(root).await?;
        (
            list,
            ticket,
            serves_proved_ranges(&client.server.server_version),
        )
    };
    let mut sources = sources_except(&list, share.as_ref().map(|s| s.own));
    // The engine runs one worker per source, so the source count IS the
    // parallelism. Capped by the user's setting rather than hardcoded: on a
    // metered or narrow link, eight simultaneous peers is a cost, not a gift.
    if max_sources > 0 && sources.len() > max_sources {
        sources.truncate(max_sources);
    }
    if sources.is_empty() && helpers.is_empty() {
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
    let mut burrows: Vec<Arc<BurrowSource>> = Vec::new();
    if let Some(node_id) = origin.filter(|_| list.server_has && serves_ranges) {
        // The burrow the download is from is "the burrow" in the Transfers
        // row, as it has always been; another burrow is named, so the two
        // are told apart.
        burrows.push(Arc::new(BurrowSource::origin(
            ORIGIN_SOURCE.to_string(),
            origin_session.clone(),
            node_id,
            size,
        )));
    }
    burrows.extend(helpers.iter().cloned());
    all.extend(burrows.iter().map(|b| b.clone() as Arc<dyn RangeSource>));
    // From here on every ticket goes back, whether this returns, fails, or
    // is dropped where it stands because the person stopped the download.
    let mut hold = TicketsBack(burrows.clone());
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
    // Watched from here so the advert waits for a unit that is actually
    // this burrow's to offer: what another burrow lent is kept but never
    // marked, and advertising a file none of which can be served would only
    // send peers away empty.
    let offered = share.as_ref().map(|s| s.seeds.clone());
    let seeds = share.map(|s| s.seeds);
    // Opted in: every unit that lands is offered on at once, with its
    // proof, and the whole file when it is done — unless another burrow
    // carried part of it, which is not this one's to be given.
    let mut fetch = FetchTask(tokio::spawn(async move {
        fetch_swarm_from(&all, root, size, &dest_owned, Some(tx), seeds).await
    }));
    while let Some(u) = rx.recv().await {
        if let Some((entry, ttl)) = &advert {
            let due = advertised.is_none_or(|(at, after)| at.elapsed().as_secs() >= after);
            let any_held = offered
                .as_ref()
                .and_then(|seeds| seeds.have(&root))
                .is_some_and(|held| held.bits.iter().any(|byte| *byte != 0));
            if due && any_held {
                // Found by peers that can use part of a file; a burrow that
                // predates that is not told until the file is whole. The
                // session is shared with this download's own asks, so a
                // turn that does not come is simply skipped.
                // A turn that does not come, or a burrow that says nothing,
                // is tried again with the next unit rather than taken for an
                // answer: one missed moment must not end the sharing.
                let told = match tokio::time::timeout(SESSION_WAIT, origin_session.lock()).await {
                    Ok(mut client) => tokio::time::timeout(
                        ASK_TIMEOUT,
                        client.swarm_advertise_partial(vec![entry.clone()], *ttl),
                    )
                    .await
                    .ok()
                    .and_then(Result::ok),
                    Err(_) => None,
                };
                if let Some(granted) = told {
                    advertised = Some((
                        std::time::Instant::now(),
                        // A burrow that predates partial adverts answers
                        // nothing and is not asked again until the file is
                        // whole.
                        crate::seeding::reannounce_after(
                            granted.map(|ack| ack.ttl_secs).unwrap_or(*ttl),
                        ),
                    ));
                }
            }
        }
        emit(SwarmEvent::Chunk {
            endpoint: u.endpoint,
            offset: u.offset,
            done_units: u.done_units,
            total_units: u.total_units,
        });
    }
    let report = (&mut fetch.0)
        .await
        .map_err(|e| SwarmError::Fetch(rabbithole_swarm::peer::PeerError::Verify(e.to_string())))?
        .map_err(SwarmError::Fetch);
    // The burrow of the download gives its ticket back here — unless the
    // fetch failed and it is the one the file may yet come from, in which
    // case the ticket is handed on rather than opened again.
    for burrow in &burrows {
        if report.is_err() && !burrow.lent() {
            *kept = burrow.take_ticket().await;
        }
        burrow.close().await;
    }
    hold.0.clear();
    let report = report?;
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

/// How a download went: which way it came, and whether what landed may be
/// offered to the burrow's swarm. It may not when another burrow carried
/// part of it: that content was lent to this person, not given to this
/// burrow to hand on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Downloaded {
    pub route: Route,
    pub may_share: bool,
}

/// Download `root` the way the person asked: from peers, from the burrow, or
/// (the default) from peers when there are any and the burrow when there are
/// not. Before this, a download in the app failed outright whenever nobody
/// happened to be seeding the file, which on most burrows is always.
///
/// `node_id` is the file's node on the origin; without it the origin cannot be
/// asked, and the download is peers-only whatever was chosen.
pub async fn run_download(
    session: &Session,
    want: &Wanted,
    dest: &Path,
    emit: impl FnMut(SwarmEvent),
) -> Result<Downloaded, SwarmError> {
    run_download_sharing(session, want, dest, None, &[], emit).await
}

/// [`run_download`], offering what lands to this burrow's swarm through
/// `share` as it goes (the person opted in to seeding), not only once the
/// file is whole, and taking chunks from `others` — the app's other burrows
/// — when they hold the same content and the person left the choice to the
/// app.
pub async fn run_download_sharing(
    session: &Session,
    want: &Wanted,
    dest: &Path,
    share: Option<ShareAs>,
    others: &[BurrowLink],
    mut emit: impl FnMut(SwarmEvent),
) -> Result<Downloaded, SwarmError> {
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
        let list = session.lock().await.swarm_find_all(root).await?;
        let own = share.as_ref().map(|s| s.own);
        (sources_except(&list, own).len(), list.server_has)
    };
    // Asked before the route is chosen, so a download that only the other
    // burrows can carry still takes the swarm route, and the count the UI
    // is told is of sources that have actually answered.
    // Only when the person left the choice of sources to the app: a
    // download told where to come from asks no other burrow, and tells none
    // what is being looked for.
    let helpers = if mode == SourceMode::Auto {
        confirm_helpers(others, root).await
    } else {
        Vec::new()
    };
    // Their tickets go back however this download ends, including one
    // dropped where it stands because the person stopped it.
    let _helpers_back = TicketsBack(helpers.clone());
    let seeds = share.as_ref().map(|s| s.seeds.clone());
    let route = choose_route(mode, peers + helpers.len(), server_has, node_id.is_some());
    let route = match route {
        Ok(route) => route,
        Err(e) => {
            for helper in &helpers {
                helper.close().await;
            }
            return Err(e);
        }
    };
    match route {
        Route::Swarm => {
            // Unless the person asked for peers only, the burrow is a source
            // alongside them.
            let origin = (mode == SourceMode::Auto && server_has)
                .then_some(node_id)
                .flatten();
            let mut kept: Option<rabbithole_proto::transfer::TransferTicket> = None;
            let attempt = run_swarm_download(
                session,
                root,
                size,
                dest,
                max_sources,
                share,
                origin,
                helpers,
                &mut kept,
                &mut emit,
            )
            .await;
            match attempt
            {
                Ok(report) => Ok(Downloaded {
                    route: Route::Swarm,
                    may_share: !report.borrowed,
                }),
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
                    let mut client = session.lock().await;
                    // What was offered in part is no longer here to serve.
                    if seeds.as_ref().is_some_and(|s| !s.holds_whole(&root)) {
                        let _ = client.swarm_withdraw(vec![root]).await;
                    }
                    // On the ticket the swarm attempt already opened, when
                    // it got that far: one file is one download.
                    run_origin_download(&mut client, node_id, kept.take(), size, dest, &mut emit)
                        .await?;
                    check_origin_copy(dest, root)?;
                    Ok(Downloaded {
                        route: Route::Origin,
                        may_share: true,
                    })
                }
                Err(e) => {
                    // Not falling back: a ticket the attempt left behind is
                    // given back rather than held to the session's end.
                    if let Some(ticket) = kept.take() {
                        if let Ok(mut client) =
                            tokio::time::timeout(SESSION_WAIT, session.lock()).await
                        {
                            let _ = tokio::time::timeout(
                                ASK_TIMEOUT,
                                client.close_transfer(ticket.transfer_id),
                            )
                            .await;
                        }
                    }
                    Err(e)
                }
            }
        }
        Route::Origin => {
            for helper in &helpers {
                helper.close().await;
            }
            let node_id = node_id.expect("choose_route requires a reachable origin");
            let mut client = session.lock().await;
            run_origin_download(&mut client, node_id, None, size, dest, &mut emit).await?;
            check_origin_copy(dest, root)?;
            Ok(Downloaded {
                route: Route::Origin,
                may_share: true,
            })
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
    // A ticket the swarm attempt already opened for this very file, when
    // there is one: the burrow counts and charges one download, not two.
    open: Option<rabbithole_proto::transfer::TransferTicket>,
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
        let transfer = async {
            match &open {
                Some(ticket) => client.transfer_download_with(ticket, dest).await,
                None => client.transfer_download(node_id, dest).await,
            }
        };
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

        // A failure says whether trying again could work, so the row does
        // not offer a Retry that cannot help.
        let failed = SwarmEvent::Failed {
            reason: "nobody has this file right now, not even the burrow".into(),
            sources_tried: 0,
            retryable: false,
        };
        let fv = serde_json::to_value(&failed).unwrap();
        assert_eq!(fv["kind"], "failed");
        assert_eq!(fv["sources_tried"], 0);
        assert_eq!(fv["retryable"], false);
    }

    #[test]
    fn a_download_asks_the_other_burrows_that_can_answer_and_no_others() {
        let key = |n: u8| [n; 32];
        let here = key(1);
        let sessions = vec![
            ("ws://home".to_string(), here, "0.232.0".to_string()),
            ("ws://other".to_string(), key(2), "0.232.0".to_string()),
            // The same burrow, reached another way: one burrow, not two.
            ("quic://other".to_string(), key(2), "0.232.0".to_string()),
            ("ws://old".to_string(), key(3), "0.231.0".to_string()),
            ("ws://new".to_string(), key(4), "1.0.0".to_string()),
            ("ws://newer".to_string(), key(5), "0.240.1".to_string()),
        ];
        let pick = |mode, max| other_burrows(mode, &sessions, "ws://home", here, max);
        // Not the burrow it is downloading from, not the same burrow twice,
        // not one too old to be asked.
        assert_eq!(pick(SourceMode::Auto, 8), vec![1, 4, 5]);
        // Never more than it was told to.
        assert_eq!(pick(SourceMode::Auto, 2), vec![1, 4]);
        // The person chose where it comes from: no other burrow is asked.
        assert!(pick(SourceMode::PeersOnly, 8).is_empty());
        assert!(pick(SourceMode::OriginOnly, 8).is_empty());
        // The burrow of the download, found by its key rather than the
        // endpoint the app happens to have dialled.
        let by_key = other_burrows(SourceMode::Auto, &sessions, "ws://nowhere", key(2), 8);
        assert_eq!(by_key, vec![0, 4, 5]);
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
