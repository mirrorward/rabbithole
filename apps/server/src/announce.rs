//! Announcing this burrow to Looking Glass trackers, so people who don't
//! already know its address can find it.
//!
//! A burrow that nobody can discover is a burrow nobody joins, so this runs by
//! default. It stays inert until [`ServerConfig::advertise_host`] is set: we
//! will not list an address we can't state.
//!
//! # The wire contract
//!
//! A Looking Glass takes `POST /api/announce` with
//! `{"descriptor": {…}, "signature": "<hex>"}`, where the signature is Ed25519
//! over the **canonical JSON** of the descriptor — object keys sorted
//! recursively, no insignificant whitespace, UTF-8. The coordinator then
//! publishes its index onward to `rabbithole.directory`; burrows never talk to
//! the directory themselves.
//!
//! [`canonical_json`] is the security-critical piece and is written out
//! explicitly rather than leaning on `serde_json`'s map ordering, which is a
//! function of a feature flag (`preserve_order`) that any crate in the tree
//! could turn on. Signing bytes we didn't deliberately produce is how a
//! signature quietly starts covering something else.
//!
//! # Opting out
//!
//! `announce_enabled = false` stops the announce *and* stamps a `noindex`
//! feature tag into the signed `.well-known` descriptor ([`crate::well_known`]).
//! Discovery here is gossip: a visitor can pass your burrow along to a tracker
//! or a friend. Because the tag rides inside your own signature, the wish not
//! to be listed is attributable to you and survives the retelling, rather than
//! depending on the good behaviour of everyone who ever saw you.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use rabbithole_identity::IdentityKey;
use rabbithole_server_core::config::ServerConfig;
use serde_json::{Map, Value};
use tokio::task::JoinHandle;

use crate::syndication::{exchange, parse_http_response, FeedUrl};
use crate::Shared;

/// How long a single announce may take, end to end.
const ANNOUNCE_TIMEOUT: Duration = Duration::from_secs(20);

/// Bounds the glass protocol enforces on `ttl`. Sending something outside them
/// is a guaranteed rejection, so clamp rather than argue.
const TTL_MIN_SECS: u32 = 30;
const TTL_MAX_SECS: u32 = 3600;

/// Field caps from the glass protocol. Over-long values are rejected wholesale,
/// so truncate: a listing with a clipped description beats no listing.
const MAX_SYSOP: usize = 64;
const MAX_DESCRIPTION: usize = 240;
const MAX_LISTENERS: usize = 12;

/// Serialize `value` as canonical JSON: object keys sorted recursively, no
/// insignificant whitespace, UTF-8.
///
/// Both sides of an announce hash *these* bytes, so this is the actual signed
/// message. See the module docs for why it isn't left to `serde_json`.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            // BTreeMap sorts by Rust's `Ord` for `String`, i.e. by UTF-8 bytes,
            // which is what "sorted keys" means for a JSON canonicalization.
            let sorted: BTreeMap<&String, &Value> = map.iter().collect();
            out.push('{');
            for (i, (k, v)) in sorted.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*k).clone()).to_string());
                out.push(':');
                write_canonical(v, out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            // Arrays are ordered data, not a set: order is preserved.
            out.push('[');
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(v, out);
            }
            out.push(']');
        }
        // Scalars have one JSON spelling each, and `serde_json` already emits
        // strings with the escaping the spec requires.
        other => out.push_str(&other.to_string()),
    }
}

/// The announced name, in the glass's `handle@host` form (`alice@wonderland`).
///
/// The handle half is the operator; the host half is what people dial. Falls
/// back to a slug of the burrow's display name so an unconfigured burrow still
/// announces something a human recognizes.
pub fn announce_name(cfg: &ServerConfig) -> Option<String> {
    let host = cfg.advertise_host.trim();
    if host.is_empty() {
        return None;
    }
    let sysop = cfg.announce_sysop.trim();
    let handle = if sysop.is_empty() {
        slug(&cfg.name)
    } else {
        clip(sysop, MAX_SYSOP)
    };
    if handle.is_empty() {
        return None;
    }
    Some(format!("{handle}@{host}"))
}

