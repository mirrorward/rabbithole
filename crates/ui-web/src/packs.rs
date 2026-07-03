//! Theme packs as shareable token sets.
//!
//! A [`PackTokens`] is the complete set of CSS custom properties (`--rh-*`)
//! one theme pack contributes: a colour map per [`Mode`] plus mode-independent
//! `shared` tokens (spacing, radii, typography). The three built-ins mirror
//! [`rabbithole_core::theme::ThemePack`]:
//!
//! - **Clean** — the neutral default the SPA has always shipped.
//! - **Retro** — CP437/BBS aesthetic: classic ANSI palette accents, a
//!   scanline background via a repeating CSS gradient, and a
//!   monospace-forward type scale.
//! - **High Contrast** — WCAG-AAA-leaning black/white tokens with a slightly
//!   larger type scale.
//!
//! ## Token files
//!
//! Packs round-trip through JSON ([`PackTokens::to_tokens_json`] /
//! [`PackTokens::from_tokens_json`]) so a pack can travel as a standalone
//! token file — the seam a future server theme bundle plugs into. Parsing is
//! tamper-tolerant: unknown keys are ignored and missing keys fall back to
//! the Clean built-in, so a truncated or hand-edited file still yields a
//! complete, renderable pack.

use std::collections::BTreeMap;

use rabbithole_core::theme::{Mode, Palette, Rgb, ThemePack};
use serde::{Deserialize, Serialize};

/// `#rrggbb` for a core [`Rgb`].
fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

/// One CSS-variable map: `--rh-*` name → value.
pub type VarMap = BTreeMap<String, String>;

/// The complete token set for one theme pack.
///
/// Serialises as a JSON token file. Deserialisation is tamper-tolerant:
/// unknown keys are ignored (serde's default) and missing fields are taken
/// from the Clean built-in via `#[serde(default)]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PackTokens {
    /// Human-readable pack name.
    pub name: String,
    /// Mode-independent tokens: spacing, radii, typography.
    pub shared: VarMap,
    /// Light-mode colour tokens.
    pub light: VarMap,
    /// Dark-mode colour tokens.
    pub dark: VarMap,
}

impl Default for PackTokens {
    /// The Clean built-in — the base every partial token file is completed
    /// against.
    fn default() -> Self {
        Self::builtin(ThemePack::Clean)
    }
}

/// The colour variables for one mode: the six palette roles plus the
/// pack's background texture (`--rh-bg-image`, `none` for flat packs).
fn colour_vars(p: &Palette, bg_image: &str) -> VarMap {
    let mut m = VarMap::new();
    m.insert("--rh-bg".into(), hex(p.background));
    m.insert("--rh-surface".into(), hex(p.surface));
    m.insert("--rh-text".into(), hex(p.text));
    m.insert("--rh-muted".into(), hex(p.muted));
    m.insert("--rh-accent".into(), hex(p.accent));
    m.insert("--rh-error".into(), hex(p.error));
    m.insert("--rh-bg-image".into(), bg_image.into());
    m
}

/// The mode-independent tokens, parameterised by the pack's type scale and
/// corner rounding.
#[allow(clippy::too_many_arguments)]
fn shared_vars(
    font_sans: &str,
    font_mono: &str,
    font_size: &str,
    font_sm: &str,
    font_xs: &str,
    radius: &str,
    radius_lg: &str,
) -> VarMap {
    let mut m = VarMap::new();
    m.insert("--rh-space-1".into(), ".25rem".into());
    m.insert("--rh-space-2".into(), ".5rem".into());
    m.insert("--rh-space-3".into(), ".75rem".into());
    m.insert("--rh-space-4".into(), "1rem".into());
    m.insert("--rh-space-6".into(), "1.5rem".into());
    m.insert("--rh-radius".into(), radius.into());
    m.insert("--rh-radius-lg".into(), radius_lg.into());
    m.insert("--rh-font-sans".into(), font_sans.into());
    m.insert("--rh-font-mono".into(), font_mono.into());
    m.insert("--rh-font-size".into(), font_size.into());
    m.insert("--rh-font-sm".into(), font_sm.into());
    m.insert("--rh-font-xs".into(), font_xs.into());
    m
}

/// The default sans stack (Clean and High Contrast).
const SANS: &str = "system-ui,-apple-system,'Segoe UI',Roboto,sans-serif";
/// The mono stack; Retro also uses it as its body face.
const MONO: &str = "ui-monospace,SFMono-Regular,Menlo,Consolas,monospace";

