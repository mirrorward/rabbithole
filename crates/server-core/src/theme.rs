//! Server-side theme-bundle application (Wave 8): validation, WCAG
//! contrast rails, and the config bridge that persists an applied bundle.
//!
//! Wave 2.3 established the wire shape — a signed, content-addressed
//! [`ThemeBundle`] served via `ThemeGet` — and stored the operator's accent
//! and ANSI logo in config keys (`theme_accent`, `theme_logo_ansi`). Wave 8
//! lets an admin upload a whole bundle (accent tokens, logo art, icon set)
//! that the server then serves to everyone, so the rails here are
//! **stricter than the client editor's**: where `ui-web`'s theme editor
//! merely warns below 4.5:1 contrast, [`apply_theme_bundle`] *rejects*,
//! reporting the computed ratio — a server-applied theme hits every user.
//!
//! Tokens are a closed grammar mirroring the client editor's validation:
//! colour tokens must be hex (`#rgb`/`#rrggbb`), metric tokens must be
//! simple CSS lengths, `--rh-bg-image` may only be `none`, and anything
//! unknown (font stacks, gradients, free-form CSS) is refused outright —
//! a theme bundle must never become a CSS injection vector.

use std::collections::BTreeMap;

use rabbithole_proto::welcome::ThemeBundle;

use crate::config::ServerConfig;

/// Minimum WCAG 2.x contrast ratio for text-on-bg and accent-on-bg, per
/// mode. Below this the bundle is rejected (the client editor only warns).
pub const MIN_CONTRAST: f64 = 4.5;

/// Longest accepted theme display name, in bytes.
pub const MAX_NAME_LEN: usize = 64;

/// Cap on the inline ANSI/CP437 logo art, in bytes (raster art travels as
/// blobs and rides the existing blob caps instead).
pub const MAX_LOGO_BYTES: usize = 64 * 1024;

/// Cap on the number of icon overrides in one bundle.
pub const MAX_ICONS: usize = 64;

/// Longest accepted icon name or token value, in bytes.
pub const MAX_TOKEN_LEN: usize = 64;

/// An sRGB colour as channel bytes.
pub type Rgb8 = (u8, u8, u8);

/// Colour tokens allowed in the per-mode maps (hex values only).
const COLOR_VARS: [&str; 6] = [
    "--rh-bg",
    "--rh-surface",
    "--rh-text",
    "--rh-muted",
    "--rh-accent",
    "--rh-error",
];

/// The background-texture token: the one place the client grammar allows
/// free-form CSS (gradients). Server bundles may only say `none`.
const BG_IMAGE_VAR: &str = "--rh-bg-image";

/// Metric tokens allowed in the shared map (CSS lengths only). Font-stack
/// tokens are deliberately absent: they're free-form strings client-side,
/// which a server bundle must not carry.
const LENGTH_VARS: [&str; 10] = [
    "--rh-space-1",
    "--rh-space-2",
    "--rh-space-3",
    "--rh-space-4",
    "--rh-space-6",
    "--rh-radius",
    "--rh-radius-lg",
    "--rh-font-size",
    "--rh-font-sm",
    "--rh-font-xs",
];

/// The Clean built-in palette anchors contrast checks for tokens a bundle
/// leaves unset (same values as `rabbithole-core`'s `Palette::builtin`).
const CLEAN_LIGHT: (Rgb8, Rgb8, Rgb8) = (
    (0xfa, 0xfb, 0xfc), // bg
    (0x1a, 0x1d, 0x24), // text
    (0x2b, 0x63, 0xd8), // accent
);
const CLEAN_DARK: (Rgb8, Rgb8, Rgb8) = ((0x14, 0x16, 0x1b), (0xe6, 0xe8, 0xec), (0x6c, 0x9c, 0xff));

