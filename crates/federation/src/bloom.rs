//! A classic Bloom filter for the federation seen-set.
//!
//! The seen-set answers "have I already processed this event id?" to gate
//! re-ingest and rebroadcast loops (the durable stores remain the permanent
//! record; this is the fast, space-cheap front line, complementing the
//! time-windowed `DedupStore` in `server-core`). A Bloom filter trades a
//! small, tunable false-positive rate for a fixed, tiny footprint that never
//! grows with the number of inserts.
//!
//! It is a standard `m`-bit vector with `k` hash functions. The `k` bit
//! positions for an id are derived from a single `blake3(salt ‖ id)` split
//! into two 64-bit halves and combined by the Kirsch–Mitzenmacher double
//! hashing scheme (`g_i = h1 + i·h2 mod m`) — one hash, `k` cheap positions,
//! no measurable loss in false-positive rate.
//!
//! Guarantees: **no false negatives** (if `insert` was called, `contains`
//! returns `true`); false positives occur at approximately the configured
//! rate. The `salt` lets independent filters (or filters on different peers)
//! disagree on which ids collide, so an attacker can't craft one id that
//! collides everywhere.

/// A fixed-capacity Bloom filter over 32-byte event ids.
#[derive(Debug, Clone)]
pub struct BloomFilter {
    /// Bit storage, packed 64 bits per word.
    words: Vec<u64>,
    /// Number of bits (`words.len() * 64 >= num_bits`).
    num_bits: usize,
    /// Number of hash functions.
    num_hashes: u32,
    /// Salt mixed into the hash so filters can differ.
    salt: u64,
}

impl BloomFilter {
    /// Build a filter sized for `expected_items` insertions at the target
    /// `fp_rate` false-positive probability, using the optimal `m` and `k`.
    ///
    /// `expected_items` is clamped to at least 1; `fp_rate` is clamped into
    /// the open interval `(0, 1)` so the sizing math stays finite.
    pub fn with_capacity(expected_items: usize, fp_rate: f64) -> Self {
        Self::with_capacity_salted(expected_items, fp_rate, 0)
    }

    /// [`with_capacity`](Self::with_capacity) with an explicit salt.
    pub fn with_capacity_salted(expected_items: usize, fp_rate: f64, salt: u64) -> Self {
        let n = expected_items.max(1) as f64;
        let p = fp_rate.clamp(f64::MIN_POSITIVE, 1.0 - f64::EPSILON);
        let ln2 = core::f64::consts::LN_2;

        // Optimal bit count: m = -n·ln(p) / (ln2)^2.
        let m = (-(n * p.ln()) / (ln2 * ln2)).ceil();
        let num_bits = (m as usize).max(1);
        // Optimal hash count: k = (m/n)·ln2.
        let k = ((num_bits as f64 / n) * ln2).round();
        let num_hashes = (k as u32).max(1);

        let words = vec![0u64; num_bits.div_ceil(64)];
        BloomFilter {
            words,
            num_bits,
            num_hashes,
            salt,
        }
    }

    /// Record an id as seen.
    pub fn insert(&mut self, id: &[u8; 32]) {
        let (h1, h2) = self.base_hashes(id);
        for i in 0..self.num_hashes as u64 {
            let bit = self.bit_index(h1, h2, i);
            self.words[bit / 64] |= 1u64 << (bit % 64);
        }
    }

    /// Test whether an id may have been seen. `false` is definitive (never
    /// seen); `true` means "probably seen" (subject to the false-positive
    /// rate).
    pub fn contains(&self, id: &[u8; 32]) -> bool {
        let (h1, h2) = self.base_hashes(id);
        (0..self.num_hashes as u64).all(|i| {
            let bit = self.bit_index(h1, h2, i);
            self.words[bit / 64] & (1u64 << (bit % 64)) != 0
        })
    }

    /// The filter's bit-vector length (`m`).
    pub fn num_bits(&self) -> usize {
        self.num_bits
    }

