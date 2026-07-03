//! Pure, DOM-free state for the admin **Theme editor** panel.
//!
//! Like [`crate::state`], [`crate::files`], and [`crate::admin`], this module
//! holds no Leptos or `web_sys` types: the whole editor — the working
//! [`PackTokens`], the [`EditorAction`] reducer, per-action validation, and
//! the WCAG contrast checker — is unit-tested on the host with `cargo test`.
//! The panel component in [`crate::components`] owns a reactive
//! `RwSignal<EditorState>` and folds actions into it via
//! [`EditorState::apply`].
//!
//! ## Validation philosophy
//!
//! Edits are validated *per action*: a colour variable must be `#rgb` /
//! `#rrggbb` hex, spacing/radii/type-scale variables must be CSS lengths, and
//! the free-form slots (`--rh-bg-image`, font stacks) just have to be
//! non-empty. An invalid action leaves the working pack untouched and parks a
//! human-readable message in [`EditorState::error`] — never a panic.
//!
//! Contrast, by contrast (sorry), only **warns**: ratios below
//! [`MIN_CONTRAST`] surface as [`ContrastWarning`]s but nothing blocks a save
//! or export. That matches the theme-bundle safety-rails philosophy in
//! [`crate::packs`] — token files are tamper-tolerant, and an ugly theme is
//! the operator's prerogative.

use rabbithole_core::theme::{Mode, ThemePack};

use crate::packs::PackTokens;

/// The WCAG AA threshold for normal text; ratios below this warn.
pub const MIN_CONTRAST: f64 = 4.5;

/// One editor intent, folded into [`EditorState`] via [`EditorState::apply`].
#[derive(Debug, Clone, PartialEq)]
pub enum EditorAction {
    /// Set a colour variable for one mode (hex, or free-form for
    /// `--rh-bg-image`).
    SetColor {
        /// Which mode's colour map to edit.
        mode: Mode,
        /// The `--rh-*` variable name.
        var: String,
        /// The new value.
        value: String,
    },
    /// Set a shared (mode-independent) variable: spacing, radii, typography.
    SetShared {
        /// The `--rh-*` variable name.
        var: String,
        /// The new value.
        value: String,
    },
    /// Replace the working pack with a parsed JSON token file
    /// (tamper-tolerant, via [`PackTokens::from_tokens_json`]).
    LoadJson(String),
    /// Discard edits and reload a built-in pack.
    Reset(ThemePack),
    /// Choose a different built-in as the editing base (same effect as
    /// [`EditorAction::Reset`]; kept distinct so the UI reads as intent).
    SelectBase(ThemePack),
}

/// The theme editor's full model: a working token pack plus edit bookkeeping.
#[derive(Debug, Clone, PartialEq)]
pub struct EditorState {
    /// The built-in pack the working copy started from.
    pub base: ThemePack,
    /// The tokens being edited.
    pub working: PackTokens,
    /// Whether the working copy has diverged from `base` (any successful
    /// edit or import since the last [`EditorAction::Reset`] /
    /// [`EditorAction::SelectBase`]).
    pub dirty: bool,
    /// The most recent validation/parse failure, cleared by the next
    /// successful action.
    pub error: Option<String>,
}

impl EditorState {
    /// A clean editor seeded from a built-in pack.
    pub fn new(base: ThemePack) -> Self {
        Self {
            base,
            working: PackTokens::builtin(base),
            dirty: false,
            error: None,
        }
    }