/// Why a theme bundle was refused. Contrast refusals carry the computed
/// ratio — the operator sees exactly how far short the pair fell.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ThemeError {
    #[error("bundle bytes do not decode as a theme bundle")]
    BadEncoding,
    #[error("bundle signature does not verify against the server key")]
    BadSignature,
    #[error("theme name is longer than {MAX_NAME_LEN} bytes")]
    NameTooLong,
    #[error("unknown token {var:?} (structured tokens only; free-form CSS is refused)")]
    UnknownToken { var: String },
    #[error("duplicate token {var:?}")]
    DuplicateToken { var: String },
    #[error("token {var}: {value:?} is not a hex colour (#rgb or #rrggbb)")]
    BadColor { var: String, value: String },
    #[error("token {var}: {value:?} is not a simple CSS length (e.g. .25rem, 16px)")]
    BadLength { var: String, value: String },
    #[error("token {var}: {value:?} is free-form CSS; server bundles may only say \"none\"")]
    FreeFormCss { var: String, value: String },
    #[error("token {var}: value exceeds {MAX_TOKEN_LEN} bytes")]
    TokenTooLong { var: String },
    #[error(
        "{mode} mode: {pair} contrast is {ratio:.2}:1, below the {MIN_CONTRAST}:1 minimum \
         for server-applied themes"
    )]
    LowContrast {
        mode: &'static str,
        pair: &'static str,
        ratio: f64,
    },
    #[error("logo art is {len} bytes (max {max})")]
    LogoTooLarge { len: usize, max: usize },
    #[error("banner blob is not in the store (upload via BlobPut first)")]
    BannerMissing,
    #[error("banner blob is {size} bytes (max {max})")]
    BannerTooLarge { size: u64, max: u64 },
    #[error("bundle has {count} icons (max {max})")]
    TooManyIcons { count: usize, max: usize },
    #[error("icon name {name:?} is empty, too long, or repeated")]
    BadIconName { name: String },
    #[error("icon {name:?}: blob is not in the store (upload via BlobPut first)")]
    IconMissing { name: String },
    #[error("icon {name:?}: blob is {size} bytes (max {max})")]
    IconTooLarge { name: String, size: u64, max: u64 },
}

/// Size caps for bundle art, taken from the existing blob-cap config keys.
#[derive(Debug, Clone, Copy)]
pub struct ThemeLimits {
    /// Banner blob cap (`banner_max_bytes`).
    pub banner_max_bytes: u64,
    /// Per-icon blob cap (`avatar_max_bytes` — icons are avatar-sized art).
    pub icon_max_bytes: u64,
}

impl ThemeLimits {
    pub fn from_config(cfg: &ServerConfig) -> Self {
        Self {
            banner_max_bytes: cfg.banner_max_bytes as u64,
            icon_max_bytes: cfg.avatar_max_bytes as u64,
        }
    }
}

/// A validated, canonicalized theme bundle ready to persist and serve.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedTheme {
    /// The canonical bundle: trimmed name, token maps and icons sorted by
    /// name — so the served bytes (and their id) are deterministic.
    pub bundle: ThemeBundle,
    /// The canonical postcard encoding of `bundle`.
    pub canonical_bytes: Vec<u8>,
    /// blake3 of `canonical_bytes` — the content address clients cache by.
    pub id: [u8; 32],
}

