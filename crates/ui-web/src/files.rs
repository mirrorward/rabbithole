//! Pure, DOM-free file-library state and its event reducer.
//!
//! Like [`crate::state`], this module holds **no** Leptos or `web_sys` types so
//! the reducer and its helpers are unit-tested on the host with `cargo test`.
//! The [`FilesView`] component in [`crate::components`] owns a reactive
//! `RwSignal<FilesState>` and folds [`FileEvent`]s into it via
//! [`FilesState::apply`].
//!
//! File area/node metadata is reused straight from
//! [`rabbithole_proto::filelib`] rather than re-modelled here, so the wire
//! types and the view stay in lockstep. The one thing that is view-local is the
//! [`Transfer`] queue: a projection of the transfer-family event stream into a
//! queued / active / done / failed list with progress.

use rabbithole_proto::filelib::{FileAreaView, FileNodeView};

use crate::wire::FileEvent;

/// [`FileNodeView::kind`] value for a folder.
pub const KIND_FOLDER: u8 = 0;
/// [`FileNodeView::kind`] value for a file.
pub const KIND_FILE: u8 = 1;
/// [`FileNodeView::kind`] value for an alias.
pub const KIND_ALIAS: u8 = 2;

/// Direction of a queued [`Transfer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDir {
    /// Server → client.
    Download,
    /// Client → server.
    Upload,
}

/// Lifecycle of a queued [`Transfer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransferStatus {
    /// Authorised but no bytes moved yet.
    Queued,
    /// Bytes in flight.
    Active,
    /// Finished successfully.
    Done,
    /// Aborted or errored. Carries **why**: the reason arrives with the
    /// failure event and used to be dropped on the floor, leaving a red row
    /// that couldn't tell you anything.
    Failed,
}

/// One entry in the transfer queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    /// Transfer id (from the ticket, or the node id for an inline download).
    pub id: u64,
    /// Display name.
    pub name: String,
    /// Direction.
    pub dir: TransferDir,
    /// Total size in bytes (`0` if unknown).
    pub total: u64,
    /// Bytes moved so far.
    pub done: u64,
    /// Current status.
    pub status: TransferStatus,
    /// The content's blake3 hash (hex), when known — the content-addressed id
    /// the swarm keys on. `None` for the ticketed path until the node is known.
    pub hash: Option<String>,
    /// Why this transfer failed, when it did. Set from the failure event's
    /// own detail — the transport always knew; the UI just wasn't keeping it.
    pub error: Option<String>,
    /// How many sources the last attempt pulled from, when the swarm reported
    /// it. `None` for the inline (single-source) path.
    pub sources: Option<u32>,
    /// The file node this transfer came from, so a failed transfer can be
    /// retried without hunting for it again.
    pub node_id: Option<i64>,
    /// Whether retrying could plausibly work. A file nobody holds won't appear
    /// because you clicked Retry; a peer that timed out might. Offering Retry
    /// either way trains people to ignore the button.
    pub retryable: bool,
}