    /// The number of hash functions (`k`).
    pub fn num_hashes(&self) -> u32 {
        self.num_hashes
    }

    /// Two independent 64-bit base hashes from `blake3(salt ‖ id)`.
    fn base_hashes(&self, id: &[u8; 32]) -> (u64, u64) {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&self.salt.to_le_bytes());
        hasher.update(id);
        let digest = hasher.finalize();
        let bytes = digest.as_bytes();
        let h1 = u64::from_le_bytes(bytes[0..8].try_into().expect("8 bytes"));
        // Force h2 odd so the arithmetic progression visits distinct
        // positions (a zero step would map every hash to the same bit).
        let h2 = u64::from_le_bytes(bytes[8..16].try_into().expect("8 bytes")) | 1;
        (h1, h2)
    }

    /// The `i`-th bit position via double hashing, folded into `[0, m)`.
    fn bit_index(&self, h1: u64, h2: u64, i: u64) -> usize {
        (h1.wrapping_add(i.wrapping_mul(h2)) % self.num_bits as u64) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic id from a counter — spreads across the byte space.
    fn id(n: u64) -> [u8; 32] {
        let mut out = [0u8; 32];
        let h = blake3::hash(&n.to_le_bytes());
        out.copy_from_slice(h.as_bytes());
        out
    }

    #[test]
    fn sizing_is_sane() {
        let bf = BloomFilter::with_capacity(10_000, 0.01);
        // For p=0.01, k should be about 7 (ln2 * m/n ≈ 6.6 → 7).
        assert_eq!(bf.num_hashes(), 7);
        assert!(bf.num_bits() >= 10_000);
    }

    #[test]
    fn no_false_negatives_over_large_set() {
        let n = 20_000usize;
        let mut bf = BloomFilter::with_capacity(n, 0.01);
        for i in 0..n as u64 {
            bf.insert(&id(i));
        }
        // Everything inserted must report present — this is the hard
        // guarantee a seen-set relies on.
        for i in 0..n as u64 {
            assert!(bf.contains(&id(i)), "false negative at {i}");
        }
    }

    #[test]
    fn false_positive_rate_is_approximately_correct() {
        let n = 10_000usize;
        let target = 0.01;
        let mut bf = BloomFilter::with_capacity(n, target);
        for i in 0..n as u64 {
            bf.insert(&id(i));
        }
        // Probe with ids we never inserted (disjoint range).
        let trials = 50_000u64;
        let mut fp = 0u64;
        for i in 0..trials {
            if bf.contains(&id(1_000_000 + i)) {
                fp += 1;
            }
        }
        let observed = fp as f64 / trials as f64;
        // Generous band: correct sizing lands near 1%; anything under ~2.5%
        // proves the math and hashing are working (and not, say, saturating
        // every bit).
        assert!(
            observed < 0.025,
            "observed fp rate {observed} too high (target {target})"
        );
    }

    #[test]
    fn different_salts_disagree_on_collisions() {
        // A never-inserted id shouldn't be forced to collide across salts.
        let a = BloomFilter::with_capacity_salted(100, 0.01, 1);
        let b = BloomFilter::with_capacity_salted(100, 0.01, 2);
        // Empty filters: nothing present, regardless of salt.
        assert!(!a.contains(&id(42)));
        assert!(!b.contains(&id(42)));
        // The salt actually changes bit selection.
        assert_ne!(a.base_hashes(&id(42)), b.base_hashes(&id(42)));
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        // Zero items and extreme rates must still yield a usable filter.
        let mut bf = BloomFilter::with_capacity(0, 0.0);
        bf.insert(&id(1));
        assert!(bf.contains(&id(1)));
        let bf2 = BloomFilter::with_capacity(1, 1.0);
        assert!(bf2.num_bits() >= 1 && bf2.num_hashes() >= 1);
    }
}