/// Validate and canonicalize an uploaded theme bundle.
///
/// `bundle_bytes` is the postcard [`ThemeBundle`] exactly as it would ride
/// a `ThemeReply` (the v1 travel format). `signature`, when non-empty,
/// must be Ed25519 over `bundle_bytes` by the server identity key — the
/// re-import path for a previously served bundle; empty skips the check
/// (the server signs fresh at serve time either way). `blob_size` reports
/// a referenced blob's stored size, or `None` when absent.
pub fn apply_theme_bundle(
    bundle_bytes: &[u8],
    signature: &[u8],
    server_key: &[u8; 32],
    limits: &ThemeLimits,
    blob_size: impl Fn(&[u8; 32]) -> Option<u64>,
) -> Result<AppliedTheme, ThemeError> {
    if !signature.is_empty() {
        let sig: [u8; 64] = signature.try_into().map_err(|_| ThemeError::BadSignature)?;
        let pk = rabbithole_identity::PublicKey(*server_key);
        if !pk.verify(bundle_bytes, &rabbithole_identity::Signature(sig)) {
            return Err(ThemeError::BadSignature);
        }
    }

    let bundle: ThemeBundle =
        postcard::from_bytes(bundle_bytes).map_err(|_| ThemeError::BadEncoding)?;

    let name = bundle.name.trim().to_string();
    if name.len() > MAX_NAME_LEN {
        return Err(ThemeError::NameTooLong);
    }

    if let Some(logo) = &bundle.logo_ansi {
        if logo.len() > MAX_LOGO_BYTES {
            return Err(ThemeError::LogoTooLarge {
                len: logo.len(),
                max: MAX_LOGO_BYTES,
            });
        }
    }

    if let Some(banner) = &bundle.banner {
        let size = blob_size(banner).ok_or(ThemeError::BannerMissing)?;
        if size > limits.banner_max_bytes {
            return Err(ThemeError::BannerTooLarge {
                size,
                max: limits.banner_max_bytes,
            });
        }
    }

    if bundle.icons.len() > MAX_ICONS {
        return Err(ThemeError::TooManyIcons {
            count: bundle.icons.len(),
            max: MAX_ICONS,
        });
    }
    let mut icons: BTreeMap<String, [u8; 32]> = BTreeMap::new();
    for (icon_name, blob) in &bundle.icons {
        let trimmed = icon_name.trim();
        if trimmed.is_empty()
            || trimmed.len() > MAX_TOKEN_LEN
            || icons.insert(trimmed.to_string(), *blob).is_some()
        {
            return Err(ThemeError::BadIconName {
                name: icon_name.clone(),
            });
        }
        let size = blob_size(blob).ok_or_else(|| ThemeError::IconMissing {
            name: trimmed.to_string(),
        })?;
        if size > limits.icon_max_bytes {
            return Err(ThemeError::IconTooLarge {
                name: trimmed.to_string(),
                size,
                max: limits.icon_max_bytes,
            });
        }
    }

    let light = validate_tokens(&bundle.tokens_light, TokenMap::Color)?;
    let dark = validate_tokens(&bundle.tokens_dark, TokenMap::Color)?;
    let shared = validate_tokens(&bundle.tokens_shared, TokenMap::Shared)?;

    // Contrast rails: text-on-bg and accent-on-bg must clear MIN_CONTRAST
    // in *both* modes. Tokens a bundle leaves unset fall back to the Clean
    // built-ins (a mode's accent falls back to the v1 `accent_rgb` field
    // first) — so a single shared accent must be readable in both modes,
    // and serious bundles supply per-mode accents.
    for (mode, map, builtin) in [("light", &light, CLEAN_LIGHT), ("dark", &dark, CLEAN_DARK)] {
        let (builtin_bg, builtin_text, builtin_accent) = builtin;
        let bg = token_rgb(map, "--rh-bg").unwrap_or(builtin_bg);
        let text = token_rgb(map, "--rh-text").unwrap_or(builtin_text);
        let accent = token_rgb(map, "--rh-accent")
            .or(bundle.accent_rgb.map(|[r, g, b]| (r, g, b)))
            .unwrap_or(builtin_accent);
        for (pair, fg) in [
            ("text on background", text),
            ("accent on background", accent),
        ] {
            let ratio = contrast_ratio(fg, bg);
            if ratio < MIN_CONTRAST {
                return Err(ThemeError::LowContrast { mode, pair, ratio });
            }
        }
    }

    // Canonical form: trimmed name, everything sorted.
    let mut canonical = ThemeBundle::new(name);
    canonical.accent_rgb = bundle.accent_rgb;
    canonical.logo_ansi = bundle.logo_ansi.clone();
    canonical.banner = bundle.banner;
    canonical.icons = icons.into_iter().collect();
    canonical.tokens_light = light.into_iter().collect();
    canonical.tokens_dark = dark.into_iter().collect();
    canonical.tokens_shared = shared.into_iter().collect();

    let canonical_bytes = postcard::to_allocvec(&canonical).map_err(|_| ThemeError::BadEncoding)?;
    let id = *blake3::hash(&canonical_bytes).as_bytes();
    Ok(AppliedTheme {
        bundle: canonical,
        canonical_bytes,
        id,
    })
}

/// Which grammar a token map is validated against.
enum TokenMap {
    /// Per-mode colour map: `COLOR_VARS` as hex, `--rh-bg-image` as `none`.
    Color,
    /// Shared metric map: `LENGTH_VARS` as CSS lengths.
    Shared,
}