/// Format a 32-byte blob id as lowercase hex.
fn blob_hex(id: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in id {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

impl Transfer {
    /// Fractional progress in `0.0..=1.0`. A zero-byte transfer reports `1.0`
    /// once done and `0.0` otherwise.
    pub fn progress(&self) -> f32 {
        if self.total == 0 {
            return match self.status {
                TransferStatus::Done => 1.0,
                _ => 0.0,
            };
        }
        (self.done as f32 / self.total as f32).clamp(0.0, 1.0)
    }

    /// Progress as an integer percentage `0..=100`.
    pub fn percent(&self) -> u8 {
        (self.progress() * 100.0).round() as u8
    }
}

/// The full, flat file-library UI model. `Default` is the empty state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FilesState {
    /// Every file area.
    pub areas: Vec<FileAreaView>,
    /// Slug of the area currently open, if any.
    pub current_area: Option<String>,
    /// Breadcrumb path segments into the current area (empty = root).
    pub path: Vec<String>,
    /// Child nodes of the current folder.
    pub nodes: Vec<FileNodeView>,
    /// Id of the node whose metadata card is shown, if any.
    pub selected: Option<i64>,
    /// The transfer queue.
    pub transfers: Vec<Transfer>,
    /// One-line status/error line for the panel.
    pub status: String,
}

impl FilesState {
    /// Fold a single [`FileEvent`] into the state. Unknown
    /// (`#[non_exhaustive]`) events are ignored.
    pub fn apply(&mut self, event: &FileEvent) {
        match event {
            FileEvent::AreasListed(areas) => self.areas = areas.clone(),
            FileEvent::FolderListed { nodes } => self.nodes = nodes.clone(),
            FileEvent::NodeUpdated(node) => self.upsert_node(node.clone()),
            FileEvent::FileDownloaded { node, size } => {
                self.upsert_node(node.clone());
                // An inline download completes immediately; surface it in the
                // queue keyed by the node id so the same list view shows both
                // inline and ticketed transfers.
                self.record_transfer(Transfer {
                    id: node.id as u64,
                    name: node.name.clone(),
                    dir: TransferDir::Download,
                    total: *size as u64,
                    done: *size as u64,
                    status: TransferStatus::Done,
                    hash: node.blob_id.as_ref().map(blob_hex),
                    error: None,
                    sources: None,
                    node_id: Some(node.id),
                    retryable: true,
                });
                self.status = format!("Downloaded {} ({})", node.name, human_size(*size as i64));
            }
            FileEvent::FileAdded { id, .. } => {
                self.status = format!("New file (#{id}) available");
            }
            FileEvent::TransferOpened {
                transfer_id,
                size,
                server_have,
            } => {
                let name = self.name_for_transfer(*transfer_id);
                self.record_transfer(Transfer {
                    id: *transfer_id,
                    name,
                    dir: TransferDir::Download,
                    total: *size,
                    done: *server_have,
                    status: if *server_have >= *size && *size > 0 {
                        TransferStatus::Done
                    } else if *server_have > 0 {
                        TransferStatus::Active
                    } else {
                        TransferStatus::Queued
                    },
                    hash: None,
                    error: None,
                    sources: None,
                    node_id: None,
                    retryable: true,
                });
            }
            FileEvent::ChunkReceived {
                transfer_id,
                offset,
                last,
                len,
            } => {
                if let Some(t) = self.transfers.iter_mut().find(|t| t.id == *transfer_id) {
                    // `offset + len` is authoritative for the high-water mark;
                    // out-of-order chunks never move it backwards.
                    t.done = t.done.max(offset.saturating_add(*len as u64));
                    if let Some(total) = (t.total > 0).then_some(t.total) {
                        t.done = t.done.min(total);
                    }
                    t.status = if *last {
                        TransferStatus::Done
                    } else {
                        TransferStatus::Active
                    };
                }
            }
            FileEvent::Failed(detail) => {
                self.status = format!("Error: {detail}");
                // No transfer id on this one (a list or metadata request), so
                // the most recent still-running transfer is the best guess.
                if let Some(t) =
                    self.transfers.iter_mut().rev().find(|t| {
                        matches!(t.status, TransferStatus::Queued | TransferStatus::Active)
                    })
                {
                    t.status = TransferStatus::Failed;
                    // Keep the reason ON the transfer. It was already in hand
                    // here and thrown away, which is why a failed row could
                    // only ever say "Failed".
                    t.error = Some(detail.clone());
                }
            }
            FileEvent::TransferFailed {
                transfer_id,
                detail,
                sources_tried,
                retryable,
            } => {
                self.status = format!("Error: {detail}");
                match self.transfers.iter_mut().find(|t| t.id == *transfer_id) {
                    Some(t) => {
                        t.status = TransferStatus::Failed;
                        t.error = Some(detail.clone());
                        t.sources = Some(*sources_tried as u32);
                        t.retryable = *retryable;
                    }
                    // A download can fail *before* any transfer is opened (no
                    // sources found, the origin refusing a ticket). Recording
                    // the failure as a row is the difference between "it told
                    // me why" and a click that appears to do nothing at all.
                    None => {
                        let name = self.name_for_transfer(*transfer_id);
                        let node_id = self
                            .nodes
                            .iter()
                            .find(|n| n.id as u64 == *transfer_id)
                            .map(|n| n.id);
                        self.transfers.push(Transfer {
                            id: *transfer_id,
                            name,
                            dir: TransferDir::Download,
                            total: 0,
                            done: 0,
                            status: TransferStatus::Failed,
                            hash: None,
                            error: Some(detail.clone()),
                            sources: Some(*sources_tried as u32),
                            node_id,
                            retryable: *retryable,
                        });
                    }
                }
            }
        }
    }

    /// Insert `node`, replacing an existing entry with the same id.
    fn upsert_node(&mut self, node: FileNodeView) {
        if let Some(slot) = self.nodes.iter_mut().find(|n| n.id == node.id) {
            *slot = node;
        } else {
            self.nodes.push(node);
        }
    }

    /// Insert or replace a transfer keyed by id.
    fn record_transfer(&mut self, transfer: Transfer) {
        if let Some(slot) = self.transfers.iter_mut().find(|t| t.id == transfer.id) {
            *slot = transfer;
        } else {
            self.transfers.push(transfer);
        }
    }

    /// A display name for a transfer id, from a matching node if one is loaded.
    fn name_for_transfer(&self, transfer_id: u64) -> String {
        self.nodes
            .iter()
            .find(|n| n.id as u64 == transfer_id)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| format!("transfer #{transfer_id}"))
    }

    /// The node whose metadata card is shown, if any.
    pub fn selected_node(&self) -> Option<&FileNodeView> {
        let id = self.selected?;
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Breadcrumb crumbs for the current location, most general first. Each is
    /// a `(label, path)` pair; the area root has `path == None`.
    pub fn breadcrumbs(&self) -> Vec<(String, Option<String>)> {
        let mut crumbs = vec![("Root".to_string(), None)];
        let mut acc = String::new();
        for seg in &self.path {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(seg);
            crumbs.push((seg.clone(), Some(acc.clone())));
        }
        crumbs
    }
}

/// The current folder path as a `/`-joined string, or `None` at the root.
pub fn join_path(segments: &[String]) -> Option<String> {
    if segments.is_empty() {
        None
    } else {
        Some(segments.join("/"))
    }
}

/// A short, human-readable label for a [`FileNodeView::kind`].
/// Does a file row match a type-to-filter query? Case-insensitive substring over
/// the name and the uploader — the two things you actually squint for in a busy
/// library. An empty query matches everything. Pure — host-tested.
pub fn node_matches(name: &str, uploader: &str, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    name.to_lowercase().contains(&q) || uploader.to_lowercase().contains(&q)
}

/// A compact "when was this added" for a dense table: today/yesterday, then days,
/// then weeks, months, years. `created_at_unix` and `now_unix` are seconds; a
/// zero or future timestamp reads as "—" rather than a lie. Pure — host-tested.
pub fn relative_day(created_at_unix: i64, now_unix: i64) -> String {
    if created_at_unix <= 0 {
        return "\u{2014}".to_string();
    }
    let secs = now_unix - created_at_unix;
    if secs < 0 {
        return "\u{2014}".to_string();
    }
    const DAY: i64 = 86_400;
    let days = secs / DAY;
    match days {
        0 => "today".to_string(),
        1 => "yesterday".to_string(),
        2..=6 => format!("{days}d ago"),
        7..=27 => format!("{}w ago", days / 7),
        28..=364 => format!("{}mo ago", days / 30),
        _ => format!("{}y ago", days / 365),
    }
}

pub fn node_kind_label(kind: u8) -> &'static str {
    match kind {
        KIND_FOLDER => "folder",
        KIND_FILE => "file",
        KIND_ALIAS => "alias",
        _ => "node",
    }
}

