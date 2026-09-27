//! Bounded, fair history passes shared by all live links to one peer.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rabbithole_store_server::repo4::HistoryCursor;
use tokio::time::Instant;

use super::{fed_frame, FloodEdge, IHave, Interest, MT_IHAVE};
use crate::Shared;
use anyhow::{bail, Result};
use rabbithole_net::Connection;
use rabbithole_proto::FrameKind;
use rabbithole_store_server::repo4::HistoryEntry;

pub(super) const SCAN_LIMIT: usize = 256;
pub(super) const OFFER_IDS: usize = 128;
pub(super) const OFFER_FRAMES: usize = 8;
const PEER_LIMIT: usize = 4096;
const LINK_LIMIT: usize = 8;

#[derive(Default)]
pub(crate) struct HistoryState(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
    next_link: u64,
    peers: HashMap<[u8; 32], Peer>,
}

struct Peer {
    last_pass: Option<Instant>,
    last_link: u64,
    links: BTreeMap<u64, Link>,
}

#[derive(Default)]
struct Link {
    active: bool,
    busy: bool,
    cursor: Option<HistoryCursor>,
}

pub(super) struct Registration {
    state: Arc<Mutex<State>>,
    peer: [u8; 32],
    link: u64,
}

pub(super) struct Pass {
    state: Arc<Mutex<State>>,
    peer: [u8; 32],
    link: u64,
    pub cursor: Option<HistoryCursor>,
}

impl HistoryState {
    pub(super) fn register(&self, peer: [u8; 32], now: Instant) -> Option<Registration> {
        let mut state = self.0.lock().expect("history mutex");
        // Disconnected peers retain their cooldown across reconnects. Once the
        // maximum configured interval elapsed, their empty slots are reusable.
        state.peers.retain(|_, entry| {
            !entry.links.is_empty()
                || entry.last_pass.is_some_and(|last| {
                    now.saturating_duration_since(last) < Duration::from_secs(3600)
                })
        });
        if !state.peers.contains_key(&peer) && state.peers.len() >= PEER_LIMIT {
            return None;
        }
        let id = state.next_link.checked_add(1)?;
        state.next_link = id;
        let entry = state.peers.entry(peer).or_insert_with(|| Peer {
            last_pass: None,
            last_link: 0,
            links: BTreeMap::new(),
        });
        if entry.links.len() >= LINK_LIMIT {
            return None;
        }
        entry.links.insert(id, Link::default());
        Some(Registration {
            state: self.0.clone(),
            peer,
            link: id,
        })
    }
}

impl Registration {
    pub(super) fn active(&self, active: bool) {
        if let Some(link) = self
            .state
            .lock()
            .expect("history mutex")
            .peers
            .get_mut(&self.peer)
            .and_then(|peer| peer.links.get_mut(&self.link))
        {
            link.active = active;
        }
    }

    pub(super) fn begin(&self, now: Instant, cadence_secs: u64) -> Option<Pass> {
        let mut state = self.state.lock().expect("history mutex");
        let peer = state.peers.get_mut(&self.peer)?;
        let interval = Duration::from_secs(if cadence_secs == 0 {
            60
        } else {
            cadence_secs.clamp(5, 3600)
        });
        if peer
            .last_pass
            .is_some_and(|last| now.saturating_duration_since(last) < interval)
            || peer.links.values().any(|link| link.busy)
        {
            return None;
        }
        let turn = peer
            .links
            .iter()
            .filter(|(_, link)| link.active)
            .find(|(id, _)| **id > peer.last_link)
            .or_else(|| peer.links.iter().find(|(_, link)| link.active))
            .map(|(&id, _)| id)?;
        if turn != self.link {
            return None;
        }
        let link = peer.links.get_mut(&self.link)?;
        link.busy = true;
        let cursor = link.cursor;
        peer.last_pass = Some(now);
        peer.last_link = self.link;
        Some(Pass {
            state: self.state.clone(),
            peer: self.peer,
            link: self.link,
            cursor,
        })
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(peer) = self
            .state
            .lock()
            .expect("history mutex")
            .peers
            .get_mut(&self.peer)
        {
            peer.links.remove(&self.link);
        }
    }
}

impl Pass {
    pub(super) fn finish(self, cursor: Option<HistoryCursor>) {
        if let Some(link) = self
            .state
            .lock()
            .expect("history mutex")
            .peers
            .get_mut(&self.peer)
            .and_then(|peer| peer.links.get_mut(&self.link))
        {
            link.cursor = cursor;
        }
    }
}

impl Drop for Pass {
    fn drop(&mut self) {
        if let Some(link) = self
            .state
            .lock()
            .expect("history mutex")
            .peers
            .get_mut(&self.peer)
            .and_then(|peer| peer.links.get_mut(&self.link))
        {
            link.busy = false;
        }
    }
}

