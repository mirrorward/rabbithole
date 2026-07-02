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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferStatus {
    /// Authorised but no bytes moved yet.
    Queued,
    /// Bytes in flight.
    Active,
    /// Finished successfully.
    Done,
    /// Aborted or errored.
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
                // The most recent still-running transfer is the likely victim.
                if let Some(t) =
                    self.transfers.iter_mut().rev().find(|t| {
                        matches!(t.status, TransferStatus::Queued | TransferStatus::Active)
                    })
                {
                    t.status = TransferStatus::Failed;
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
    fn zero_byte_transfer_progress_edge() {
        let t = Transfer {
            id: 1,
            name: "x".into(),
            dir: TransferDir::Upload,
            total: 0,
            done: 0,
            status: TransferStatus::Active,
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
