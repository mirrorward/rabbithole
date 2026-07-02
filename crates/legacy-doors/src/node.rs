//! Node-number allocation: [`NodePool`] and its RAII [`NodeLease`].
//!
//! Classic multi-node BBS software gives every simultaneous caller a small
//! 1-based **node number**, and drop files carry it so doors can keep
//! per-node state files apart. [`NodePool`] models the board's node table:
//! it hands out the **lowest free number**, and a [`NodeLease`] gives it back
//! automatically on drop (or explicitly via [`NodeLease::release`]).
//!
//! Doors restricted to a [`NodeRange`] allocate through
//! [`NodePool::allocate_in`]; a single-node door locks correctly because its
//! one-slot range simply has no second number to offer.
//!
//! The pool is thread-safe (a plain [`Mutex`] around a [`BTreeSet`]) and
//! never panics — a poisoned lock is recovered, since the guarded set is
//! always in a consistent state.
//!
//! ```
//! use std::sync::Arc;
//! use rabbithole_legacy_doors::NodePool;
//!
//! let pool = Arc::new(NodePool::new(2));
//! let a = pool.allocate().unwrap();
//! let b = pool.allocate().unwrap();
//! assert_eq!((a.node(), b.node()), (1, 2));
//! assert!(pool.allocate().is_err()); // full
//! drop(a);
//! assert_eq!(pool.allocate().unwrap().node(), 1); // lowest free again
//! # drop(b);
//! ```

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::door::NodeRange;
use crate::error::Error;

/// A pool of node numbers `1..=max_nodes`, allocated lowest-free-first.
///
/// Construct once, wrap in an [`Arc`], and allocate [`NodeLease`]s from it.
#[derive(Debug)]
pub struct NodePool {
    max_nodes: u16,
    taken: Mutex<BTreeSet<u16>>,
}

impl NodePool {
    /// A pool offering node numbers `1..=max_nodes`. A `max_nodes` of `0`
    /// yields a pool that can never allocate (every request fails).
    #[must_use]
    pub fn new(max_nodes: u16) -> Self {
        NodePool {
            max_nodes,
            taken: Mutex::new(BTreeSet::new()),
        }
    }

    /// The highest node number this pool can hand out.
    #[must_use]
    pub fn max_nodes(&self) -> u16 {
        self.max_nodes
    }

    /// How many nodes are currently allocated.
    #[must_use]
    pub fn in_use(&self) -> usize {
        self.locked().len()
    }

    /// Whether `node` is currently unallocated (regardless of whether it is
    /// within `1..=max_nodes`).
    #[must_use]
    pub fn is_free(&self, node: u16) -> bool {
        !self.locked().contains(&node)
    }

    /// Allocate the lowest free node in the whole pool (`1..=max_nodes`).
    ///
    /// # Errors
    ///
    /// [`Error::NodesExhausted`] when every node is taken (or the pool is
    /// empty).
    pub fn allocate(self: &Arc<Self>) -> Result<NodeLease, Error> {
        self.allocate_in(NodeRange::new(1, self.max_nodes))
    }

    /// Allocate the lowest free node inside `range` (clamped to the pool's
    /// `1..=max_nodes`). Use this with a door's [`NodeRange`]; a single-node
    /// range serializes callers, since a second allocation finds no free
    /// slot until the first lease drops.
    ///
    /// # Errors
    ///
    /// [`Error::NodesExhausted`] when no node in the (clamped) range is
    /// free, or when the range does not intersect the pool at all.
    pub fn allocate_in(self: &Arc<Self>, range: NodeRange) -> Result<NodeLease, Error> {
        let exhausted = Err(Error::NodesExhausted {
            first: range.first,
            last: range.last,
        });
        let first = range.first.max(1);
        let last = range.last.min(self.max_nodes);
        if first > last {
            return exhausted;
        }
        let mut taken = self.locked();
        match (first..=last).find(|n| !taken.contains(n)) {
            Some(node) => {
                taken.insert(node);
                drop(taken);
                Ok(NodeLease {
                    node,
                    pool: Arc::clone(self),
                })
            }
            None => exhausted,
        }
    }

    /// Lock the allocation set, recovering from poisoning (the set is always
    /// consistent, so a panic in another thread cannot corrupt it).
    fn locked(&self) -> MutexGuard<'_, BTreeSet<u16>> {
        self.taken.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Return `node` to the pool (called by [`NodeLease`] on drop).
    fn release(&self, node: u16) {
        self.locked().remove(&node);
    }
}

/// An allocated node number, returned to its [`NodePool`] on drop.
///
/// The lease keeps an [`Arc`] to its pool, so it stays valid even if the
/// caller drops every other pool handle.
#[derive(Debug)]
pub struct NodeLease {
    node: u16,
    pool: Arc<NodePool>,
}

impl NodeLease {
    /// The node number this lease holds.
    #[must_use]
    pub fn node(&self) -> u16 {
        self.node
    }

    /// Release the node explicitly (equivalent to dropping the lease; this
    /// method just makes the intent readable at the call site).
    pub fn release(self) {
        drop(self);
    }
}

impl Drop for NodeLease {
    fn drop(&mut self) {
        self.pool.release(self.node);
    }
}
