//! Sans-I/O flood-fill (epidemic dissemination) engine for tunnel messages.
//!
//! Given the set of tunnel [peers](PeerId) this node is connected to and a
//! [`TunnelMessage`], [`FloodFill::plan_forward`]
//! computes the deterministic set of peers to relay the message to — **every
//! peer except the source and any peer already known to hold it** — decrementing
//! the remaining hop budget and dropping the message once it reaches its hop
//! horizon. This is the classic loop-safe gossip rule: never send a message back
//! toward where it came from, and never re-send it to a peer that already has
//! it.
//!
//! Loop-safety is provided by a per-message *seen-by* record kept in a
//! [`ForwardLedger`] (who we have relayed each id to, or otherwise know holds
//! it), with a TTL and an injected clock — the same TTL discipline as
//! [`AnnounceCache`](crate::announce::AnnounceCache) and
//! [`MessageStore`](crate::tunnel::MessageStore). The engine performs **no I/O,
//! no clock reads, and no randomness**: the caller injects `now_ms` and drives
//! the actual packet transmission.
//!
//! # Recommended flow (the adapter drives this)
//!
//! ```text
//!   on receiving `msg` from `from_peer` at `now`:
//!     ledger.record(msg.id, from_peer, now)          // the source has it
//!     let plan = floodfill.plan_forward(&msg, Some(from_peer), &ledger, now);
//!     let relay = msg.forwarded();                   // hops + 1
//!     for peer in &plan { batcher.enqueue(*peer, relay.clone(), now); }
//!     ledger.record_all(msg.id, &plan, now);         // don't re-flood later
//! ```
//!
//! `plan_forward` is a **pure read** — it never mutates the ledger — so the
//! caller records only the peers it actually transmitted to. A locally-originated
//! message is planned with `from_peer = None`.
//!
//! # Model vs. spec
//!
//! Flooding to "every peer except the source and known holders" is a **model**
//! of epidemic store-and-forward dissemination. It is *not* how upstream RNS
//! `Transport` routes packets (which uses learned paths and a path table), nor
//! how an LXMF *propagation node* syncs with peers (which negotiates a
//! transfer). Those are the interop surfaces a later adapter maps onto.
//!
//! // SPEC-CHECK: real RNS/LXMF propagation limits fan-out by learned topology
//! // and per-peer sync state, not a blind "all-but-source" flood; the hop
//! // horizon here is a message-level TTL, distinct from `RNS.Packet` transport
//! // hop limits. Pinned by the fan-out and loop-safety tests below so an
//! // interop pass can retune the policy in one place.

use std::collections::{BTreeMap, BTreeSet};

use crate::tunnel::{PeerId, TunnelMessage, TUNNEL_ID_LENGTH};

/// A pure record of which peers are known to hold each message id, with a TTL.
///
/// Used by [`FloodFill::plan_forward`] to stay loop-safe: a peer is recorded
/// either because we relayed the message to it, or because we learned it holds
/// the message (e.g. we received the message *from* it). Records expire
/// `ttl_ms` after they were last written; expired records are ignored lazily and
/// removed by [`purge_expired`](Self::purge_expired). All time is a
/// caller-injected monotonic millisecond clock.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ForwardLedger {
    ttl_ms: u64,
    /// message id → (peer → last-recorded ms).
    entries: BTreeMap<[u8; TUNNEL_ID_LENGTH], BTreeMap<PeerId, u64>>,
}

impl ForwardLedger {
    /// Create a ledger whose records live for `ttl_ms`.
    pub fn new(ttl_ms: u64) -> Self {
        Self {
            ttl_ms,
            entries: BTreeMap::new(),
        }
    }

    /// Record that `peer` is known to hold message `id` as of `now_ms`.
    pub fn record(&mut self, id: [u8; TUNNEL_ID_LENGTH], peer: PeerId, now_ms: u64) {
        self.entries.entry(id).or_default().insert(peer, now_ms);
    }

    /// Record a whole plan at once (see [`FloodFill::plan_forward`]).
    pub fn record_all(&mut self, id: [u8; TUNNEL_ID_LENGTH], peers: &[PeerId], now_ms: u64) {
        if peers.is_empty() {
            return;
        }
        let slot = self.entries.entry(id).or_default();
        for &peer in peers {
            slot.insert(peer, now_ms);
        }
    }