fn validate_tokens(
    tokens: &[(String, String)],
    kind: TokenMap,
) -> Result<BTreeMap<String, String>, ThemeError> {
    let mut out = BTreeMap::new();
    for (var, value) in tokens {
        let value = value.trim();
        if value.len() > MAX_TOKEN_LEN {
            return Err(ThemeError::TokenTooLong { var: var.clone() });
        }
        match kind {
            TokenMap::Color if COLOR_VARS.contains(&var.as_str()) => {
                if parse_hex(value).is_none() {
                    return Err(ThemeError::BadColor {
                        var: var.clone(),
                        value: value.to_string(),
                    });
                }
            }
            TokenMap::Color if var == BG_IMAGE_VAR => {
                if value != "none" {
                    return Err(ThemeError::FreeFormCss {
                        var: var.clone(),
                        value: value.to_string(),
                    });
                }
            }
            TokenMap::Shared if LENGTH_VARS.contains(&var.as_str()) => {
                if !is_css_length(value) {
                    return Err(ThemeError::BadLength {
                        var: var.clone(),
                        value: value.to_string(),
                    });
                }
            }
            _ => return Err(ThemeError::UnknownToken { var: var.clone() }),
        }
        if out.insert(var.clone(), value.to_string()).is_some() {
            return Err(ThemeError::DuplicateToken { var: var.clone() });
        }
    }
    Ok(out)
}

fn token_rgb(map: &BTreeMap<String, String>, var: &str) -> Option<Rgb8> {
    map.get(var).and_then(|v| parse_hex(v))
}

