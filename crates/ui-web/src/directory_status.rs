//! Directory proof copy and source-bound offline persistence.

use rabbithole_directory::{Binding, DirectoryVerification};

pub fn label(status: &DirectoryVerification) -> &'static str {
    match status {
        DirectoryVerification::Unverified => "Unverified",
        DirectoryVerification::Invalid => "Verification failed",
        DirectoryVerification::Future => "Future-dated listing",
        DirectoryVerification::Stale(_) => "Signed listing expired",
        DirectoryVerification::Verified(proof) => match proof.binding {
            Binding::Endpoint => "Signature verified",
            Binding::Address => "Address signed",
        },
    }
}

pub fn explanation(status: &DirectoryVerification) -> &'static str {
    match status {
        DirectoryVerification::Unverified => "This listing has no complete signed proof. Availability and uptime are directory observations.",
        DirectoryVerification::Invalid => "The proof could not verify this name and address. Refresh the listing before relying on it.",
        DirectoryVerification::Future => "The signed timestamp is ahead of this device's clock. Check the clock and refresh the listing.",
        DirectoryVerification::Stale(_) => "The signature verifies, but the listing has expired. Refresh to check for a current statement.",
        DirectoryVerification::Verified(proof) => match proof.binding {
            Binding::Endpoint => "This exact connection endpoint appears in the server's signed listing. Availability and uptime are separate directory observations.",
            Binding::Address => "This IP address and port appear in the server's signed listing. This legacy proof does not certify the connection protocol. Availability and uptime are separate directory observations.",
        },
    }
}

pub fn signing_key(status: &DirectoryVerification) -> Option<&str> {
    match status {
        DirectoryVerification::Verified(proof) | DirectoryVerification::Stale(proof) => {
            Some(&proof.public_key)
        }
        _ => None,
    }
}

pub const UNKNOWN_NATIVE_SOURCE: &str = "native tracker (source unavailable)";

pub fn native_source_valid(source: &str) -> bool {
    !source.is_empty()
        && source.len() <= 240
        && source
            .chars()
            .all(|c| !c.is_whitespace() && !c.is_control())
        && source != UNKNOWN_NATIVE_SOURCE
}

#[cfg(any(target_arch = "wasm32", test))]
#[derive(serde::Serialize, serde::Deserialize)]
struct NativeCache {
    source: String,
    data: String,
}

#[cfg(any(target_arch = "wasm32", test))]
fn decode_native(text: &str) -> Option<rabbithole_directory::cache::CachedListing> {
    use rabbithole_directory::cache;
    if text.len() > cache::MAX_CACHE_BYTES * 2 + 1024 {
        return None;
    }
    let saved: NativeCache = serde_json::from_str(text).ok()?;
    if !native_source_valid(&saved.source) {
        return None;
    }
    let inner = cache::decode(&format!("native:{}", saved.source), &saved.data)?;
    (inner.source_label == saved.source).then_some(inner)
}

#[cfg(target_arch = "wasm32")]
pub mod storage {
    use super::{decode_native, native_source_valid, NativeCache};
    use rabbithole_directory::{cache, DirectorySource, LiveListing};

    // Three bounded slots; remote rows never choose a storage namespace.
    const SOURCES: [(&str, &str); 2] = [
        ("rabbithole.directory", "rh.directory.proofs.directory.v1"),
        ("tracker.rabbit.direct", "rh.directory.proofs.tracker.v1"),
    ];
    const NATIVE_KEY: &str = "rh.directory.proofs.native.v1";

    fn store() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok()?
    }

    pub fn remember(listing: &mut LiveListing, now_ms: i64) {
        let Some(storage) = store() else { return };
        let source = listing.source.label();
        if let Some((_, key)) = SOURCES.iter().find(|(known, _)| *known == source) {
            let previous = storage.get_item(key).ok().flatten().and_then(|text| {
                cache::decode(source, &text).filter(|saved| saved.source_label == source)
            });
            if let Some(text) = cache::update(
                source,
                source,
                now_ms,
                &mut listing.servers,
                previous.as_ref(),
            ) {
                let _ = storage.set_item(key, &text);
            }
        } else if matches!(listing.source, DirectorySource::Tracker(_))
            && native_source_valid(source)
        {
            let previous = storage
                .get_item(NATIVE_KEY)
                .ok()
                .flatten()
                .and_then(|text| decode_native(&text))
                .filter(|saved| saved.source_label == source);
            if let Some(data) = cache::update(
                &format!("native:{source}"),
                source,
                now_ms,
                &mut listing.servers,
                previous.as_ref(),
            ) {
                if let Ok(text) = serde_json::to_string(&NativeCache {
                    source: source.into(),
                    data,
                }) {
                    let _ = storage.set_item(NATIVE_KEY, &text);
                }
            }
        }
    }

    pub fn load(now_ms: i64) -> Option<LiveListing> {
        let storage = store()?;
        let native = storage
            .get_item(NATIVE_KEY)
            .ok()
            .flatten()
            .and_then(|text| decode_native(&text));
        let saved = SOURCES
            .iter()
            .filter_map(|(source, key)| {
                let text = storage.get_item(key).ok()??;
                cache::decode(source, &text).filter(|saved| saved.source_label == *source)
            })
            .chain(native)
            .max_by_key(|saved| saved.fetched_at_ms)?;
        // Every rendered row rechecks signed bytes against the current clock.
        let age = now_ms.saturating_sub(saved.fetched_at_ms).max(0) / 60_000;
        let when = if age < 1 {
            "just now".to_string()
        } else if age < 60 {
            format!("{age} min ago")
        } else if age < 1440 {
            format!("{} h ago", age / 60)
        } else {
            format!("{} days ago", age / 1440)
        };
        Some(LiveListing {
            servers: saved.servers,
            source: DirectorySource::Cached(format!("saved {} · {when}", saved.source_label)),
            fallback_reason: Some("No live directory answered.".into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_cache_requires_the_actual_source_and_bound_envelope() {
        let source = "127.0.0.1:5497";
        let data = rabbithole_directory::cache::encode_with_label(
            &format!("native:{source}"),
            source,
            1000,
            &[],
        )
        .unwrap();
        let mut saved = NativeCache {
            source: source.into(),
            data,
        };
        let text = serde_json::to_string(&saved).unwrap();
        assert_eq!(decode_native(&text).unwrap().source_label, source);
        saved.source = "tracker.rabbit.direct:4655".into();
        assert!(decode_native(&serde_json::to_string(&saved).unwrap()).is_none());
        assert!(!native_source_valid(UNKNOWN_NATIVE_SOURCE));
        assert!(!native_source_valid(""));
        assert!(!native_source_valid("bad\nsource"));
    }
}