/// Lowercase a display name into a DNS-ish label: alphanumerics kept, runs of
/// anything else collapsed to a single dash, no leading or trailing dash.
fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(ch.to_ascii_lowercase());
        } else {
            pending_dash = true;
        }
    }
    out
}

/// Truncate to `max` **characters**, never splitting a UTF-8 sequence.
fn clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Build the descriptor to announce, or `None` when this burrow has nothing
/// truthful to say — announcing off, or no `advertise_host` to point at.
///
/// Pure over config + clock so the exact signed document is host-testable.
pub fn descriptor(cfg: &ServerConfig, public_key_hex: &str, now_ms: i64) -> Option<Value> {
    if !cfg.announce_enabled {
        return None;
    }
    let name = announce_name(cfg)?;

    let mut d = Map::new();
    d.insert("name".into(), Value::String(name));
    d.insert(
        "publicKey".into(),
        Value::String(public_key_hex.to_string()),
    );
    d.insert("timestamp".into(), Value::from(now_ms));
    d.insert("ttl".into(), Value::from(ttl_secs(cfg)));
    d.insert(
        "version".into(),
        Value::String(clip(env!("CARGO_PKG_VERSION"), 32)),
    );

    let slug_cfg = cfg.announce_slug.trim();
    if !slug_cfg.is_empty() {
        d.insert("slug".into(), Value::String(slug_cfg.to_string()));
    }
    let sysop = cfg.announce_sysop.trim();
    if !sysop.is_empty() {
        d.insert("sysop".into(), Value::String(clip(sysop, MAX_SYSOP)));
    }
    if let Some(text) = description(cfg) {
        d.insert("description".into(), Value::String(text));
    }

    let listeners = listeners(cfg);
    if !listeners.is_empty() {
        d.insert(
            "listeners".into(),
            Value::Array(listeners.into_iter().map(Value::String).collect()),
        );
    }
    let endpoints = endpoints(cfg);
    if !endpoints.is_empty() {
        d.insert("endpoints".into(), Value::Object(endpoints));
    }

    Some(Value::Object(d))
}

/// The announce interval, clamped into what the glass protocol accepts.
fn ttl_secs(cfg: &ServerConfig) -> u32 {
    cfg.announce_ttl_secs.clamp(TTL_MIN_SECS, TTL_MAX_SECS)
}

/// The listing blurb: the explicit setting, else the welcome ticker (already a
/// one-liner written for strangers), else nothing.
fn description(cfg: &ServerConfig) -> Option<String> {
    for candidate in [&cfg.announce_description, &cfg.welcome_ticker] {
        let text = candidate.trim();
        if !text.is_empty() {
            return Some(clip(text, MAX_DESCRIPTION));
        }
    }
    None
}

/// Protocol tokens for the surfaces actually switched on, in a fixed order so
/// the signed bytes are deterministic for a given config.
fn listeners(cfg: &ServerConfig) -> Vec<String> {
    let mut out = vec!["quic".to_string()];
    if !cfg.ws_public_url.trim().is_empty() {
        out.push("ws".into());
    }
    for (on, tag) in [
        (cfg.telnet_enabled, "telnet"),
        (cfg.hotline_enabled, "hotline"),
        (cfg.finger_enabled, "finger"),
        (cfg.radio_enabled, "radio"),
        (cfg.nntp_enabled, "nntp"),
    ] {
        if on {
            out.push(tag.into());
        }
    }
    out.truncate(MAX_LISTENERS);
    out
}

/// Dialable URIs per protocol. Only surfaces whose *public* address we actually
/// know: the QUIC port under `advertise_host`, and the WebSocket proxy URL if
/// one is configured. A backend bind address can't reveal its external scheme,
/// host or port, so it is never guessed.
fn endpoints(cfg: &ServerConfig) -> Map<String, Value> {
    let mut m = Map::new();
    let host = cfg.advertise_host.trim();
    if !host.is_empty() {
        m.insert(
            "quic".into(),
            Value::String(format!("quic://{host}:{}", cfg.quic_addr.port())),
        );
    }
    let ws = cfg.ws_public_url.trim();
    if !ws.is_empty() {
        m.insert("ws".into(), Value::String(ws.to_string()));
    }
    m
}

