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
    /// Mode-independent tokens: spacing, the radii scale, the type scale,
    /// and the elevation (shadow) ramp.
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

/// The colour variables for one mode: the six palette roles, the pack's
/// background texture (`--rh-bg-image`, `none` for flat packs), and the
/// keyboard focus-outline colour (`--rh-focus`).
///
/// `--rh-focus` is seeded from the accent — the strongest-chroma role every
/// pack already keeps readable against its fields — and is asserted to
/// clear WCAG's 3:1 non-text minimum against both `--rh-bg` and
/// `--rh-surface` in every built-in pack × mode (see the tests below). It is
/// a distinct token so custom packs can tune the outline independently; the
/// theme editor validates and contrast-warns on it like any other colour.
fn colour_vars(p: &Palette, bg_image: &str) -> VarMap {
    let mut m = VarMap::new();
    m.insert("--rh-bg".into(), hex(p.background));
    m.insert("--rh-surface".into(), hex(p.surface));
    m.insert("--rh-text".into(), hex(p.text));
    m.insert("--rh-muted".into(), hex(p.muted));
    m.insert("--rh-accent".into(), hex(p.accent));
    m.insert("--rh-error".into(), hex(p.error));
    m.insert("--rh-focus".into(), hex(p.accent));
    m.insert("--rh-bg-image".into(), bg_image.into());
    m
}

/// The mode-independent tokens a pack contributes: the type scale, the
/// corner-rounding scale, and the elevation (shadow) scale. Spacing is
/// universal (same rhythm on every pack) and filled in by [`shared_vars`];
/// everything here is what makes Clean feel airy and elevated, Retro boxy
/// and flat, and High Contrast crisp.
struct SharedSpec {
    font_sans: &'static str,
    font_mono: &'static str,
    /// Type scale: xs < sm < base < lg < xl < 2xl < 3xl.
    font_xs: &'static str,
    font_sm: &'static str,
    font_size: &'static str,
    font_lg: &'static str,
    font_xl: &'static str,
    font_2xl: &'static str,
    font_3xl: &'static str,
    /// Corner rounding: sm < base < lg < xl, plus a pill/circle `full`.
    radius_sm: &'static str,
    radius: &'static str,
    radius_lg: &'static str,
    radius_xl: &'static str,
    radius_full: &'static str,
    /// Elevation ramp (`none` for the flat packs).
    shadow_1: &'static str,
    shadow_2: &'static str,
    shadow_3: &'static str,
}

/// The mode-independent tokens: universal spacing rhythm plus the pack's
/// [`SharedSpec`] (type scale, rounding, elevation).
fn shared_vars(s: &SharedSpec) -> VarMap {
    let mut m = VarMap::new();
    m.insert("--rh-space-1".into(), ".25rem".into());
    m.insert("--rh-space-2".into(), ".5rem".into());
    m.insert("--rh-space-3".into(), ".75rem".into());
    m.insert("--rh-space-4".into(), "1rem".into());
    m.insert("--rh-space-5".into(), "1.25rem".into());
    m.insert("--rh-space-6".into(), "1.5rem".into());
    m.insert("--rh-space-8".into(), "2rem".into());
    m.insert("--rh-radius-sm".into(), s.radius_sm.into());
    m.insert("--rh-radius".into(), s.radius.into());
    m.insert("--rh-radius-lg".into(), s.radius_lg.into());
    m.insert("--rh-radius-xl".into(), s.radius_xl.into());
    m.insert("--rh-radius-full".into(), s.radius_full.into());
    m.insert("--rh-font-sans".into(), s.font_sans.into());
    m.insert("--rh-font-mono".into(), s.font_mono.into());
    m.insert("--rh-font-xs".into(), s.font_xs.into());
    m.insert("--rh-font-sm".into(), s.font_sm.into());
    m.insert("--rh-font-size".into(), s.font_size.into());
    m.insert("--rh-font-lg".into(), s.font_lg.into());
    m.insert("--rh-font-xl".into(), s.font_xl.into());
    m.insert("--rh-font-2xl".into(), s.font_2xl.into());
    m.insert("--rh-font-3xl".into(), s.font_3xl.into());
    m.insert("--rh-shadow-1".into(), s.shadow_1.into());
    m.insert("--rh-shadow-2".into(), s.shadow_2.into());
    m.insert("--rh-shadow-3".into(), s.shadow_3.into());
    m
}

