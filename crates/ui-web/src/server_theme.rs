//! Client-side application of a server-published theme bundle (PLAN §9.11).
//!
//! Server theming **layers on top of** the user's pack + light/dark choice:
//! the operator's accent and any structured `--rh-*` tokens overlay the
//! resolved built-in pack, nudging the look without replacing it, and the user
//! can switch it off entirely. The bundle travels as a
//! [`rabbithole_proto::welcome::ThemeBundle`] the server has already validated
//! against a closed grammar plus WCAG contrast rails and signed. Live clients
//! independently verify the signature and token grammar before mapping tokens
//! onto the CSS-variable maps [`crate::packs`] emits.
//!
//! Everything here is pure and host-tested. Because the server grammar is a
//! **subset** of the client's token set (the six colour roles, `--rh-bg-image`,
//! and the ten metric tokens — never the elevation/type-scale extras the
//! redesign added), overlaying a bundle can only ever set keys the built-in
//! pack already defines, so a partial bundle just replaces what it names and
//! leaves the rest of the pack intact.

use rabbithole_proto::welcome::{ThemeBundle, ThemeReply};

use crate::packs::{PackTokens, VarMap};

/// A transport update. Unchanged avoids rebuilding the root style for repeated
/// replies; Apply(None) removes an absent, empty, or rejected server theme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeUpdate {
    Unchanged,
    Apply(Option<ServerOverlay>),
}

/// One live connection's verified theme cache. Never shared across burrows or
/// persisted: the handshake binds its key, and a reconnect resets that binding.
/// Only a digest is retained; the session signal owns the rendered overlay.
#[derive(Debug, Default)]
pub struct ThemeCache {
    server_key: Option<[u8; 32]>,
    content_hash: Option<[u8; 32]>,
}

impl ThemeCache {
    /// Bind a fresh handshake (or forget it on disconnect). The caller also
    /// clears its rendered overlay whenever it resets the connection.
    pub fn reset(&mut self, server_key: Option<[u8; 32]>) {
        self.server_key = server_key;
        self.content_hash = None;
    }

    /// Clear on NotFound, a refused fetch, or an invalid response. Forget the
    /// digest too, so re-enabling a theme can reapply the same valid content.
    pub fn clear(&mut self) -> ThemeUpdate {
        self.content_hash = None;
        ThemeUpdate::Apply(None)
    }

    /// Verify exact signed bytes before decoding or consulting the cache.
    /// Authentication of a server is not permission to inject arbitrary CSS:
    /// independently enforce the server token grammar at the rendering edge.
    pub fn accept(&mut self, reply: &ThemeReply) -> ThemeUpdate {
        // A valid bundle carries at most 64 KiB inline art, 64 small icon
        // references, and 24 short tokens. Leave room for their wire overhead.
        if reply.bundle.len() > 128 * 1024 {
            return self.clear();
        }
        let bundle = self
            .server_key
            .as_ref()
            .and_then(|key| rabbithole_core::theme::verify_theme_bundle(reply, key));
        let Some(bundle) = bundle.filter(safe_bundle) else {
            return self.clear();
        };
        let hash = *blake3::hash(&reply.bundle).as_bytes();
        if self.content_hash == Some(hash) {
            return ThemeUpdate::Unchanged;
        }
        self.content_hash = Some(hash);
        let overlay = ServerOverlay::from_bundle(&bundle);
        ThemeUpdate::Apply((!overlay.is_empty()).then_some(overlay))
    }
}