/// The full `{descriptor, signature}` body to POST, signed with `key`.
pub fn signed_body(cfg: &ServerConfig, key: &IdentityKey, now_ms: i64) -> Option<String> {
    let public_hex = hex::encode(key.public().0);
    let descriptor = descriptor(cfg, &public_hex, now_ms)?;
    let signature = key.sign(canonical_json(&descriptor).as_bytes());

    let mut body = Map::new();
    body.insert("descriptor".into(), descriptor);
    body.insert("signature".into(), Value::String(hex::encode(signature.0)));
    serde_json::to_string(&Value::Object(body)).ok()
}

/// Normalize a tracker config entry into the announce endpoint URL.
///
/// Accepts `tracker.rabbit.direct`, `tracker.rabbit.direct:8443`,
/// `https://tracker.rabbit.direct`, or a full path. A bare host gets HTTPS:
/// an announce carries a signature over a public descriptor, but downgrading a
/// coordinator to plaintext by omission is not a decision config should make
/// silently.
pub fn announce_url(entry: &str) -> Option<String> {
    let e = entry.trim().trim_end_matches('/');
    if e.is_empty() {
        return None;
    }
    if e.contains("://") {
        return Some(if e.contains("/api/") {
            e.to_string()
        } else {
            format!("{e}/api/announce")
        });
    }
    Some(format!("https://{e}/api/announce"))
}

/// POST one announce and return the tracker's HTTP status.
async fn post_announce(url: &str, body: &str) -> Result<u16> {
    let target = FeedUrl::parse(url)?;
    let request = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: rabbithole-burrow/{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        target.path,
        target.host_header(),
        env!("CARGO_PKG_VERSION"),
        body.len(),
        body,
    );
    let raw = tokio::time::timeout(ANNOUNCE_TIMEOUT, exchange(&target, request.as_bytes()))
        .await
        .map_err(|_| anyhow!("announce to {url} timed out after {ANNOUNCE_TIMEOUT:?}"))??;
    let resp = parse_http_response(&raw)?;
    if !(200..300).contains(&resp.status) {
        let detail = String::from_utf8_lossy(&resp.body);
        bail!(
            "{url} answered {} — {}",
            resp.status,
            clip(detail.trim(), 200)
        );
    }
    Ok(resp.status)
}

/// The POST path, exposed for the integration test in `tests/e2e_announce.rs`
/// so it exercises the shipping request builder rather than a copy of it.
#[doc(hidden)]
pub async fn post_announce_for_test(url: &str, body: &str) -> Result<u16> {
    post_announce(url, body).await
}

/// Announce to every configured tracker once. Each is independent: one
/// coordinator being down must not cost you a listing on the others.
async fn announce_round(shared: &Arc<Shared>) {
    let (body, trackers) = {
        let cfg = shared.config.read();
        let key = IdentityKey::from_seed(&shared.server_signing_seed);
        match signed_body(&cfg, &key, now_unix_millis()) {
            Some(b) => (b, cfg.announce_trackers.clone()),
            None => return,
        }
    };

    for entry in trackers {
        let Some(url) = announce_url(&entry) else {
            continue;
        };
        match post_announce(&url, &body).await {
            Ok(_) => tracing::debug!(tracker = %url, "announced"),
            // A tracker we can't reach is normal weather, not an incident: the
            // burrow keeps serving and the next round tries again.
            Err(e) => tracing::warn!(tracker = %url, error = %e, "announce failed"),
        }
    }
}

/// Spawn the announce loop. Re-reads config every round, so `ctl config set
/// announce_enabled false` takes effect without a restart — and the `noindex`
/// tag in the descriptor flips with it.
pub fn spawn_announce(shared: Arc<Shared>) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            announce_round(&shared).await;
            let ttl = {
                let cfg = shared.config.read();
                if cfg.announce_enabled {
                    ttl_secs(&cfg)
                } else {
                    // Announcing is off. Idle at the floor rather than exiting,
                    // so turning it back on doesn't need a restart.
                    TTL_MIN_SECS
                }
            };
            tokio::time::sleep(Duration::from_secs(u64::from(ttl))).await;
        }
    })
}