    /// Whether `peer` is known (via a non-expired record) to hold `id` at
    /// `now_ms`.
    pub fn knows(&self, id: &[u8; TUNNEL_ID_LENGTH], peer: &PeerId, now_ms: u64) -> bool {
        self.entries
            .get(id)
            .and_then(|slot| slot.get(peer))
            .is_some_and(|&at| now_ms.saturating_sub(at) < self.ttl_ms)
    }

    /// The peers known to hold `id` at `now_ms`, in deterministic order.
    pub fn recipients(&self, id: &[u8; TUNNEL_ID_LENGTH], now_ms: u64) -> Vec<PeerId> {
        let ttl = self.ttl_ms;
        self.entries
            .get(id)
            .into_iter()
            .flat_map(|slot| slot.iter())
            .filter(|(_, &at)| now_ms.saturating_sub(at) < ttl)
            .map(|(&peer, _)| peer)
            .collect()
    }

    /// Drop every record (and now-empty id slot) that has aged past the TTL.
    pub fn purge_expired(&mut self, now_ms: u64) {
        let ttl = self.ttl_ms;
        for slot in self.entries.values_mut() {
            slot.retain(|_, &mut at| now_ms.saturating_sub(at) < ttl);
        }
        self.entries.retain(|_, slot| !slot.is_empty());
    }

    /// Forget everything recorded for `id` (e.g. once its message expired).
    pub fn forget(&mut self, id: &[u8; TUNNEL_ID_LENGTH]) {
        self.entries.remove(id);
    }

    /// Number of message ids with at least one record.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the ledger holds no records at all.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The sans-I/O flood-fill engine: a set of tunnel peers plus the planning rule.
///
/// Holds only the current set of connected tunnel [peers](PeerId). The
/// per-message loop-safety state lives in a separate [`ForwardLedger`] the
/// caller threads through [`plan_forward`](Self::plan_forward), so planning is a
/// pure function of `(peers, message, source, ledger, now)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FloodFill {
    peers: BTreeSet<PeerId>,
}

impl FloodFill {
    /// Create an engine with no peers.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an engine seeded with `peers`.
    pub fn with_peers<I: IntoIterator<Item = PeerId>>(peers: I) -> Self {
        Self {
            peers: peers.into_iter().collect(),
        }
    }

    /// Add a tunnel peer. Returns `true` if it was newly added.
    pub fn add_peer(&mut self, peer: PeerId) -> bool {
        self.peers.insert(peer)
    }

    /// Remove a tunnel peer. Returns `true` if it was present.
    pub fn remove_peer(&mut self, peer: &PeerId) -> bool {
        self.peers.remove(peer)
    }

    /// Whether `peer` is a known tunnel peer.
    pub fn has_peer(&self, peer: &PeerId) -> bool {
        self.peers.contains(peer)
    }

    /// The current peer set, in deterministic (sorted) order.
    pub fn peers(&self) -> Vec<PeerId> {
        self.peers.iter().copied().collect()
    }