fn local_interest(shared: &Shared) -> Interest {
    let wanted = shared.config.read().federation_board_subscribe;
    if wanted.iter().any(|board| board == "*" || board == "all") {
        Interest::All
    } else if wanted.is_empty() {
        Interest::None
    } else {
        Interest::Boards(
            wanted
                .into_iter()
                .take(super::MAX_SUBSCRIBE_BOARDS)
                .collect(),
        )
    }
}

fn has_interest(interest: &Interest) -> bool {
    match interest {
        Interest::None => false,
        Interest::All => true,
        Interest::Boards(boards) => !boards.is_empty(),
    }
}

fn eligible(shared: &Shared, local: &Interest, peer: &Interest, entry: &HistoryEntry) -> bool {
    entry.postable
        && local.covers(&entry.board)
        && peer.covers(&entry.board)
        && !shared.moderation.post_quarantined(&entry.target_id)
        && !shared.moderation.post_quarantined(&entry.root_id)
}

/// One recovery opportunity, whether scheduled or subscription-triggered.
/// A session-local live Bloom filter never suppresses recovery candidates.
pub(super) async fn offer(
    conn: &mut dyn Connection,
    shared: &Arc<Shared>,
    edge: &mut FloodEdge,
    periodic: bool,
) -> Result<()> {
    offer_at(conn, shared, edge, periodic, Instant::now()).await
}

async fn offer_at(
    conn: &mut dyn Connection,
    shared: &Arc<Shared>,
    edge: &mut FloodEdge,
    periodic: bool,
    now: Instant,
) -> Result<()> {
    let local = local_interest(shared);
    let Some(registration) = &edge.history else {
        return Ok(());
    };
    registration.active(has_interest(&local) && has_interest(&edge.interest));
    let cadence = shared.config.read().federation_history_reoffer_secs;
    if periodic && cadence == 0 {
        return Ok(());
    }
    if !shared
        .peers
        .is_approved_origin(&edge.peer_key, &edge.peer_origin)
    {
        bail!("history peer approval revoked");
    }
    let Some(pass) = registration.begin(now, cadence) else {
        return Ok(());
    };
    let rows = match shared.boards.history_page(pass.cursor, SCAN_LIMIT).await {
        Ok(rows) => rows,
        Err(error) => {
            // Local storage failure spends this pass but keeps the session and
            // cursor usable. The next normal cadence retries it.
            tracing::debug!(%error, "federation history page unavailable");
            return Ok(());
        }
    };
    let mut complete_page = true;
    let at_end = rows.len() < SCAN_LIMIT;
    let mut cursor = pass.cursor;
    let mut batches: Vec<(String, Vec<HistoryEntry>)> = Vec::new();
    let mut offered = 0;
    for entry in rows {
        if eligible(shared, &local, &edge.interest, &entry) {
            let existing = batches.iter().position(|(board, _)| board == &entry.board);
            if offered == OFFER_IDS || (existing.is_none() && batches.len() == OFFER_FRAMES) {
                complete_page = false;
                break;
            }
            cursor = Some(entry.cursor);
            match existing {
                Some(index) => batches[index].1.push(entry),
                None => batches.push((entry.board.clone(), vec![entry])),
            }
            offered += 1;
        } else {
            // Held, unsubscribed, already offered and invalid-board positions
            // never stall the cursor at the beginning of a large history.
            cursor = Some(entry.cursor);
        }
    }
    for (board, entries) in batches {
        let current_board = match shared.boards.board(&board).await {
            Ok(Some(row)) if row.kind == 2 => row,
            Ok(_) => continue,
            Err(error) => {
                tracing::debug!(%error, "federation history board unavailable");
                return Ok(());
            }
        };
        let local = local_interest(shared);
        let ids: Vec<_> = entries
            .into_iter()
            .filter(|entry| eligible(shared, &local, &edge.interest, entry))
            .map(|entry| entry.cursor.event_id)
            .collect();
        if ids.is_empty() {
            continue;
        }
        // Recheck after every database await, immediately before transmission.
        if !shared
            .peers
            .is_approved_origin(&edge.peer_key, &edge.peer_origin)
        {
            bail!("history peer approval revoked");
        }
        conn.send(fed_frame(
            FrameKind::Push,
            MT_IHAVE,
            &IHave {
                board: current_board.slug,
                event_ids: ids.clone(),
            },
        ))
        .await?;
        for id in ids {
            edge.seen.insert(&id);
        }
    }
    pass.finish(if complete_page && at_end {
        None
    } else {
        cursor
    });
    Ok(())
}

#[cfg(test)]
mod tests;