fn now_unix_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ServerConfig {
        let mut c = ServerConfig {
            name: "Wonderland BBS".into(),
            advertise_host: "wonderland.example".into(),
            ..Default::default()
        };
        c.quic_addr = "0.0.0.0:4653".parse().unwrap();
        c
    }

    #[test]
    fn canonical_json_sorts_keys_at_every_level() {
        let v: Value = serde_json::from_str(
            r#"{"z":1,"a":{"y":[3,{"q":1,"b":2}],"b":"x"},"m":null,"c":true}"#,
        )
        .unwrap();
        assert_eq!(
            canonical_json(&v),
            r#"{"a":{"b":"x","y":[3,{"b":2,"q":1}]},"c":true,"m":null,"z":1}"#
        );
    }

    #[test]
    fn canonical_json_preserves_array_order_and_escapes_strings() {
        // Arrays are ordered data, not sets — sorting them would change what
        // the descriptor says (a listener list is a list).
        let v: Value = serde_json::from_str(r#"{"l":["quic","ws","telnet"]}"#).unwrap();
        assert_eq!(canonical_json(&v), r#"{"l":["quic","ws","telnet"]}"#);

        // The signed bytes must survive anything a sysop can type into a
        // description: quotes, newlines, and non-ASCII.
        let v = serde_json::json!({ "d": "a \"quoted\" line\nwith ünïcödé" });
        let out = canonical_json(&v);
        assert_eq!(
            serde_json::from_str::<Value>(&out).unwrap(),
            v,
            "round-trips through a real JSON parser"
        );
        assert!(!out.contains('\n'), "the literal newline is escaped");
    }

    #[test]
    fn canonical_json_has_no_insignificant_whitespace() {
        let v = serde_json::json!({"a": 1, "b": [1, 2], "c": {"d": "e"}});
        let out = canonical_json(&v);
        // Whitespace only ever appears inside a string value.
        assert!(!out.contains(' '), "{out}");
    }

    #[test]
    fn a_burrow_with_no_advertise_host_announces_nothing() {
        // We would be listing an address we cannot state. Better to be
        // undiscoverable than to be discoverable and unreachable.
        let mut c = cfg();
        c.advertise_host = String::new();
        assert!(descriptor(&c, "aa", 1).is_none());
        assert!(announce_name(&c).is_none());
    }

    #[test]
    fn opting_out_announces_nothing() {
        let mut c = cfg();
        c.announce_enabled = false;
        assert!(descriptor(&c, "aa", 1).is_none());
        assert!(
            signed_body(&c, &IdentityKey::from_seed(&[7u8; 32]), 1).is_none(),
            "and there is nothing to POST"
        );
    }

    #[test]
    fn the_name_is_handle_at_host() {
        let mut c = cfg();
        assert_eq!(
            announce_name(&c).unwrap(),
            "wonderland-bbs@wonderland.example",
            "an unconfigured burrow still announces a recognizable handle"
        );
        c.announce_sysop = "alice".into();
        assert_eq!(announce_name(&c).unwrap(), "alice@wonderland.example");
    }

    #[test]
    fn slugs_collapse_punctuation_without_leading_or_trailing_dashes() {
        assert_eq!(slug("  The Rabbit's *Hole*  "), "the-rabbit-s-hole");
        assert_eq!(slug("!!!"), "");
        assert_eq!(slug("A1"), "a1");
    }

    #[test]
    fn the_descriptor_states_only_endpoints_we_actually_know() {
        let mut c = cfg();
        let d = descriptor(&c, "ab12", 1_700_000_000_000).unwrap();
        assert_eq!(d["endpoints"]["quic"], "quic://wonderland.example:4653");
        assert!(
            d["endpoints"].get("ws").is_none(),
            "a backend bind address cannot reveal a public ws URL"
        );
        assert_eq!(d["listeners"], serde_json::json!(["quic"]));

        c.ws_public_url = "wss://wonderland.example/rhp".into();
        c.telnet_enabled = true;
        let d = descriptor(&c, "ab12", 1).unwrap();
        assert_eq!(d["endpoints"]["ws"], "wss://wonderland.example/rhp");
        assert_eq!(d["listeners"], serde_json::json!(["quic", "ws", "telnet"]));
    }

    #[test]
    fn optional_fields_are_omitted_rather_than_sent_empty() {
        let d = descriptor(&cfg(), "ab12", 1).unwrap();
        for absent in ["slug", "sysop", "description"] {
            assert!(d.get(absent).is_none(), "{absent} should be omitted");
        }
        for required in ["name", "publicKey", "timestamp", "ttl"] {
            assert!(d.get(required).is_some(), "{required} is required");
        }
    }

    #[test]
    fn the_description_falls_back_to_the_welcome_ticker() {
        let mut c = cfg();
        c.welcome_ticker = "Open since 1994. Be kind.".into();
        assert_eq!(
            descriptor(&c, "ab", 1).unwrap()["description"],
            c.welcome_ticker
        );

        c.announce_description = "The tea party never ended.".into();
        assert_eq!(
            descriptor(&c, "ab", 1).unwrap()["description"],
            "The tea party never ended.",
            "the explicit setting wins"
        );
    }

    #[test]
    fn over_long_text_is_truncated_rather_than_rejected_wholesale() {
        // The glass rejects an over-long field outright, which would cost the
        // whole listing. A clipped description beats no listing.
        let mut c = cfg();
        c.announce_description = "é".repeat(400);
        let d = descriptor(&c, "ab", 1).unwrap();
        let got = d["description"].as_str().unwrap();
        assert_eq!(got.chars().count(), MAX_DESCRIPTION);
        assert!(std::str::from_utf8(got.as_bytes()).is_ok(), "no split char");
    }

    #[test]
    fn the_ttl_is_clamped_into_what_the_protocol_accepts() {
        let mut c = cfg();
        c.announce_ttl_secs = 1;
        assert_eq!(descriptor(&c, "ab", 1).unwrap()["ttl"], TTL_MIN_SECS);
        c.announce_ttl_secs = 99_999;
        assert_eq!(descriptor(&c, "ab", 1).unwrap()["ttl"], TTL_MAX_SECS);
    }

    #[test]
    fn the_signature_verifies_over_the_canonical_bytes() {
        // The whole contract: a tracker re-canonicalizes the descriptor it
        // received and checks the signature against the key inside it.
        let key = IdentityKey::from_seed(&[42u8; 32]);
        let body: Value =
            serde_json::from_str(&signed_body(&cfg(), &key, 1_700_000_000_000).unwrap()).unwrap();

        let descriptor = &body["descriptor"];
        let sig_hex = body["signature"].as_str().unwrap();
        assert_eq!(sig_hex.len(), 128, "64 signature bytes, hex");

        let announced_key = descriptor["publicKey"].as_str().unwrap();
        assert_eq!(
            announced_key,
            hex::encode(key.public().0),
            "the descriptor names the key that signed it"
        );

        // Rebuild the verifier from the *announced* hex, the way a tracker
        // that has only the JSON would, rather than from the key we signed
        // with.
        let vk =
            rabbithole_identity::PublicKey(hex::decode(announced_key).unwrap().try_into().unwrap());
        let sig = rabbithole_identity::Signature(hex::decode(sig_hex).unwrap().try_into().unwrap());
        assert!(
            vk.verify(canonical_json(descriptor).as_bytes(), &sig),
            "verifies over the canonical bytes"
        );

        // And it is genuinely bound to the content: change one field and the
        // signature must stop verifying.
        let mut tampered = descriptor.clone();
        tampered["name"] = Value::String("evil@elsewhere".into());
        assert!(
            !vk.verify(canonical_json(&tampered).as_bytes(), &sig),
            "a rewritten descriptor does not verify"
        );
    }

    #[test]
    fn tracker_entries_normalize_to_an_announce_url() {
        assert_eq!(
            announce_url("tracker.rabbit.direct").unwrap(),
            "https://tracker.rabbit.direct/api/announce",
            "a bare host gets HTTPS, never a silent plaintext downgrade"
        );
        assert_eq!(
            announce_url(" glass.example:8443/ ").unwrap(),
            "https://glass.example:8443/api/announce"
        );
        assert_eq!(
            announce_url("https://glass.example").unwrap(),
            "https://glass.example/api/announce"
        );
        assert_eq!(
            announce_url("http://127.0.0.1:3000/api/announce").unwrap(),
            "http://127.0.0.1:3000/api/announce",
            "an explicit scheme and path are honored — local coordinators exist"
        );
        assert!(announce_url("   ").is_none());
    }
}