    /// Fold one action into the state. Invalid input sets [`Self::error`]
    /// and leaves the working pack (and `dirty`) unchanged.
    pub fn apply(&mut self, action: EditorAction) {
        match action {
            EditorAction::SetColor { mode, var, value } => {
                let value = value.trim().to_string();
                if let Err(e) = validate_colour(&var, &value) {
                    self.error = Some(e);
                    return;
                }
                let map = match mode {
                    Mode::Light => &mut self.working.light,
                    Mode::Dark => &mut self.working.dark,
                };
                match map.get_mut(&var) {
                    Some(slot) => {
                        *slot = value;
                        self.dirty = true;
                        self.error = None;
                    }
                    None => self.error = Some(format!("Unknown colour variable {var}.")),
                }
            }
            EditorAction::SetShared { var, value } => {
                let value = value.trim().to_string();
                if let Err(e) = validate_shared(&var, &value) {
                    self.error = Some(e);
                    return;
                }
                match self.working.shared.get_mut(&var) {
                    Some(slot) => {
                        *slot = value;
                        self.dirty = true;
                        self.error = None;
                    }
                    None => self.error = Some(format!("Unknown shared variable {var}.")),
                }
            }
            EditorAction::LoadJson(json) => match PackTokens::from_tokens_json(&json) {
                Some(pack) => {
                    self.working = pack;
                    self.dirty = true;
                    self.error = None;
                }
                None => {
                    self.error = Some("Import failed: that is not valid JSON.".to_string());
                }
            },
            EditorAction::Reset(pack) | EditorAction::SelectBase(pack) => {
                self.base = pack;
                self.working = PackTokens::builtin(pack);
                self.dirty = false;
                self.error = None;
            }
        }
    }

    /// Serialise the working pack as a shareable JSON token file (the same
    /// serde path server theme bundles will travel through).
    pub fn export_json(&self) -> String {
        self.working.to_tokens_json()
    }
}

/// Validate a colour-map value: `--rh-bg-image` is free-form (gradients,
/// `none`), everything else must be hex.
fn validate_colour(var: &str, value: &str) -> Result<(), String> {
    if var == "--rh-bg-image" {
        if value.is_empty() {
            Err(format!("{var} cannot be empty (use \"none\" for flat)."))
        } else {
            Ok(())
        }
    } else if is_hex_color(value) {
        Ok(())
    } else {
        Err(format!(
            "{var}: \"{value}\" is not a hex colour (#rgb or #rrggbb)."
        ))
    }
}

/// Validate a shared-map value: spacing, radii, and the type scale must be
/// CSS lengths; the font stacks just have to be non-empty.
fn validate_shared(var: &str, value: &str) -> Result<(), String> {
    let needs_length = var.starts_with("--rh-space")
        || var.starts_with("--rh-radius")
        || matches!(var, "--rh-font-size" | "--rh-font-sm" | "--rh-font-xs");
    if needs_length {
        if is_css_length(value) {
            Ok(())
        } else {
            Err(format!(
                "{var}: \"{value}\" is not a CSS length (e.g. .25rem, 16px)."
            ))
        }
    } else if value.is_empty() {
        Err(format!("{var} cannot be empty."))
    } else {
        Ok(())
    }
}

/// Whether `s` is a `#rgb` or `#rrggbb` hex colour.
pub fn is_hex_color(s: &str) -> bool {
    parse_hex(s).is_some()
}

/// Parse `#rgb` / `#rrggbb` into channels; `None` for anything else.
pub fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.trim().strip_prefix('#')?;
    if !h.is_ascii() {
        return None;
    }
    match h.len() {
        3 => {
            let mut it = h.chars().map(|c| c.to_digit(16).map(|d| (d * 0x11) as u8));
            Some((it.next()??, it.next()??, it.next()??))
        }
        6 => {
            let byte = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
            Some((byte(0)?, byte(2)?, byte(4)?))
        }
        _ => None,
    }
}

/// Whether `s` is a simple non-negative CSS length: `0`, or a number with a
/// unit (`rem`, `em`, `px`, `pt`, `ch`, `vh`, `vw`, `%`).
pub fn is_css_length(s: &str) -> bool {
    let s = s.trim();
    if s == "0" {
        return true;
    }
    const UNITS: [&str; 8] = ["rem", "em", "px", "pt", "ch", "vh", "vw", "%"];
    UNITS.iter().any(|unit| {
        s.strip_suffix(unit).is_some_and(|n| {
            !n.is_empty() && n.parse::<f64>().is_ok_and(|v| v.is_finite() && v >= 0.0)
        })
    })
}

