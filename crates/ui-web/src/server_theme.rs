//! Client-side application of a server-published theme bundle (PLAN §9.11).
//!
//! Server theming **layers on top of** the user's pack + light/dark choice:
//! the operator's accent and any structured `--rh-*` tokens overlay the
//! resolved built-in pack, nudging the look without replacing it, and the user
//! can switch it off entirely. The bundle travels as a
//! [`rabbithole_proto::welcome::ThemeBundle`] the server has already validated
//! against a closed grammar plus WCAG contrast rails
//! ([`rabbithole_server_core::theme`]) and signed; this module only maps those
//! grammar-checked tokens onto the CSS-variable maps [`crate::packs`] emits.
//!
//! Everything here is pure and host-tested. Because the server grammar is a
//! **subset** of the client's token set (the six colour roles, `--rh-bg-image`,
//! and the ten metric tokens — never the elevation/type-scale extras the
//! redesign added), overlaying a bundle can only ever set keys the built-in
//! pack already defines, so a partial bundle just replaces what it names and
//! leaves the rest of the pack intact.

use rabbithole_proto::welcome::ThemeBundle;

use crate::packs::{PackTokens, VarMap};

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
