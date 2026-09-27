//! Shared authenticated-peer budgets. No lock is held across I/O.
use std::sync::Mutex;

use anyhow::Result;
use rabbithole_federation::{PeerPolicy, RateLimiter};
use rabbithole_server_core::ServerConfig;

use crate::Shared;

const MAX_PEERS: usize = 4096;

#[derive(Debug)]
pub(super) struct Refusal(pub &'static str);
impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Refusal {}

#[derive(Clone, PartialEq, Eq)]
struct Policy {
    limits: [(u32, u32); 3],
    denied: Vec<String>,
}
impl Policy {
    fn limits(config: &ServerConfig) -> [(u32, u32); 3] {
        [
            (
                config.federation_ingest_frames_burst,
                config.federation_ingest_frames_per_sec,
            ),
            (
                config.federation_ingest_bytes_burst,
                config.federation_ingest_bytes_per_sec,
            ),
            (
                config.federation_ingest_events_burst,
                config.federation_ingest_events_per_sec,
            ),
        ]
    }
    fn matches(&self, config: &ServerConfig) -> bool {
        self.limits == Self::limits(config) && self.denied == config.federation_denied_keys
    }
    fn from_config(config: &ServerConfig) -> Self {
        Self {
            limits: Self::limits(config),
            denied: config.federation_denied_keys.clone(),
        }
    }
}

struct State {
    policy: Policy,
    denied: PeerPolicy,
    buckets: [RateLimiter; 3],
}
#[derive(Default)]
pub(crate) struct IngestState(Mutex<Option<State>>);

impl IngestState {
    fn check(
        &self,
        config: &ServerConfig,
        key: &[u8; 32],
        costs: [u32; 3],
        now: i64,
    ) -> Result<()> {
        let mut held = self.0.lock().expect("federation ingest mutex");
        if held
            .as_ref()
            .is_none_or(|state| !state.policy.matches(config))
        {
            // Validate and allocate only when policy changes. Invalid direct
            // programmatic updates fail closed without replacing good state.
            rabbithole_server_core::config::validate_federation_denied_keys(
                &config.federation_denied_keys,
            )?;
            let policy = Policy::from_config(config);
            let denied = PeerPolicy::deny(policy.denied.iter().filter_map(|s| super::hex_key(s)));
            if let Some(state) = held.as_mut() {
                for (bucket, (capacity, rate)) in state.buckets.iter_mut().zip(policy.limits) {
                    bucket.update_policy(capacity, f64::from(rate), now);
                }
                state.denied = denied;
                state.policy = policy;
            } else {
                *held = Some(State {
                    buckets: policy.limits.map(|(capacity, rate)| {
                        RateLimiter::bounded(capacity, f64::from(rate), MAX_PEERS)
                    }),
                    policy,
                    denied,
                });
            }
        }
        let state = held.as_mut().expect("ingest policy initialized");
        if !state.denied.permits(key) {
            return Err(Refusal("peer explicitly denied by federation policy").into());
        }
        for ((bucket, cost), name) in state.buckets.iter_mut().zip(costs).zip([
            "federation ingest frames budget exhausted",
            "federation ingest bytes budget exhausted",
            "federation ingest events budget exhausted",
        ]) {
            if !bucket.try_acquire_many(key, cost, now) {
                return Err(Refusal(name).into());
            }
        }
        Ok(())
    }
}
fn now() -> i64 {
    i64::try_from(rabbithole_server_core::ratelimit::now_ms()).unwrap_or(i64::MAX)
}
/// Admission only; no bucket is allocated for denied or unapproved keys.
pub(super) fn permit(shared: &Shared, key: &[u8; 32]) -> Result<()> {
    shared
        .config
        .read_with(|config| shared.fed_ingest.check(config, key, [0; 3], now()))
}
pub(super) fn frame(
    shared: &Shared,
    key: &[u8; 32],
    origin: &str,
    frame: &rabbithole_proto::Frame,
) -> Result<()> {
    if !shared.peers.is_approved_origin(key, origin) {
        return Err(Refusal("peer approval revoked").into());
    }
    let bytes = u32::try_from(frame.payload.0.len()).unwrap_or(u32::MAX);
    shared
        .config
        .read_with(|config| shared.fed_ingest.check(config, key, [1, bytes, 0], now()))
}
pub(super) fn events(shared: &Shared, key: &[u8; 32], count: usize) -> Result<()> {
    shared.config.read_with(|config| {
        shared.fed_ingest.check(
            config,
            key,
            [0, 0, u32::try_from(count).unwrap_or(u32::MAX)],
            now(),
        )
    })
}

#[cfg(test)]
mod tests;
