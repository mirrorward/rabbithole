//! Slice 2 of the native swarm backend: source discovery + the multi-source
//! download orchestration, Tauri-free so it's unit-testable. The later Tauri
//! command surface (Slice 4) just calls [`run_swarm_download`] and forwards a
//! progress `emit`; the ui-web `TransferBackend` (Slice 5) drives it over IPC.

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
            SwarmError::NoPeerSources { server_has } => {
                write!(f, "no peer sources (server_has={server_has})")
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
    mut emit: impl FnMut(SwarmEvent),
) -> Result<FetchReport, SwarmError> {
    let list = client.swarm_find(root).await?;
    let sources = sources_from_list(&list);
    if sources.is_empty() {
        return Err(SwarmError::NoPeerSources {
            server_has: list.server_has,
        });
    }
    let size = if size > 0 { size } else { size_from_list(&list) };
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

#[cfg(test)]
mod tests {
    use super::*;
    use rabbithole_proto::swarm::{SourceInfo, SourceList};

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