/// The wire bundle is signed by a remote operator. Only closed token names and
/// values may reach a style attribute, even if that operator bypassed their
/// server's normal validation (for example, by editing its config directly).
fn safe_bundle(bundle: &ThemeBundle) -> bool {
    use crate::theme_editor::{is_css_length, parse_hex, SERVER_COLOR_VARS, SERVER_LENGTH_VARS};
    let valid_map = |tokens: &[(String, String)], shared: bool| {
        let limit = if shared {
            SERVER_LENGTH_VARS.len()
        } else {
            SERVER_COLOR_VARS.len() + 1
        };
        if tokens.len() > limit {
            return false;
        }
        let mut seen = std::collections::BTreeSet::new();
        tokens.iter().all(|(name, value)| {
            if value.len() > 64 || !seen.insert(name) {
                return false;
            }
            if shared {
                SERVER_LENGTH_VARS.contains(&name.as_str()) && is_css_length(value)
            } else if name == "--rh-bg-image" {
                value.trim() == "none"
            } else {
                SERVER_COLOR_VARS.contains(&name.as_str()) && parse_hex(value).is_some()
            }
        })
    };
    bundle.name.len() <= 64
        && bundle
            .logo_ansi
            .as_ref()
            .is_none_or(|art| art.len() <= 64 * 1024)
        && bundle.icons.len() <= 64
        && bundle.icons.iter().all(|(name, _)| name.len() <= 64)
        && valid_map(&bundle.tokens_light, false)
        && valid_map(&bundle.tokens_dark, false)
        && valid_map(&bundle.tokens_shared, true)
}

/// The design-token overlay a server theme contributes: a display name plus
/// partial per-mode colour maps and a partial shared (metric) map. Only the
/// keys the bundle actually sets are present.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ServerOverlay {
    /// The theme's display name (usually the server name).
    pub name: String,
    /// Partial light-mode colour overrides (`--rh-*` → value).
    pub light: VarMap,
    /// Partial dark-mode colour overrides.
    pub dark: VarMap,
    /// Partial mode-independent metric overrides.
    pub shared: VarMap,
}

impl ServerOverlay {
    /// Build an overlay from a validated bundle. The legacy `accent_rgb`
    /// field, when present, seeds `--rh-accent` **and** `--rh-focus` in both
    /// modes — but only where the structured per-mode maps don't already name
    /// them, so an explicit per-mode accent always wins. Structured tokens are
    /// copied verbatim (the server already grammar-checked them).
    pub fn from_bundle(b: &ThemeBundle) -> ServerOverlay {
        let mut light: VarMap = b.tokens_light.iter().cloned().collect();
        let mut dark: VarMap = b.tokens_dark.iter().cloned().collect();
        let shared: VarMap = b.tokens_shared.iter().cloned().collect();
        if let Some([r, g, bl]) = b.accent_rgb {
            let hex = format!("#{r:02x}{g:02x}{bl:02x}");
            for map in [&mut light, &mut dark] {
                map.entry("--rh-accent".into())
                    .or_insert_with(|| hex.clone());
                map.entry("--rh-focus".into())
                    .or_insert_with(|| hex.clone());
            }
        }
        ServerOverlay {
            name: b.name.clone(),
            light,
            dark,
            shared,
        }
    }

    /// Whether the overlay contributes any token (an accent-less, token-less
    /// bundle is a no-op the caller can drop).
    pub fn is_empty(&self) -> bool {
        self.light.is_empty() && self.dark.is_empty() && self.shared.is_empty()
    }

    /// Overlay this server theme onto `base`, returning a full [`PackTokens`]:
    /// the base pack with the server's keys replaced. Keys the bundle omits
    /// keep the base's value, so a partial bundle only nudges what it names.
    /// A non-empty `name` renames the resolved pack.
    pub fn over(&self, base: &PackTokens) -> PackTokens {
        let mut out = base.clone();
        overlay_into(&mut out.light, &self.light);
        overlay_into(&mut out.dark, &self.dark);
        overlay_into(&mut out.shared, &self.shared);
        if !self.name.is_empty() {
            out.name = self.name.clone();
        }
        out
    }
}

/// Insert every `from` entry into `into`, replacing any existing value.
fn overlay_into(into: &mut VarMap, from: &VarMap) {
    for (k, v) in from {
        into.insert(k.clone(), v.clone());
    }
}