/// Parse `#rgb` / `#rrggbb` into channels; `None` for anything else.
pub fn parse_hex(s: &str) -> Option<Rgb8> {
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

/// Whether `s` is a simple non-negative CSS length: `0`, or a finite
/// non-negative number with a unit (`rem`, `em`, `px`, `pt`, `ch`, `vh`,
/// `vw`, `%`) — the same small grammar the client editor enforces.
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

/// The WCAG 2.x contrast ratio between two sRGB colours (1.0..=21.0),
/// using gamma-corrected relative luminance — the same math as the client
/// editor's checker, re-implemented here because the server refuses (not
/// merely warns about) low-contrast bundles.
pub fn contrast_ratio(a: Rgb8, b: Rgb8) -> f64 {
    fn luminance((r, g, b): Rgb8) -> f64 {
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

// ---------------------------------------------------------------------------
// Config bridge: an applied theme persists in the theme_* config keys the
// v1 accent/logo already used, and the welcome/theme serve path rebuilds
// the canonical bundle from config on every fetch (hot application).
// ---------------------------------------------------------------------------

/// Persist an applied theme into config (all fields land under one caller-
/// held lock via [`crate::config::LiveConfig::update`]).
pub fn write_to_config(
    applied: &AppliedTheme,
    cfg: &mut ServerConfig,
    applied_at_unix: i64,
    applied_by: &str,
) {
    let b = &applied.bundle;
    cfg.theme_name = b.name.clone();
    cfg.theme_accent = b
        .accent_rgb
        .map(|[r, g, bl]| format!("{r:02x}{g:02x}{bl:02x}"))
        .unwrap_or_default();
    cfg.theme_logo_ansi = b.logo_ansi.clone().unwrap_or_default();
    cfg.theme_banner = b.banner.map(hex::encode).unwrap_or_default();
    cfg.theme_icons = b
        .icons
        .iter()
        .map(|(n, blob)| (n.clone(), hex::encode(blob)))
        .collect();
    cfg.theme_tokens_light = b.tokens_light.iter().cloned().collect();
    cfg.theme_tokens_dark = b.tokens_dark.iter().cloned().collect();
    cfg.theme_tokens_shared = b.tokens_shared.iter().cloned().collect();
    cfg.theme_applied_at_unix = applied_at_unix;
    cfg.theme_applied_by = applied_by.to_string();
}

/// Clear every theme field — clients fall back to default tokens on their
/// next fetch.
pub fn clear_config(cfg: &mut ServerConfig) {
    cfg.theme_name = String::new();
    cfg.theme_accent = String::new();
    cfg.theme_logo_ansi = String::new();
    cfg.theme_banner = String::new();
    cfg.theme_icons = BTreeMap::new();
    cfg.theme_tokens_light = BTreeMap::new();
    cfg.theme_tokens_dark = BTreeMap::new();
    cfg.theme_tokens_shared = BTreeMap::new();
    cfg.theme_applied_at_unix = 0;
    cfg.theme_applied_by = String::new();
}

/// Rebuild the canonical served bundle from config, or `None` when no
/// theme is set (the v1 rule, extended to the new fields). Hand-edited
/// config values that don't parse (bad hex) are skipped, like the v1
/// accent path.
pub fn bundle_from_config(cfg: &ServerConfig) -> Option<ThemeBundle> {
    let accent = (!cfg.theme_accent.is_empty())
        .then(|| hex::decode(&cfg.theme_accent).ok())
        .flatten()
        .and_then(|v| <[u8; 3]>::try_from(v).ok());
    let logo = (!cfg.theme_logo_ansi.is_empty()).then(|| cfg.theme_logo_ansi.clone());
    let banner = (!cfg.theme_banner.is_empty())
        .then(|| hex::decode(&cfg.theme_banner).ok())
        .flatten()
        .and_then(|v| <[u8; 32]>::try_from(v).ok());
    let icons: Vec<(String, [u8; 32])> = cfg
        .theme_icons
        .iter()
        .filter_map(|(n, h)| {
            let blob = <[u8; 32]>::try_from(hex::decode(h).ok()?).ok()?;
            Some((n.clone(), blob))
        })
        .collect();

    if accent.is_none()
        && logo.is_none()
        && banner.is_none()
        && icons.is_empty()
        && cfg.theme_tokens_light.is_empty()
        && cfg.theme_tokens_dark.is_empty()
        && cfg.theme_tokens_shared.is_empty()
    {
        return None;
    }

    let name = if cfg.theme_name.is_empty() {
        cfg.name.clone()
    } else {
        cfg.theme_name.clone()
    };
    let mut bundle = ThemeBundle::new(name);
    bundle.accent_rgb = accent;
    bundle.logo_ansi = logo;
    bundle.banner = banner;
    bundle.icons = icons;
    bundle.tokens_light = map_to_vec(&cfg.theme_tokens_light);
    bundle.tokens_dark = map_to_vec(&cfg.theme_tokens_dark);
    bundle.tokens_shared = map_to_vec(&cfg.theme_tokens_shared);
    Some(bundle)
}

/// [`bundle_from_config`] plus the canonical bytes and content id — what
/// `ThemeBundleGet` and `ctl theme-status` report.
pub fn applied_from_config(cfg: &ServerConfig) -> Option<AppliedTheme> {
    let bundle = bundle_from_config(cfg)?;
    let canonical_bytes = postcard::to_allocvec(&bundle).ok()?;
    let id = *blake3::hash(&canonical_bytes).as_bytes();
    Some(AppliedTheme {
        bundle,
        canonical_bytes,
        id,
    })
}

fn map_to_vec(map: &BTreeMap<String, String>) -> Vec<(String, String)> {
    map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rabbithole_identity::keys::IdentityKey;

    const WHITE: Rgb8 = (0xff, 0xff, 0xff);
    const BLACK: Rgb8 = (0, 0, 0);

    fn limits() -> ThemeLimits {
        ThemeLimits {
            banner_max_bytes: 1024 * 1024,
            icon_max_bytes: 256 * 1024,
        }
    }

    /// Every blob exists at a friendly size.
    fn blobs_ok(_id: &[u8; 32]) -> Option<u64> {
        Some(1024)
    }

    fn apply(bundle: &ThemeBundle) -> Result<AppliedTheme, ThemeError> {
        let bytes = postcard::to_allocvec(bundle).unwrap();
        apply_theme_bundle(&bytes, &[], &[0u8; 32], &limits(), blobs_ok)
    }

    /// A bundle that clears every rail: per-mode accents from the Clean
    /// built-ins, a logo, and a metric token.
    fn good_bundle() -> ThemeBundle {
        let mut b = ThemeBundle::new("Wonderland");
        b.logo_ansi = Some("== W8 ==".into());
        b.tokens_light = vec![("--rh-accent".into(), "#2b63d8".into())];
        b.tokens_dark = vec![("--rh-accent".into(), "#6c9cff".into())];
        b.tokens_shared = vec![("--rh-radius".into(), ".5rem".into())];
        b
    }

    // ---- contrast math ----------------------------------------------------

    #[test]
    fn contrast_math_matches_known_ratios() {
        // The canonical extremes.
        assert!((contrast_ratio(BLACK, WHITE) - 21.0).abs() < 0.01);
        assert!(
            (contrast_ratio(WHITE, BLACK) - 21.0).abs() < 0.01,
            "symmetric"
        );
        assert!((contrast_ratio(WHITE, WHITE) - 1.0).abs() < 1e-9);
        // #767676 on white is the classic just-passes-AA gray (≈4.54:1);
        // #777777 just fails (≈4.48:1).
        let just_passes = contrast_ratio((0x76, 0x76, 0x76), WHITE);
        assert!((4.5..4.6).contains(&just_passes), "{just_passes}");
        let just_fails = contrast_ratio((0x77, 0x77, 0x77), WHITE);
        assert!((4.4..4.5).contains(&just_fails), "{just_fails}");
        // Gamma correction: mid-gray luminance is ~0.216, not 0.5 — the
        // linear (non-gamma) formula would put this ratio near 11.
        let mid = contrast_ratio((0x80, 0x80, 0x80), BLACK);
        assert!((5.2..5.4).contains(&mid), "{mid}");
    }

    #[test]
    fn builtin_palettes_clear_the_rails_in_both_modes() {
        for (bg, text, accent) in [CLEAN_LIGHT, CLEAN_DARK] {
            assert!(contrast_ratio(text, bg) >= MIN_CONTRAST);
            assert!(contrast_ratio(accent, bg) >= MIN_CONTRAST);
        }
    }

    // ---- token grammar ------------------------------------------------------

    #[test]
    fn hex_and_length_grammar() {
        assert_eq!(parse_hex("#fff"), Some((255, 255, 255)));
        assert_eq!(parse_hex("#abc"), Some((0xaa, 0xbb, 0xcc)));
        assert_eq!(parse_hex(" #102030 "), Some((0x10, 0x20, 0x30)));
        assert_eq!(parse_hex("fff"), None);
        assert_eq!(parse_hex("#ffff"), None);
        assert_eq!(parse_hex("#gg0000"), None);
        assert_eq!(parse_hex("#\u{e9}\u{e9}\u{e9}"), None);

        for ok in ["0", ".25rem", "16px", "1.5em", "100%", "2ch"] {
            assert!(is_css_length(ok), "{ok}");
        }
        for bad in ["", "rem", "-1px", "1e400px", "12", "url(x)", "16 px"] {
            assert!(!is_css_length(bad), "{bad}");
        }
    }

    #[test]
    fn rejects_unknown_and_freeform_tokens() {
        let mut b = good_bundle();
        b.tokens_light.push(("--rh-custom".into(), "#fff".into()));
        assert_eq!(
            apply(&b),
            Err(ThemeError::UnknownToken {
                var: "--rh-custom".into()
            })
        );

        // Font stacks are free-form client-side; server bundles refuse them.
        let mut b = good_bundle();
        b.tokens_shared
            .push(("--rh-font-sans".into(), "Comic Sans MS".into()));
        assert_eq!(
            apply(&b),
            Err(ThemeError::UnknownToken {
                var: "--rh-font-sans".into()
            })
        );

        // A CSS injection attempt through the one texture token.
        let mut b = good_bundle();
        b.tokens_dark.push((
            BG_IMAGE_VAR.into(),
            "url(https://evil.example/x)}body{display:none".into(),
        ));
        assert!(matches!(
            apply(&b),
            Err(ThemeError::FreeFormCss { var, .. }) if var == BG_IMAGE_VAR
        ));

        // `none` is the only accepted texture.
        let mut b = good_bundle();
        b.tokens_dark.push((BG_IMAGE_VAR.into(), "none".into()));
        assert!(apply(&b).is_ok());
    }

    #[test]
    fn rejects_bad_values_and_duplicates() {
        let mut b = good_bundle();
        b.tokens_light = vec![("--rh-accent".into(), "not-a-colour".into())];
        assert_eq!(
            apply(&b),
            Err(ThemeError::BadColor {
                var: "--rh-accent".into(),
                value: "not-a-colour".into()
            })
        );

        let mut b = good_bundle();
        b.tokens_shared = vec![("--rh-radius".into(), "50vmax".into())];
        assert_eq!(
            apply(&b),
            Err(ThemeError::BadLength {
                var: "--rh-radius".into(),
                value: "50vmax".into()
            })
        );

        let mut b = good_bundle();
        b.tokens_dark = vec![
            ("--rh-accent".into(), "#6c9cff".into()),
            ("--rh-accent".into(), "#ffffff".into()),
        ];
        assert_eq!(
            apply(&b),
            Err(ThemeError::DuplicateToken {
                var: "--rh-accent".into()
            })
        );
    }

    // ---- contrast rails ------------------------------------------------------

    #[test]
    fn low_contrast_is_rejected_with_the_computed_ratio() {
        // The v1-style single accent: #ff8800 reads fine on dark but falls
        // to ~2.3:1 on the light background — rejected, ratio reported.
        let mut b = ThemeBundle::new("Blaze");
        b.accent_rgb = Some([0xff, 0x88, 0x00]);
        let expected = contrast_ratio((0xff, 0x88, 0x00), CLEAN_LIGHT.0);
        match apply(&b) {
            Err(ThemeError::LowContrast { mode, pair, ratio }) => {
                assert_eq!(mode, "light");
                assert_eq!(pair, "accent on background");
                assert!((ratio - expected).abs() < 1e-9);
                assert!(ratio < 3.0, "well below the bar: {ratio}");
            }
            other => panic!("expected LowContrast, got {other:?}"),
        }
        // The error text carries the number for operators.
        let msg = ThemeError::LowContrast {
            mode: "light",
            pair: "accent on background",
            ratio: expected,
        }
        .to_string();
        assert!(msg.contains("2.3"), "{msg}");

        // Unreadable text tokens are refused too.
        let mut b = good_bundle();
        b.tokens_dark = vec![
            ("--rh-accent".into(), "#6c9cff".into()),
            ("--rh-text".into(), "#222222".into()),
        ];
        assert!(matches!(
            apply(&b),
            Err(ThemeError::LowContrast {
                mode: "dark",
                pair: "text on background",
                ..
            })
        ));

        // Per-mode accents fix what a single shared accent cannot.
        let mut b = ThemeBundle::new("Blaze");
        b.accent_rgb = Some([0xff, 0x88, 0x00]); // legacy clients, dark-ish packs
        b.tokens_light = vec![("--rh-accent".into(), "#a34700".into())]; // darker orange
        b.tokens_dark = vec![("--rh-accent".into(), "#ff8800".into())];
        assert!(apply(&b).is_ok(), "per-mode accents clear the rails");
    }

    // ---- art caps + signature -------------------------------------------------

    #[test]
    fn rejects_oversized_and_missing_art() {
        let mut b = good_bundle();
        b.logo_ansi = Some("x".repeat(MAX_LOGO_BYTES + 1));
        assert_eq!(
            apply(&b),
            Err(ThemeError::LogoTooLarge {
                len: MAX_LOGO_BYTES + 1,
                max: MAX_LOGO_BYTES
            })
        );

        let mut b = good_bundle();
        b.banner = Some([7; 32]);
        let bytes = postcard::to_allocvec(&b).unwrap();
        // Missing from the store.
        assert_eq!(
            apply_theme_bundle(&bytes, &[], &[0u8; 32], &limits(), |_| None),
            Err(ThemeError::BannerMissing)
        );
        // Present but over the banner cap.
        assert_eq!(
            apply_theme_bundle(&bytes, &[], &[0u8; 32], &limits(), |_| Some(
                2 * 1024 * 1024
            )),
            Err(ThemeError::BannerTooLarge {
                size: 2 * 1024 * 1024,
                max: 1024 * 1024
            })
        );

        let mut b = good_bundle();
        b.icons = vec![("dm".into(), [9; 32])];
        let bytes = postcard::to_allocvec(&b).unwrap();
        assert_eq!(
            apply_theme_bundle(&bytes, &[], &[0u8; 32], &limits(), |_| Some(512 * 1024)),
            Err(ThemeError::IconTooLarge {
                name: "dm".into(),
                size: 512 * 1024,
                max: 256 * 1024
            })
        );

        let mut b = good_bundle();
        b.icons = (0..=MAX_ICONS)
            .map(|i| (format!("icon-{i}"), [i as u8; 32]))
            .collect();
        assert_eq!(
            apply(&b),
            Err(ThemeError::TooManyIcons {
                count: MAX_ICONS + 1,
                max: MAX_ICONS
            })
        );

        let mut b = good_bundle();
        b.icons = vec![("dm".into(), [1; 32]), (" dm ".into(), [2; 32])];
        assert!(matches!(apply(&b), Err(ThemeError::BadIconName { .. })));
    }

    #[test]
    fn signature_verification_per_v1() {
        let key = IdentityKey::generate();
        let bytes = postcard::to_allocvec(&good_bundle()).unwrap();
        let sig = key.sign(&bytes).0.to_vec();

        // A previously served (signed) bundle re-imports.
        let ok = apply_theme_bundle(&bytes, &sig, &key.public().0, &limits(), blobs_ok);
        assert!(ok.is_ok());

        // Wrong key refuses.
        let other = IdentityKey::generate();
        assert_eq!(
            apply_theme_bundle(&bytes, &sig, &other.public().0, &limits(), blobs_ok),
            Err(ThemeError::BadSignature)
        );

        // Tampered bytes refuse.
        let mut tampered = bytes.clone();
        tampered[0] ^= 0xff;
        assert_eq!(
            apply_theme_bundle(&tampered, &sig, &key.public().0, &limits(), blobs_ok),
            Err(ThemeError::BadSignature)
        );

        // Garbage refuses as encoding, not a panic.
        assert_eq!(
            apply_theme_bundle(&[1, 2, 3], &[], &key.public().0, &limits(), blobs_ok),
            Err(ThemeError::BadEncoding)
        );
    }

    // ---- canonicalization + config round trip ----------------------------------

    #[test]
    fn canonical_id_is_order_independent() {
        let mut a = good_bundle();
        a.tokens_shared = vec![
            ("--rh-radius".into(), ".5rem".into()),
            ("--rh-space-1".into(), ".25rem".into()),
        ];
        let mut b = good_bundle();
        b.tokens_shared = vec![
            ("--rh-space-1".into(), ".25rem".into()),
            ("--rh-radius".into(), ".5rem".into()),
        ];
        let (a, b) = (apply(&a).unwrap(), apply(&b).unwrap());
        assert_eq!(a.id, b.id, "same tokens, same content address");
        assert_eq!(a.canonical_bytes, b.canonical_bytes);
    }

    #[test]
    fn config_round_trip_preserves_the_bundle() {
        let mut b = good_bundle();
        b.accent_rgb = Some([0x2b, 0x63, 0xd8]);
        b.banner = Some([7; 32]);
        b.icons = vec![("dm".into(), [9; 32])];
        let applied = apply(&b).unwrap();

        let mut cfg = ServerConfig::default();
        write_to_config(&applied, &mut cfg, 12345, "root");
        assert_eq!(cfg.theme_applied_at_unix, 12345);
        assert_eq!(cfg.theme_applied_by, "root");
        assert_eq!(cfg.theme_accent, "2b63d8");
        assert_eq!(cfg.theme_logo_ansi, "== W8 ==");

        let rebuilt = applied_from_config(&cfg).expect("theme present");
        assert_eq!(rebuilt.bundle, applied.bundle);
        assert_eq!(rebuilt.id, applied.id, "content address survives config");

        clear_config(&mut cfg);
        assert!(bundle_from_config(&cfg).is_none(), "cleared = no theme");
        assert_eq!(cfg.theme_applied_at_unix, 0);
    }

    #[test]
    fn v1_config_still_serves_and_name_falls_back() {
        // A pre-Wave-8 config: accent + logo only, no bundle fields.
        let mut cfg = ServerConfig {
            name: "The Warren".into(),
            theme_accent: "ff8800".into(),
            theme_logo_ansi: "== v1 ==".into(),
            ..ServerConfig::default()
        };
        let bundle = bundle_from_config(&cfg).expect("v1 theme present");
        assert_eq!(bundle.name, "The Warren", "v1 used the server name");
        assert_eq!(bundle.accent_rgb, Some([0xff, 0x88, 0x00]));
        assert_eq!(bundle.logo_ansi.as_deref(), Some("== v1 =="));
        assert!(bundle.tokens_light.is_empty());

        // An unparseable hand-edited accent is skipped, not fatal.
        cfg.theme_accent = "nope".into();
        let bundle = bundle_from_config(&cfg).expect("logo keeps it present");
        assert_eq!(bundle.accent_rgb, None);
    }
}