/// Format a byte count as a compact human string (`1.5 KB`, `3.0 MB`, …).
/// Negative sizes (never expected on the wire) clamp to `0 B`.
pub fn human_size(bytes: i64) -> String {
    if bytes <= 0 {
        return "0 B".to_string();
    }
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: i64, kind: u8, name: &str) -> FileNodeView {
        FileNodeView::new(id, "warez", kind, name, name)
    }

    #[test]
    fn areas_and_folder_listings_replace_state() {
        let mut s = FilesState::default();
        s.apply(&FileEvent::AreasListed(vec![FileAreaView::new(
            "warez", "Warez", "goods",
        )]));
        assert_eq!(s.areas.len(), 1);
        s.apply(&FileEvent::FolderListed {
            nodes: vec![node(1, KIND_FILE, "a.lha")],
        });
        assert_eq!(s.nodes.len(), 1);
        // A second listing replaces, not appends.
        s.apply(&FileEvent::FolderListed {
            nodes: vec![node(2, KIND_FILE, "b.lha")],
        });
        assert_eq!(s.nodes.len(), 1);
        assert_eq!(s.nodes[0].id, 2);
    }

    #[test]
    fn node_updated_upserts() {
        let mut s = FilesState::default();
        s.apply(&FileEvent::FolderListed {
            nodes: vec![node(1, KIND_FILE, "a.lha")],
        });
        let mut updated = node(1, KIND_FILE, "a.lha");
        updated.comment = "edited".into();
        s.apply(&FileEvent::NodeUpdated(updated));
        assert_eq!(s.nodes.len(), 1);
        assert_eq!(s.nodes[0].comment, "edited");
        // An unknown id is appended.
        s.apply(&FileEvent::NodeUpdated(node(9, KIND_FILE, "new.lha")));
        assert_eq!(s.nodes.len(), 2);
    }

    #[test]
    fn inline_download_records_a_completed_transfer() {
        let mut s = FilesState::default();
        s.apply(&FileEvent::FileDownloaded {
            node: node(3, KIND_FILE, "c.lha"),
            size: 2048,
        });
        assert_eq!(s.transfers.len(), 1);
        let t = &s.transfers[0];
        assert_eq!(t.status, TransferStatus::Done);
        assert_eq!(t.total, 2048);
        assert_eq!(t.percent(), 100);
        assert!(s.status.contains("2.0 KB"));
    }

    #[test]
    fn ticketed_transfer_progresses_with_chunks() {
        let mut s = FilesState::default();
        s.apply(&FileEvent::FolderListed {
            nodes: vec![node(5, KIND_FILE, "big.zip")],
        });
        s.apply(&FileEvent::TransferOpened {
            transfer_id: 5,
            size: 1000,
            server_have: 0,
        });
        assert_eq!(s.transfers[0].status, TransferStatus::Queued);
        assert_eq!(s.transfers[0].name, "big.zip");

        s.apply(&FileEvent::ChunkReceived {
            transfer_id: 5,
            offset: 0,
            last: false,
            len: 400,
        });
        assert_eq!(s.transfers[0].status, TransferStatus::Active);
        assert_eq!(s.transfers[0].done, 400);
        assert_eq!(s.transfers[0].percent(), 40);

        // Out-of-order / duplicate chunk cannot rewind progress.
        s.apply(&FileEvent::ChunkReceived {
            transfer_id: 5,
            offset: 0,
            last: false,
            len: 100,
        });
        assert_eq!(s.transfers[0].done, 400);

        s.apply(&FileEvent::ChunkReceived {
            transfer_id: 5,
            offset: 400,
            last: true,
            len: 600,
        });
        assert_eq!(s.transfers[0].status, TransferStatus::Done);
        assert_eq!(s.transfers[0].done, 1000);
        assert_eq!(s.transfers[0].percent(), 100);
    }

    #[test]
    fn failure_marks_running_transfer_and_status() {
        let mut s = FilesState::default();
        s.apply(&FileEvent::TransferOpened {
            transfer_id: 7,
            size: 100,
            server_have: 10,
        });
        s.apply(&FileEvent::Failed("boom".into()));
        assert_eq!(s.transfers[0].status, TransferStatus::Failed);
        assert!(s.status.contains("boom"));
    }

    #[test]
    fn filter_matches_name_or_uploader_case_insensitively() {
        assert!(
            node_matches("Cool Demo.zip", "rabbit", ""),
            "empty query matches all"
        );
        assert!(node_matches("Cool Demo.zip", "rabbit", "demo"));
        assert!(
            node_matches("Cool Demo.zip", "rabbit", "DEMO"),
            "case-insensitive"
        );
        assert!(
            node_matches("Cool Demo.zip", "rabbit", "  demo "),
            "query is trimmed"
        );
        // You often squint for who uploaded it, not just the filename.
        assert!(node_matches("Cool Demo.zip", "rabbit", "rabb"));
        assert!(!node_matches("Cool Demo.zip", "rabbit", "mp3"));
    }

    #[test]
    fn relative_day_reads_naturally_and_never_lies() {
        const DAY: i64 = 86_400;
        let now = 1_000 * DAY;
        assert_eq!(relative_day(now, now), "today");
        assert_eq!(relative_day(now - DAY, now), "yesterday");
        assert_eq!(relative_day(now - 3 * DAY, now), "3d ago");
        assert_eq!(relative_day(now - 14 * DAY, now), "2w ago");
        assert_eq!(relative_day(now - 60 * DAY, now), "2mo ago");
        assert_eq!(relative_day(now - 800 * DAY, now), "2y ago");
        // Unknown or future timestamps show an em dash rather than inventing one.
        assert_eq!(relative_day(0, now), "\u{2014}");
        assert_eq!(relative_day(now + DAY, now), "\u{2014}");
    }

    #[test]
    fn a_failed_transfer_keeps_why_and_which_row() {
        let mut s = FilesState::default();
        // Two downloads in flight — the case that made "the most recent
        // running transfer" the wrong answer.
        s.apply(&FileEvent::TransferOpened {
            transfer_id: 1,
            size: 100,
            server_have: 0,
        });
        s.apply(&FileEvent::TransferOpened {
            transfer_id: 2,
            size: 100,
            server_have: 0,
        });
        s.apply(&FileEvent::TransferFailed {
            transfer_id: 1,
            detail: "peer refused the ticket".into(),
            sources_tried: 2,
            retryable: true,
        });
        let one = s.transfers.iter().find(|t| t.id == 1).unwrap();
        let two = s.transfers.iter().find(|t| t.id == 2).unwrap();
        assert_eq!(one.status, TransferStatus::Failed);
        assert_eq!(one.error.as_deref(), Some("peer refused the ticket"));
        assert_eq!(one.sources, Some(2));
        assert!(one.retryable);
        // The other download is untouched — the id decides, not recency.
        assert_ne!(two.status, TransferStatus::Failed);
        assert!(two.error.is_none());
    }

    #[test]
    fn a_failure_before_the_transfer_opened_still_becomes_a_row() {
        // No sources found means the download never opened a transfer. Without
        // a row the click looks like it did nothing at all.
        let mut s = FilesState::default();
        s.apply(&FileEvent::TransferFailed {
            transfer_id: 42,
            detail: "no peer has this file".into(),
            sources_tried: 0,
            retryable: false,
        });
        assert_eq!(s.transfers.len(), 1);
        assert_eq!(s.transfers[0].id, 42);
        assert_eq!(s.transfers[0].status, TransferStatus::Failed);
        assert_eq!(
            s.transfers[0].error.as_deref(),
            Some("no peer has this file")
        );
    }

    #[test]
    fn an_unretryable_failure_says_so() {
        let mut s = FilesState::default();
        s.apply(&FileEvent::TransferOpened {
            transfer_id: 9,
            size: 10,
            server_have: 0,
        });
        s.apply(&FileEvent::TransferFailed {
            transfer_id: 9,
            detail: "no peer has this file".into(),
            sources_tried: 0,
            retryable: false,
        });
        let t = &s.transfers[0];
        assert_eq!(t.status, TransferStatus::Failed);
        assert!(!t.retryable, "a file nobody holds won't appear on retry");
        assert_eq!(t.sources, Some(0));
    }

    #[test]
    fn zero_byte_transfer_progress_edge() {
        let t = Transfer {
            id: 1,
            name: "x".into(),
            dir: TransferDir::Upload,
            total: 0,
            done: 0,
            status: TransferStatus::Active,
            hash: None,
            error: None,
            sources: None,
            node_id: None,
            retryable: true,
        };
        assert_eq!(t.progress(), 0.0);
        let done = Transfer {
            status: TransferStatus::Done,
            ..t
        };
        assert_eq!(done.progress(), 1.0);
    }

    #[test]
    fn selected_node_lookup() {
        let mut s = FilesState::default();
        s.apply(&FileEvent::FolderListed {
            nodes: vec![node(1, KIND_FILE, "a.lha"), node(2, KIND_FOLDER, "sub")],
        });
        assert!(s.selected_node().is_none());
        s.selected = Some(2);
        assert_eq!(s.selected_node().unwrap().name, "sub");
    }

    #[test]
    fn breadcrumbs_accumulate_paths() {
        let s = FilesState {
            path: vec!["utils".into(), "cli".into()],
            ..Default::default()
        };
        let crumbs = s.breadcrumbs();
        assert_eq!(crumbs.len(), 3);
        assert_eq!(crumbs[0], ("Root".into(), None));
        assert_eq!(crumbs[1], ("utils".into(), Some("utils".into())));
        assert_eq!(crumbs[2], ("cli".into(), Some("utils/cli".into())));
    }

    #[test]
    fn join_path_and_labels() {
        assert_eq!(join_path(&[]), None);
        assert_eq!(
            join_path(&["a".to_string(), "b".to_string()]),
            Some("a/b".to_string())
        );
        assert_eq!(node_kind_label(KIND_FOLDER), "folder");
        assert_eq!(node_kind_label(KIND_ALIAS), "alias");
        assert_eq!(node_kind_label(99), "node");
    }

    #[test]
    fn human_size_scales_units() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(-5), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