/// CRT scanlines: a repeating 3px horizontal gradient over `--rh-bg`.
fn scanlines(alpha: &str) -> String {
    format!(
        "repeating-linear-gradient(0deg,rgba(0,0,0,{alpha}) 0,\
rgba(0,0,0,{alpha}) 1px,transparent 1px,transparent 3px)"
    )
}

impl PackTokens {
    /// The built-in token set for a [`ThemePack`].
    pub fn builtin(pack: ThemePack) -> PackTokens {
        match pack {
            ThemePack::Clean => PackTokens {
                name: "Clean".into(),
                shared: shared_vars(SANS, MONO, "1rem", ".85rem", ".8rem", ".4rem", ".6rem"),
                light: colour_vars(&Palette::builtin(ThemePack::Clean, Mode::Light), "none"),
                dark: colour_vars(&Palette::builtin(ThemePack::Clean, Mode::Dark), "none"),
            },
            // CP437/BBS: monospace body, boxy corners, ANSI-palette accents,
            // and scanlines. Dark is the core Retro palette (dark navy field,
            // amber accent, ANSI bright-red error); light is its paper-mode
            // counterpart with ANSI blue/red accents.
            ThemePack::Retro => PackTokens {
                name: "Retro".into(),
                shared: shared_vars(MONO, MONO, ".95rem", ".85rem", ".8rem", ".15rem", ".25rem"),
                light: colour_vars(
                    &Palette {
                        background: Rgb(0xf2, 0xef, 0xe2),
                        surface: Rgb(0xfb, 0xf9, 0xf0),
                        text: Rgb(0x20, 0x20, 0x30),
                        muted: Rgb(0x60, 0x60, 0x58),
                        accent: Rgb(0x00, 0x00, 0xaa), // ANSI blue
                        error: Rgb(0xaa, 0x00, 0x00),  // ANSI red
                    },
                    &scanlines(".05"),
                ),
                dark: colour_vars(
                    &Palette::builtin(ThemePack::Retro, Mode::Dark),
                    &scanlines(".22"),
                ),
            },
            // WCAG-AAA-leaning: pure black/white fields, no texture, and a
            // slightly larger type scale.
            ThemePack::HighContrast => PackTokens {
                name: "High Contrast".into(),
                shared: shared_vars(SANS, MONO, "1.05rem", ".9rem", ".85rem", ".4rem", ".6rem"),
                light: colour_vars(
                    &Palette::builtin(ThemePack::HighContrast, Mode::Light),
                    "none",
                ),
                dark: colour_vars(
                    &Palette::builtin(ThemePack::HighContrast, Mode::Dark),
                    "none",
                ),
            },
        }
    }

    /// Render the pack's variables for `mode` as `--rh-*:value;…` for a
    /// `style` attribute: the mode's colour map followed by the shared tokens.
    pub fn style_for(&self, mode: Mode) -> String {
        let colours = match mode {
            Mode::Light => &self.light,
            Mode::Dark => &self.dark,
        };
        let mut out = String::new();
        for (name, value) in colours.iter().chain(self.shared.iter()) {
            out.push_str(name);
            out.push(':');
            out.push_str(value);
            out.push(';');
        }
        out
    }

    /// Serialise the pack as a shareable JSON token file.
    pub fn to_tokens_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("string maps always serialise")
    }

    /// Parse a JSON token file, tolerantly.
    ///
    /// Unknown keys are ignored and missing keys default: absent fields fall
    /// back to the Clean built-in wholesale, and any individual variable a
    /// map omits is filled in from Clean, so the result is always a complete
    /// pack. Returns `None` only for malformed JSON.
    pub fn from_tokens_json(json: &str) -> Option<PackTokens> {
        let mut pack: PackTokens = serde_json::from_str(json).ok()?;
        let base = PackTokens::default();
        fill_missing(&mut pack.shared, base.shared);
        fill_missing(&mut pack.light, base.light);
        fill_missing(&mut pack.dark, base.dark);
        Some(pack)
    }
}