/// Clean's soft, cool elevation ramp. In dark mode these near-black shadows
/// read as faint depth (the lighter surface token carries most of the
/// elevation); in light mode they lift cards off the page.
const CLEAN_SHADOW_1: &str = "0 1px 2px rgba(15,23,42,.10)";
const CLEAN_SHADOW_2: &str = "0 2px 4px -1px rgba(15,23,42,.14),0 6px 16px -4px rgba(15,23,42,.18)";
const CLEAN_SHADOW_3: &str =
    "0 12px 32px -8px rgba(15,23,42,.35),0 4px 10px -4px rgba(15,23,42,.22)";

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
                shared: shared_vars(&SharedSpec {
                    font_sans: SANS,
                    font_mono: MONO,
                    font_xs: ".75rem",
                    font_sm: ".875rem",
                    font_size: "1rem",
                    font_lg: "1.125rem",
                    font_xl: "1.375rem",
                    font_2xl: "1.75rem",
                    font_3xl: "2.25rem",
                    radius_sm: ".375rem",
                    radius: ".625rem",
                    radius_lg: ".875rem",
                    radius_xl: "1.25rem",
                    radius_full: "9999px",
                    shadow_1: CLEAN_SHADOW_1,
                    shadow_2: CLEAN_SHADOW_2,
                    shadow_3: CLEAN_SHADOW_3,
                }),
                light: colour_vars(&Palette::builtin(ThemePack::Clean, Mode::Light), "none"),
                dark: colour_vars(&Palette::builtin(ThemePack::Clean, Mode::Dark), "none"),
            },
            // CP437/BBS: monospace body, boxy corners, ANSI-palette accents,
            // and scanlines. Dark is the core Retro palette (dark navy field,
            // amber accent, ANSI bright-red error); light is its paper-mode
            // counterpart with ANSI blue/red accents.
            ThemePack::Retro => PackTokens {
                name: "Retro".into(),
                shared: shared_vars(&SharedSpec {
                    font_sans: MONO,
                    font_mono: MONO,
                    font_xs: ".8rem",
                    font_sm: ".85rem",
                    font_size: ".95rem",
                    font_lg: "1.05rem",
                    font_xl: "1.2rem",
                    font_2xl: "1.45rem",
                    font_3xl: "1.8rem",
                    radius_sm: ".1rem",
                    radius: ".15rem",
                    radius_lg: ".25rem",
                    radius_xl: ".35rem",
                    radius_full: ".35rem",
                    shadow_1: "none",
                    shadow_2: "none",
                    shadow_3: "none",
                }),
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
                shared: shared_vars(&SharedSpec {
                    font_sans: SANS,
                    font_mono: MONO,
                    font_xs: ".85rem",
                    font_sm: ".9rem",
                    font_size: "1.05rem",
                    font_lg: "1.2rem",
                    font_xl: "1.45rem",
                    font_2xl: "1.8rem",
                    font_3xl: "2.3rem",
                    radius_sm: ".25rem",
                    radius: ".4rem",
                    radius_lg: ".6rem",
                    radius_xl: ".9rem",
                    radius_full: "9999px",
                    shadow_1: "none",
                    shadow_2: "none",
                    shadow_3: "none",
                }),
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
    fn focus_outline_clears_3_to_1_in_every_pack_and_mode() {
        // WCAG 2.x SC 1.4.11 (non-text contrast): the keyboard focus
        // indicator must reach 3:1 against adjacent colours. With a 2px
        // outline-offset the outline sits on the page background or a panel
        // surface, so both pairs are checked, in all six pack × mode combos,
        // with the same gamma-corrected math the theme editor warns with.
        for pack in PACKS {
            let tokens = PackTokens::builtin(pack);
            for (mode, map) in [(Mode::Light, &tokens.light), (Mode::Dark, &tokens.dark)] {
                let get = |var: &str| {
                    crate::theme_editor::parse_hex(&map[var])
                        .unwrap_or_else(|| panic!("{pack:?}/{mode:?} {var} is hex"))
                };
                let focus = get("--rh-focus");
                for against in ["--rh-bg", "--rh-surface"] {
                    let ratio = crate::theme_editor::contrast_ratio(focus, get(against));
                    assert!(
                        ratio >= 3.0,
                        "{pack:?}/{mode:?}: focus outline on {against} is {ratio:.2}:1 (< 3:1)"
                    );
                }
            }
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