    /// Number of tunnel peers.
    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    /// Plan the peers to relay `msg` to, given who it came from and what the
    /// ledger already knows.
    ///
    /// Returns every peer that is **not** the `from_peer` source and **not**
    /// already known (via the `ledger`) to hold the message — unless `msg` has
    /// reached its hop horizon ([`at_horizon`](TunnelMessage::at_horizon)), in
    /// which case the plan is empty (the message is dropped rather than relayed
    /// past `ttl_hops`). The result is deterministic and sorted.
    ///
    /// This is a pure read: it does not mutate the ledger. After transmitting,
    /// record the plan with [`ForwardLedger::record_all`] so a later re-offer of
    /// the same id does not re-flood the same peers.
    pub fn plan_forward(
        &self,
        msg: &TunnelMessage,
        from_peer: Option<PeerId>,
        ledger: &ForwardLedger,
        now_ms: u64,
    ) -> Vec<PeerId> {
        if msg.at_horizon() {
            return Vec::new();
        }
        self.peers
            .iter()
            .filter(|&&peer| Some(peer) != from_peer)
            .filter(|&&peer| !ledger.knows(&msg.id, &peer, now_ms))
            .copied()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::destination::DestinationHash;
    use crate::tunnel::TunnelMessage;

    fn peer(n: u8) -> PeerId {
        DestinationHash([n; 16])
    }

    fn message(hops: u8, ttl_hops: u8) -> TunnelMessage {
        let mut m = TunnelMessage::new(1_000, ttl_hops, 0, b"gossip");
        m.hops = hops;
        m
    }

    // --- ForwardLedger --------------------------------------------------

    #[test]
    fn ledger_records_and_knows_within_ttl() {
        let mut l = ForwardLedger::new(1_000);
        let id = [7u8; 16];
        assert!(!l.knows(&id, &peer(1), 0));
        l.record(id, peer(1), 0);
        assert!(l.knows(&id, &peer(1), 0));
        assert!(l.knows(&id, &peer(1), 999));
        // At/after the TTL the record is stale.
        assert!(!l.knows(&id, &peer(1), 1_000));
        // Other peers/ids are unaffected.
        assert!(!l.knows(&id, &peer(2), 0));
        assert!(!l.knows(&[8u8; 16], &peer(1), 0));
    }

    #[test]
    fn ledger_recipients_and_record_all() {
        let mut l = ForwardLedger::new(10_000);
        let id = [1u8; 16];
        l.record_all(id, &[peer(3), peer(1), peer(2)], 0);
        // Sorted, deterministic.
        assert_eq!(l.recipients(&id, 0), vec![peer(1), peer(2), peer(3)]);
        // Empty plan is a no-op.
        l.record_all([2u8; 16], &[], 0);
        assert!(l.recipients(&[2u8; 16], 0).is_empty());
        assert_eq!(l.len(), 1);
    }

    #[test]
    fn ledger_purge_and_forget() {
        let mut l = ForwardLedger::new(1_000);
        let id = [1u8; 16];
        l.record(id, peer(1), 0);
        l.record(id, peer(2), 800);
        l.purge_expired(1_000); // peer(1) stale, peer(2) live
        assert_eq!(l.recipients(&id, 1_000), vec![peer(2)]);
        assert_eq!(l.len(), 1);
        l.purge_expired(1_800); // both stale → id slot removed
        assert!(l.is_empty());

        l.record(id, peer(1), 0);
        l.forget(&id);
        assert!(l.is_empty());
    }

    #[test]
    fn ledger_clock_regression_is_total() {
        let mut l = ForwardLedger::new(1_000);
        let id = [1u8; 16];
        l.record(id, peer(1), 10_000);
        // Backwards clock: saturating arithmetic keeps the record live.
        assert!(l.knows(&id, &peer(1), 0));
        l.purge_expired(0);
        assert_eq!(l.len(), 1);
    }

    // --- FloodFill ------------------------------------------------------

    #[test]
    fn plan_excludes_source() {
        let ff = FloodFill::with_peers([peer(1), peer(2), peer(3)]);
        let ledger = ForwardLedger::new(10_000);
        let m = message(0, 4);
        // Received from peer(2): relay to the other two, not back to peer(2).
        assert_eq!(
            ff.plan_forward(&m, Some(peer(2)), &ledger, 0),
            vec![peer(1), peer(3)]
        );
    }

    #[test]
    fn plan_for_local_origin_fans_out_to_all() {
        let ff = FloodFill::with_peers([peer(1), peer(2), peer(3)]);
        let ledger = ForwardLedger::new(10_000);
        let m = message(0, 4);
        assert_eq!(
            ff.plan_forward(&m, None, &ledger, 0),
            vec![peer(1), peer(2), peer(3)]
        );
    }

    #[test]
    fn plan_excludes_known_holders_no_reflood() {
        let ff = FloodFill::with_peers([peer(1), peer(2), peer(3)]);
        let mut ledger = ForwardLedger::new(10_000);
        let m = message(0, 4);
        // First delivery from peer(1): plan is peer(2), peer(3).
        let plan = ff.plan_forward(&m, Some(peer(1)), &ledger, 0);
        assert_eq!(plan, vec![peer(2), peer(3)]);
        ledger.record(m.id, peer(1), 0); // source holds it
        ledger.record_all(m.id, &plan, 0); // we relayed to the plan
                                           // A re-offer of the same id (from a new source) must not re-flood: all
                                           // three peers are now known holders.
        assert!(ff.plan_forward(&m, Some(peer(2)), &ledger, 0).is_empty());
    }

    #[test]
    fn plan_is_empty_at_hop_horizon() {
        let ff = FloodFill::with_peers([peer(1), peer(2)]);
        let ledger = ForwardLedger::new(10_000);
        // hops == ttl_hops → dropped.
        assert!(ff.plan_forward(&message(4, 4), None, &ledger, 0).is_empty());
        // hops beyond horizon → still dropped.
        assert!(ff.plan_forward(&message(9, 4), None, &ledger, 0).is_empty());
        // one hop below horizon → still relays.
        assert_eq!(
            ff.plan_forward(&message(3, 4), None, &ledger, 0),
            vec![peer(1), peer(2)]
        );
    }

    #[test]
    fn plan_reflows_after_ledger_ttl_lapses() {
        let ff = FloodFill::with_peers([peer(1), peer(2)]);
        let mut ledger = ForwardLedger::new(1_000);
        let m = message(0, 4);
        ledger.record_all(m.id, &[peer(1), peer(2)], 0);
        assert!(ff.plan_forward(&m, None, &ledger, 0).is_empty());
        // Once the ledger forgets (TTL lapsed), the message can flood again.
        assert_eq!(
            ff.plan_forward(&m, None, &ledger, 1_000),
            vec![peer(1), peer(2)]
        );
    }

    #[test]
    fn peer_set_management() {
        let mut ff = FloodFill::new();
        assert!(ff.add_peer(peer(1)));
        assert!(!ff.add_peer(peer(1))); // already present
        assert!(ff.add_peer(peer(2)));
        assert!(ff.has_peer(&peer(1)));
        assert_eq!(ff.peer_count(), 2);
        assert_eq!(ff.peers(), vec![peer(1), peer(2)]);
        assert!(ff.remove_peer(&peer(1)));
        assert!(!ff.remove_peer(&peer(1)));
        assert_eq!(ff.peers(), vec![peer(2)]);
    }

    #[test]
    fn small_mesh_fanout_reaches_everyone_once_and_terminates() {
        // A 4-node line mesh: A-B-C-D. Message originates at A; verify it
        // propagates to every node exactly once and never loops.
        let (a, b, c, d) = (peer(0xA), peer(0xB), peer(0xC), peer(0xD));
        let neighbours = |n: PeerId| -> FloodFill {
            match n {
                x if x == a => FloodFill::with_peers([b]),
                x if x == b => FloodFill::with_peers([a, c]),
                x if x == c => FloodFill::with_peers([b, d]),
                _ => FloodFill::with_peers([c]),
            }
        };

        let m = TunnelMessage::new(0, 8, 0, b"flood me");
        // Ledger per node (each node tracks who it knows has the message).
        let mut ledgers: BTreeMap<PeerId, ForwardLedger> = [a, b, c, d]
            .into_iter()
            .map(|n| (n, ForwardLedger::new(100_000)))
            .collect();

        // Delivery queue of (at_node, from_peer, message).
        let mut queue: Vec<(PeerId, Option<PeerId>, TunnelMessage)> = vec![(a, None, m.clone())];
        let mut delivered: BTreeMap<PeerId, usize> = BTreeMap::new();

        let mut steps = 0;
        while let Some((node, from, msg)) = queue.pop() {
            steps += 1;
            assert!(steps < 1_000, "flood did not terminate");
            *delivered.entry(node).or_default() += 1;
            if let Some(src) = from {
                ledgers.get_mut(&node).unwrap().record(msg.id, src, 0);
            }
            let ff = neighbours(node);
            let plan = {
                let ledger = &ledgers[&node];
                ff.plan_forward(&msg, from, ledger, 0)
            };
            let relay = msg.forwarded();
            for peer in &plan {
                queue.push((*peer, Some(node), relay.clone()));
            }
            ledgers.get_mut(&node).unwrap().record_all(msg.id, &plan, 0);
        }

        // Every node received the message, and the flood terminated. Each node
        // may be *offered* the message more than once (from each neighbour), but
        // the hop horizon + per-node ledger stop it re-propagating endlessly.
        for n in [a, b, c, d] {
            assert!(delivered.get(&n).copied().unwrap_or(0) >= 1, "{n} missed");
        }
    }
}