/// The WCAG 2.x contrast ratio between two sRGB colours (1.0..=21.0), using
/// gamma-corrected relative luminance. This is the exact math the
/// high-contrast pack assertions in [`crate::packs`] rely on, promoted out of
/// test-only code so the editor can warn live.
pub fn contrast_ratio(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    fn luminance((r, g, b): (u8, u8, u8)) -> f64 {
        let lin = |v: u8| {
            let s = f64::from(v) / 255.0;
            if s <= 0.04045 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
    }
    let (x, y) = (luminance(a) + 0.05, luminance(b) + 0.05);
    if x > y {
        x / y
    } else {
        y / x
    }
}

/// One low-contrast finding from [`contrast_warnings`].
#[derive(Debug, Clone, PartialEq)]
pub struct ContrastWarning {
    /// The mode whose colour map failed.
    pub mode: Mode,
    /// Human label for the pair, e.g. `"text on background"`.
    pub pair: &'static str,
    /// The measured contrast ratio.
    pub ratio: f64,
}

impl ContrastWarning {
    /// A one-line message for the editor UI.
    pub fn message(&self) -> String {
        format!(
            "{:?} mode: {} contrast is {:.2}:1 (below {MIN_CONTRAST}:1).",
            self.mode, self.pair, self.ratio
        )
    }
}

/// Check the key foreground/background pairs (text-on-bg, accent-on-bg) in
/// both modes and report every ratio below [`MIN_CONTRAST`]. Variables that
/// are not parseable hex (e.g. after a hand-imported token file) are simply
/// skipped — warnings advise, they never block.
pub fn contrast_warnings(tokens: &PackTokens) -> Vec<ContrastWarning> {
    let mut out = Vec::new();
    for (mode, map) in [(Mode::Light, &tokens.light), (Mode::Dark, &tokens.dark)] {
        let Some(bg) = map.get("--rh-bg").and_then(|v| parse_hex(v)) else {
            continue;
        };
        for (pair, var) in [
            ("text on background", "--rh-text"),
            ("accent on background", "--rh-accent"),
        ] {
            if let Some(fg) = map.get(var).and_then(|v| parse_hex(v)) {
                let ratio = contrast_ratio(fg, bg);
                if ratio < MIN_CONTRAST {
                    out.push(ContrastWarning { mode, pair, ratio });
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_color(e: &mut EditorState, mode: Mode, var: &str, value: &str) {
        e.apply(EditorAction::SetColor {
            mode,
            var: var.into(),
            value: value.into(),
        });
    }

    fn set_shared(e: &mut EditorState, var: &str, value: &str) {
        e.apply(EditorAction::SetShared {
            var: var.into(),
            value: value.into(),
        });
    }

    #[test]
    fn fresh_editor_is_clean() {
        let e = EditorState::new(ThemePack::Retro);
        assert_eq!(e.base, ThemePack::Retro);
        assert_eq!(e.working, PackTokens::builtin(ThemePack::Retro));
        assert!(!e.dirty);
        assert_eq!(e.error, None);
    }

    #[test]
    fn valid_colour_edits_apply_and_mark_dirty() {
        let mut e = EditorState::new(ThemePack::Clean);
        set_color(&mut e, Mode::Dark, "--rh-accent", "#ff00ff");
        assert_eq!(e.working.dark["--rh-accent"], "#ff00ff");
        assert!(e.dirty);
        assert_eq!(e.error, None);
        // Short-form hex and surrounding whitespace are accepted (trimmed).
        set_color(&mut e, Mode::Light, "--rh-text", "  #123  ");
        assert_eq!(e.working.light["--rh-text"], "#123");
        // The other mode's map is untouched.
        let clean = PackTokens::builtin(ThemePack::Clean);
        assert_eq!(e.working.dark["--rh-text"], clean.dark["--rh-text"]);
    }

    #[test]
    fn invalid_colours_are_rejected_without_mutating() {
        let mut e = EditorState::new(ThemePack::Clean);
        let before = e.working.clone();
        for bad in ["red", "#12", "#12345", "#1234567", "#ggg", "", "rgb(0,0,0)"] {
            set_color(&mut e, Mode::Light, "--rh-accent", bad);
            assert!(e.error.is_some(), "{bad:?} should be rejected");
            assert_eq!(e.working, before, "{bad:?} must not mutate");
            assert!(!e.dirty, "{bad:?} must not dirty");
        }
    }

    #[test]
    fn unknown_variables_are_rejected() {
        let mut e = EditorState::new(ThemePack::Clean);
        set_color(&mut e, Mode::Dark, "--rh-nonsense", "#fff");
        assert!(e.error.as_deref().unwrap().contains("--rh-nonsense"));
        assert!(!e.dirty);
        set_shared(&mut e, "--rh-bogus", "1rem");
        assert!(e.error.as_deref().unwrap().contains("--rh-bogus"));
        assert!(!e.dirty);
    }

    #[test]
    fn bg_image_is_free_form_but_not_empty() {
        let mut e = EditorState::new(ThemePack::Clean);
        set_color(&mut e, Mode::Dark, "--rh-bg-image", "none");
        assert_eq!(e.error, None);
        set_color(
            &mut e,
            Mode::Dark,
            "--rh-bg-image",
            "repeating-linear-gradient(0deg,#000 0,#000 1px,transparent 3px)",
        );
        assert_eq!(e.error, None);
        assert!(e.dirty);
        set_color(&mut e, Mode::Dark, "--rh-bg-image", "   ");
        assert!(e.error.is_some());
    }

    #[test]
    fn shared_lengths_are_validated() {
        let mut e = EditorState::new(ThemePack::Clean);
        for good in [".25rem", "1rem", "16px", "0", "2ch", "1.5em", "90%"] {
            set_shared(&mut e, "--rh-space-2", good);
            assert_eq!(e.error, None, "{good:?} should be accepted");
            assert_eq!(e.working.shared["--rh-space-2"], good);
        }
        let before = e.working.clone();
        for bad in ["abc", "1", "-1rem", "", "1rem;", "calc(1px)", "nanrem"] {
            set_shared(&mut e, "--rh-radius", bad);
            assert!(e.error.is_some(), "{bad:?} should be rejected");
            assert_eq!(e.working, before, "{bad:?} must not mutate");
        }
        // Radii and the type scale are lengths too.
        set_shared(&mut e, "--rh-font-size", "1.05rem");
        assert_eq!(e.error, None);
        set_shared(&mut e, "--rh-font-size", "large");
        assert!(e.error.is_some());
    }

    #[test]
    fn font_stacks_are_free_form_but_not_empty() {
        let mut e = EditorState::new(ThemePack::Clean);
        set_shared(&mut e, "--rh-font-sans", "'Comic Sans MS',cursive");
        assert_eq!(e.error, None);
        assert_eq!(
            e.working.shared["--rh-font-sans"],
            "'Comic Sans MS',cursive"
        );
        set_shared(&mut e, "--rh-font-mono", "");
        assert!(e.error.is_some());
    }

    #[test]
    fn errors_clear_on_the_next_valid_action() {
        let mut e = EditorState::new(ThemePack::Clean);
        set_color(&mut e, Mode::Light, "--rh-accent", "nope");
        assert!(e.error.is_some());
        set_color(&mut e, Mode::Light, "--rh-accent", "#0a0a0a");
        assert_eq!(e.error, None);
    }

    #[test]
    fn export_import_round_trips_including_edits() {
        let mut e = EditorState::new(ThemePack::Retro);
        set_color(&mut e, Mode::Dark, "--rh-accent", "#ff8800");
        set_shared(&mut e, "--rh-radius", "0");
        let json = e.export_json();

        let mut other = EditorState::new(ThemePack::Clean);
        other.apply(EditorAction::LoadJson(json));
        assert_eq!(other.error, None);
        assert!(other.dirty);
        assert_eq!(other.working, e.working);
    }

    #[test]
    fn load_json_is_tamper_tolerant() {
        let mut e = EditorState::new(ThemePack::Clean);
        // Unknown keys are ignored; missing maps fall back to Clean.
        e.apply(EditorAction::LoadJson(
            r##"{"name":"Zine","junk":1,"light":{"--rh-accent":"#f0f"}}"##.into(),
        ));
        assert_eq!(e.error, None);
        assert_eq!(e.working.name, "Zine");
        assert_eq!(e.working.light["--rh-accent"], "#f0f");
        let clean = PackTokens::default();
        assert_eq!(e.working.light["--rh-bg"], clean.light["--rh-bg"]);
        assert_eq!(e.working.dark, clean.dark);
    }

    #[test]
    fn malformed_json_errors_without_mutating() {
        let mut e = EditorState::new(ThemePack::Retro);
        let before = e.working.clone();
        e.apply(EditorAction::LoadJson("not json".into()));
        assert!(e.error.is_some());
        assert_eq!(e.working, before);
        assert!(!e.dirty);
    }

    #[test]
    fn reset_and_select_base_reload_builtins_and_clear_flags() {
        let mut e = EditorState::new(ThemePack::Clean);
        set_color(&mut e, Mode::Dark, "--rh-accent", "#123456");
        set_color(&mut e, Mode::Dark, "--rh-accent", "bogus");
        assert!(e.dirty);
        assert!(e.error.is_some());
        e.apply(EditorAction::Reset(ThemePack::Clean));
        assert_eq!(e.working, PackTokens::builtin(ThemePack::Clean));
        assert!(!e.dirty);
        assert_eq!(e.error, None);

        set_color(&mut e, Mode::Light, "--rh-bg", "#eee");
        e.apply(EditorAction::SelectBase(ThemePack::HighContrast));
        assert_eq!(e.base, ThemePack::HighContrast);
        assert_eq!(e.working, PackTokens::builtin(ThemePack::HighContrast));
        assert!(!e.dirty);
    }

    #[test]
    fn hex_parsing_covers_both_forms() {
        assert_eq!(parse_hex("#fff"), Some((255, 255, 255)));
        assert_eq!(parse_hex("#abc"), Some((0xaa, 0xbb, 0xcc)));
        assert_eq!(parse_hex("#102030"), Some((0x10, 0x20, 0x30)));
        assert_eq!(parse_hex(" #000 "), Some((0, 0, 0)));
        assert_eq!(parse_hex("fff"), None);
        assert_eq!(parse_hex("#ffff"), None);
        assert_eq!(parse_hex("#gg0000"), None);
        assert_eq!(parse_hex("#\u{e9}\u{e9}\u{e9}"), None);
    }

    #[test]
    fn contrast_math_matches_known_ratios() {
        let white = (255, 255, 255);
        let black = (0, 0, 0);
        // Black on white is the 21:1 maximum; identical colours are 1:1.
        assert!((contrast_ratio(black, white) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(white, black) - 21.0).abs() < 0.01);
        assert!((contrast_ratio(white, white) - 1.0).abs() < 1e-9);
        // #767676 on white is the canonical AA borderline (~4.54:1).
        let ratio = contrast_ratio((0x76, 0x76, 0x76), white);
        assert!((4.4..4.7).contains(&ratio), "got {ratio}");
    }

    #[test]
    fn contrast_warnings_flag_low_pairs_only() {
        // High Contrast is AAA-leaning: no warnings.
        assert_eq!(
            contrast_warnings(&PackTokens::builtin(ThemePack::HighContrast)),
            vec![]
        );
        // Grey-on-grey text plus a faint accent trips both pairs in dark mode.
        let mut e = EditorState::new(ThemePack::HighContrast);
        set_color(&mut e, Mode::Dark, "--rh-text", "#777");
        set_color(&mut e, Mode::Dark, "--rh-bg", "#555");
        set_color(&mut e, Mode::Dark, "--rh-accent", "#666");
        let warnings = contrast_warnings(&e.working);
        assert_eq!(warnings.len(), 2);
        assert!(warnings.iter().all(|w| w.mode == Mode::Dark));
        assert!(warnings.iter().all(|w| w.ratio < MIN_CONTRAST));
        assert!(warnings[0].message().contains("below 4.5:1"));
        // Unparseable values are skipped, not fatal.
        set_color(&mut e, Mode::Dark, "--rh-bg-image", "none");
        e.working
            .dark
            .insert("--rh-bg".into(), "not-a-colour".into());
        assert_eq!(contrast_warnings(&e.working), vec![]);
    }
}