/// Insert every `base` entry `map` lacks.
fn fill_missing(map: &mut VarMap, base: VarMap) {
    for (name, value) in base {
        map.entry(name).or_insert(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACKS: [ThemePack; 3] = [ThemePack::Clean, ThemePack::Retro, ThemePack::HighContrast];

    #[test]
    fn builtins_define_the_same_variable_set() {
        let reference = PackTokens::builtin(ThemePack::Clean);
        let names = |m: &VarMap| m.keys().cloned().collect::<Vec<_>>();
        for pack in PACKS {
            let t = PackTokens::builtin(pack);
            assert_eq!(
                names(&t.shared),
                names(&reference.shared),
                "{pack:?} shared"
            );
            assert_eq!(names(&t.light), names(&reference.light), "{pack:?} light");
            assert_eq!(names(&t.dark), names(&reference.dark), "{pack:?} dark");
            assert_eq!(names(&t.light), names(&t.dark), "{pack:?} modes agree");
        }
    }

    #[test]
    fn json_roundtrips_every_builtin() {
        for pack in PACKS {
            let original = PackTokens::builtin(pack);
            let json = original.to_tokens_json();
            let parsed = PackTokens::from_tokens_json(&json).expect("valid JSON");
            assert_eq!(parsed, original, "{pack:?} round-trips");
        }
    }

    #[test]
    fn parsing_ignores_unknown_keys_and_defaults_missing_ones() {
        // Unknown top-level keys are ignored; missing maps fall back to Clean.
        let json = r#"{"name":"Custom","junk":123,"nested":{"a":true}}"#;
        let pack = PackTokens::from_tokens_json(json).expect("tolerant parse");
        assert_eq!(pack.name, "Custom");
        let clean = PackTokens::default();
        assert_eq!(pack.shared, clean.shared);
        assert_eq!(pack.dark, clean.dark);
    }

    #[test]
    fn parsing_fills_partially_missing_variables() {
        // A light map with a single override keeps it and gains the rest.
        let json = r##"{"name":"Accent only","light":{"--rh-accent":"#ff00ff"}}"##;
        let pack = PackTokens::from_tokens_json(json).expect("tolerant parse");
        assert_eq!(pack.light["--rh-accent"], "#ff00ff");
        let clean = PackTokens::default();
        assert_eq!(pack.light["--rh-bg"], clean.light["--rh-bg"]);
        assert_eq!(pack.light.len(), clean.light.len());
    }

    #[test]
    fn malformed_json_is_rejected() {
        assert_eq!(PackTokens::from_tokens_json("not json"), None);
        assert_eq!(PackTokens::from_tokens_json(r#"{"name": 12"#), None);
    }

    #[test]
    fn retro_has_scanlines_and_monospace_body() {
        let retro = PackTokens::builtin(ThemePack::Retro);
        for mode in [Mode::Light, Mode::Dark] {
            let style = retro.style_for(mode);
            assert!(
                style.contains("repeating-linear-gradient"),
                "{mode:?} scanlines"
            );
            assert!(
                style.contains("--rh-font-sans:ui-monospace"),
                "{mode:?} mono body"
            );
        }
        // And Retro light/dark genuinely differ (unlike the core palette,
        // which reuses one Retro palette for both modes).
        assert_ne!(retro.style_for(Mode::Light), retro.style_for(Mode::Dark));
    }

    #[test]
    fn flat_packs_have_no_background_texture() {
        for pack in [ThemePack::Clean, ThemePack::HighContrast] {
            let t = PackTokens::builtin(pack);
            assert_eq!(t.light["--rh-bg-image"], "none");
            assert_eq!(t.dark["--rh-bg-image"], "none");
        }
    }

    #[test]
    fn high_contrast_is_aaa_leaning() {
        // WCAG gamma-corrected contrast (unlike the quick approximation in
        // `rabbithole_core::theme::Rgb::luminance`), shared with the theme
        // editor's live checker.
        fn contrast(a: Rgb, b: Rgb) -> f64 {
            crate::theme_editor::contrast_ratio((a.0, a.1, a.2), (b.0, b.1, b.2))
        }
        for mode in [Mode::Light, Mode::Dark] {
            let p = Palette::builtin(ThemePack::HighContrast, mode);
            // AAA (7:1) for running text, AA (4.5:1) at least for accents.
            assert!(contrast(p.text, p.background) >= 7.0, "{mode:?} text AAA");
            assert!(contrast(p.muted, p.background) >= 7.0, "{mode:?} muted AAA");
            assert!(contrast(p.accent, p.background) >= 4.5, "{mode:?} accent");
            assert!(contrast(p.error, p.background) >= 4.5, "{mode:?} error");
        }
    }
}
