//! Slice 2 of the native swarm backend: source discovery + the multi-source
//! download orchestration, Tauri-free so it's unit-testable. The later Tauri
//! command surface (Slice 4) just calls [`run_swarm_download`] and forwards a
//! progress `emit`; the ui-web `TransferBackend` (Slice 5) drives it over IPC.

#![cfg_attr(rustfmt, rustfmt_skip)]

use std::path::Path;

use rabbithole_core::{Client, ClientError};
use rabbithole_proto::swarm::SourceList;
use rabbithole_swarm::{fetch_swarm_resumable_with_progress, FetchReport, SourcePeer, UNIT_SIZE};

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
    list.sources
        .iter()
        .filter_map(|s| {
            Some(SourcePeer {
                endpoint: s.endpoint.clone()?,
                cert_fp: s.cert_fp?,
            })
        })
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
pub async fn run_swarm_download(
    client: &mut Client,
    root: [u8; 32],
    size: u64,
    dest: &Path,
    max_sources: usize,
    mut emit: impl FnMut(SwarmEvent),
) -> Result<FetchReport, SwarmError> {
    let list = client.swarm_find(root).await?;
    let mut sources = sources_from_list(&list);
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
    let total_units = size.div_ceil(UNIT_SIZE);
    emit(SwarmEvent::Opened {
        total_units,
        source_count: sources.len(),
    });

    // Run the fetch on its own task and drain live progress. The channel closes
    // (recv -> None) when the fetch drops the last sender, i.e. when it finishes.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let (sources, token, dest_owned) = (sources.clone(), ticket.token.clone(), dest.to_path_buf());
    let fetch = tokio::spawn(async move {
        fetch_swarm_resumable_with_progress(&sources, &token, root, size, &dest_owned, tx).await
    });
    while let Some(u) = rx.recv().await {
        emit(SwarmEvent::Chunk {
            endpoint: u.endpoint,
            offset: u.offset,
            done_units: u.done_units,
            total_units: u.total_units,
        });
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
        let list = client.swarm_find(root).await?;
        (sources_from_list(&list).len(), list.server_has)
    };
    match choose_route(mode, peers, server_has, node_id.is_some())? {
        Route::Swarm => {
            run_swarm_download(client, root, size, dest, max_sources, emit).await?;
            Ok(Route::Swarm)
        }
        Route::Origin => {
            let node_id = node_id.expect("choose_route requires a reachable origin");
            run_origin_download(client, node_id, size, dest, &mut emit).await?;
            // The origin verified the file against *its* ticket. Check it
            // against what was asked for: a node id is only a number, and the
            // content hash is the identity.
            let got = Client::hash_file(dest).map(|(root, _)| root).ok();
            if got != Some(root) {
                let _ = std::fs::remove_file(dest);
                return Err(SwarmError::Fetch(rabbithole_swarm::peer::PeerError::Verify(
                    "the burrow sent a different file than the one asked for".to_string(),
                )));
            }
            Ok(Route::Origin)
        }
    }
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
    let state = rabbithole_swarm::scheduler::rhstate_path(dest);
    if state.exists() {
        let _ = std::fs::remove_file(&state);
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