/// Browser-side persistence of the user's server-theming opt-out (`wasm32`
/// only) — the untestable `localStorage` edge over the pure overlay above, in
/// the same style as [`crate::theme_css::storage`].
#[cfg(target_arch = "wasm32")]
pub mod storage {
    /// `localStorage` key the opt-out is stored under.
    const KEY: &str = "rh-server-theme-disabled";

    /// Whether the user has switched server theming off (default `false` —
    /// absent or any non-`"1"` value means server themes apply).
    pub fn load_disabled() -> bool {
        web_sys::window()
            .and_then(|w| w.local_storage().ok().flatten())
            .and_then(|s| s.get_item(KEY).ok().flatten())
            .as_deref()
            == Some("1")
    }

    /// Persist the opt-out (best-effort; storage may be unavailable).
    pub fn save_disabled(disabled: bool) {
        if let Some(Ok(Some(storage))) = web_sys::window().map(|w| w.local_storage()) {
            let _ = storage.set_item(KEY, if disabled { "1" } else { "0" });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabbithole_core::theme::{Mode, ThemePack};

    fn bundle() -> ThemeBundle {
        let mut b = ThemeBundle::new("Wonderland");
        // Per-mode accents + a shared metric — the shape a serious bundle has.
        b.tokens_light = vec![("--rh-accent".into(), "#a34700".into())];
        b.tokens_dark = vec![("--rh-accent".into(), "#ff8800".into())];
        b.tokens_shared = vec![("--rh-radius".into(), "0".into())];
        b
    }

    fn signed(bundle: &ThemeBundle, seed: u8) -> ThemeReply {
        use ed25519_dalek::Signer;
        let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let bytes = postcard::to_allocvec(bundle).unwrap();
        let signature = key.sign(&bytes).to_bytes().to_vec();
        ThemeReply::new(bytes, signature)
    }

    fn bound_cache(seed: u8) -> ThemeCache {
        let mut cache = ThemeCache::default();
        cache.reset(Some(
            ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
                .verifying_key()
                .to_bytes(),
        ));
        cache
    }

    #[test]
    fn live_cache_verifies_then_deduplicates_exact_content() {
        let reply = signed(&bundle(), 1);
        let mut cache = bound_cache(1);
        assert_eq!(
            cache.accept(&reply),
            ThemeUpdate::Apply(Some(ServerOverlay::from_bundle(&bundle())))
        );
        assert_eq!(cache.accept(&reply), ThemeUpdate::Unchanged);

        // A cache hit is never allowed to bypass signature verification.
        let mut bad = reply.clone();
        bad.signature[0] ^= 1;
        assert_eq!(cache.accept(&bad), ThemeUpdate::Apply(None));
        assert!(matches!(cache.accept(&reply), ThemeUpdate::Apply(Some(_))));

        let mut changed = bundle();
        changed.tokens_dark[0].1 = "#aabbcc".into();
        assert_eq!(
            cache.accept(&signed(&changed, 1)),
            ThemeUpdate::Apply(Some(ServerOverlay::from_bundle(&changed)))
        );
    }

    #[test]
    fn live_cache_binds_the_handshake_and_forgets_on_reconnect() {
        let reply = signed(&bundle(), 1);
        let mut unbound = ThemeCache::default();
        assert_eq!(unbound.accept(&reply), ThemeUpdate::Apply(None));
        let mut cache = bound_cache(1);
        assert!(matches!(cache.accept(&reply), ThemeUpdate::Apply(Some(_))));
        cache.reset(Some(
            ed25519_dalek::SigningKey::from_bytes(&[2; 32])
                .verifying_key()
                .to_bytes(),
        ));
        assert_eq!(cache.accept(&reply), ThemeUpdate::Apply(None));
        assert!(matches!(
            cache.accept(&signed(&bundle(), 2)),
            ThemeUpdate::Apply(Some(_))
        ));
        cache.reset(None);
        assert_eq!(
            cache.accept(&signed(&bundle(), 2)),
            ThemeUpdate::Apply(None)
        );
    }

    #[test]
    fn absent_and_empty_themes_clear_and_same_content_can_return() {
        let reply = signed(&bundle(), 1);
        let mut cache = bound_cache(1);
        assert!(matches!(cache.accept(&reply), ThemeUpdate::Apply(Some(_))));
        assert_eq!(cache.clear(), ThemeUpdate::Apply(None));
        assert!(matches!(cache.accept(&reply), ThemeUpdate::Apply(Some(_))));
        assert_eq!(
            cache.accept(&signed(&ThemeBundle::new("Empty"), 1)),
            ThemeUpdate::Apply(None)
        );
        assert!(matches!(cache.accept(&reply), ThemeUpdate::Apply(Some(_))));
    }

    #[test]
    fn malformed_or_tampered_signed_payloads_remove_old_theme() {
        use ed25519_dalek::Signer;
        let key = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let good = signed(&bundle(), 1);
        let mut tampered = good.clone();
        tampered.bundle[0] ^= 1;
        let bytes = vec![255];
        let malformed = ThemeReply::new(bytes.clone(), key.sign(&bytes).to_bytes().to_vec());
        let mut oversized = good.clone();
        oversized.bundle.resize(128 * 1024 + 1, 0);
        for bad in [
            tampered,
            malformed,
            oversized,
            ThemeReply::new(good.bundle.clone(), vec![0; 63]),
        ] {
            let mut cache = bound_cache(1);
            assert!(matches!(cache.accept(&good), ThemeUpdate::Apply(Some(_))));
            assert_eq!(cache.accept(&bad), ThemeUpdate::Apply(None));
        }
    }

    #[test]
    fn valid_signature_cannot_authorize_unsafe_css() {
        let mut bad_bundles = Vec::new();
        for (name, value) in [
            ("--rh-accent", "#fff;position:fixed"),
            ("--rh-accent", "#ééé"),
            ("--rh-bg-image", "url(https://example.invalid/track)"),
            ("--rh-bg-image", "none}body{display:none"),
            ("--rh-unknown", "#fff"),
        ] {
            let mut bad = bundle();
            bad.tokens_light = vec![(name.into(), value.into())];
            bad_bundles.push(bad);
        }
        for (name, value) in [
            ("--rh-radius", "var(--attacker)"),
            ("--rh-radius", "NaNpx"),
            ("--rh-radius", "-1px"),
            ("--rh-radius", "1px;color:red"),
            ("--rh-font-sans", "sans-serif"),
        ] {
            let mut bad = bundle();
            bad.tokens_shared = vec![(name.into(), value.into())];
            bad_bundles.push(bad);
        }
        let mut duplicate = bundle();
        duplicate
            .tokens_light
            .push(duplicate.tokens_light[0].clone());
        bad_bundles.push(duplicate);
        let mut huge_value = bundle();
        huge_value.tokens_shared[0].1 = format!("{}1px", "0".repeat(64));
        bad_bundles.push(huge_value);
        for bad in bad_bundles {
            let mut cache = bound_cache(1);
            assert!(matches!(
                cache.accept(&signed(&bundle(), 1)),
                ThemeUpdate::Apply(Some(_))
            ));
            assert_eq!(
                cache.accept(&signed(&bad, 1)),
                ThemeUpdate::Apply(None),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn different_burrows_keep_independent_verified_caches() {
        let mut a = bound_cache(1);
        let mut b = bound_cache(2);
        let first = signed(&bundle(), 1);
        let mut other = bundle();
        other.name = "Other burrow".into();
        let second = signed(&other, 2);
        assert!(matches!(a.accept(&first), ThemeUpdate::Apply(Some(_))));
        assert_eq!(b.accept(&first), ThemeUpdate::Apply(None));
        assert_eq!(
            b.accept(&second),
            ThemeUpdate::Apply(Some(ServerOverlay::from_bundle(&other)))
        );
        assert_eq!(a.accept(&first), ThemeUpdate::Unchanged);
    }

    #[test]
    fn from_bundle_copies_tokens_verbatim() {
        let o = ServerOverlay::from_bundle(&bundle());
        assert_eq!(o.name, "Wonderland");
        assert_eq!(o.light["--rh-accent"], "#a34700");
        assert_eq!(o.dark["--rh-accent"], "#ff8800");
        assert_eq!(o.shared["--rh-radius"], "0");
        assert!(!o.is_empty());
    }

    #[test]
    fn accent_rgb_seeds_accent_and_focus_in_both_modes() {
        let mut b = ThemeBundle::new("Blaze");
        b.accent_rgb = Some([0x2b, 0x63, 0xd8]);
        let o = ServerOverlay::from_bundle(&b);
        for map in [&o.light, &o.dark] {
            assert_eq!(map["--rh-accent"], "#2b63d8");
            assert_eq!(map["--rh-focus"], "#2b63d8");
        }
    }

    #[test]
    fn explicit_per_mode_accent_wins_over_accent_rgb() {
        let mut b = bundle();
        b.accent_rgb = Some([0x11, 0x22, 0x33]); // legacy single accent
        let o = ServerOverlay::from_bundle(&b);
        // The structured per-mode accents are kept; accent_rgb only fills the
        // gap (here, --rh-focus, which the token maps didn't set).
        assert_eq!(o.light["--rh-accent"], "#a34700");
        assert_eq!(o.dark["--rh-accent"], "#ff8800");
        assert_eq!(o.light["--rh-focus"], "#112233");
        assert_eq!(o.dark["--rh-focus"], "#112233");
    }

    #[test]
    fn empty_bundle_is_empty() {
        assert!(ServerOverlay::from_bundle(&ThemeBundle::new("Bare")).is_empty());
    }

    #[test]
    fn over_replaces_named_keys_and_keeps_the_rest() {
        let base = PackTokens::builtin(ThemePack::Clean);
        let themed = ServerOverlay::from_bundle(&bundle()).over(&base);

        // Named keys are replaced, per mode.
        assert_eq!(themed.light["--rh-accent"], "#a34700");
        assert_eq!(themed.dark["--rh-accent"], "#ff8800");
        assert_eq!(themed.shared["--rh-radius"], "0");
        // Unnamed keys keep the base pack's values — including the redesign's
        // elevation/type-scale extras the server grammar can't touch.
        assert_eq!(themed.light["--rh-bg"], base.light["--rh-bg"]);
        assert_eq!(themed.dark["--rh-text"], base.dark["--rh-text"]);
        assert_eq!(themed.shared["--rh-shadow-2"], base.shared["--rh-shadow-2"]);
        assert_eq!(themed.shared["--rh-font-2xl"], base.shared["--rh-font-2xl"]);
        // The key set is unchanged (overlay never adds or drops variables).
        assert_eq!(
            themed.light.keys().collect::<Vec<_>>(),
            base.light.keys().collect::<Vec<_>>()
        );
        assert_eq!(themed.name, "Wonderland");
        // And it renders (the accent actually reaches the style string).
        assert!(themed
            .style_for(Mode::Dark)
            .contains("--rh-accent:#ff8800;"));
    }

    #[test]
    fn over_onto_different_base_packs_layers_on_each() {
        // The same bundle over Retro keeps Retro's monospace body + scanlines,
        // only swapping the accent — proving it layers, not replaces.
        let retro = PackTokens::builtin(ThemePack::Retro);
        let themed = ServerOverlay::from_bundle(&bundle()).over(&retro);
        assert_eq!(themed.dark["--rh-accent"], "#ff8800");
        assert_eq!(
            themed.shared["--rh-font-sans"],
            retro.shared["--rh-font-sans"]
        );
        assert_eq!(themed.dark["--rh-bg-image"], retro.dark["--rh-bg-image"]);
    }
}
