//! Client theming: design tokens (light/dark + retro), server theme-bundle
//! verification, and the safety rails that keep server theming from
//! producing unreadable UIs.
//!
//! Tokens are defined once here so every rich client (TUI now; the Leptos
//! GUI/web in Wave 8) means the same thing by "accent" or "surface". A
//! server theme bundle only overrides the accent and supplies art; it
//! never replaces the whole palette, and the accent is contrast-clamped.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use rabbithole_proto::welcome::{ThemeBundle, ThemeReply};

/// An RGB color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Relative luminance (WCAG-ish, 0.0 dark … 1.0 light).
    pub fn luminance(self) -> f32 {
        let f = |c: u8| c as f32 / 255.0;
        0.2126 * f(self.0) + 0.7152 * f(self.1) + 0.0722 * f(self.2)
    }

    fn contrast(self, other: Rgb) -> f32 {
        let (a, b) = (self.luminance() + 0.05, other.luminance() + 0.05);
        if a > b {
            a / b
        } else {
            b / a
        }
    }
}

/// Base appearance mode chosen by the user (follows OS by default in GUIs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Light,
    Dark,
}

/// The built-in theme packs (PLAN §9.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemePack {
    Clean,
    Retro,
    HighContrast,
}

/// Resolved design tokens a client renders with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub background: Rgb,
    pub surface: Rgb,
    pub text: Rgb,
    pub muted: Rgb,
    pub accent: Rgb,
    pub error: Rgb,
}

impl Palette {
    /// The built-in palette for a pack + mode.
    pub fn builtin(pack: ThemePack, mode: Mode) -> Palette {
        match (pack, mode) {
            (ThemePack::Clean, Mode::Dark) => Palette {
                background: Rgb(0x14, 0x16, 0x1b),
                surface: Rgb(0x1e, 0x21, 0x28),
                text: Rgb(0xe6, 0xe8, 0xec),
                muted: Rgb(0x8a, 0x90, 0x9c),
                accent: Rgb(0x6c, 0x9c, 0xff),
                error: Rgb(0xff, 0x6b, 0x6b),
            },
            (ThemePack::Clean, Mode::Light) => Palette {
                background: Rgb(0xfa, 0xfb, 0xfc),
                surface: Rgb(0xff, 0xff, 0xff),
                text: Rgb(0x1a, 0x1d, 0x24),
                muted: Rgb(0x5a, 0x62, 0x70),
                accent: Rgb(0x2b, 0x63, 0xd8),
                error: Rgb(0xc0, 0x2a, 0x2a),
            },
            // Retro: the DOS/BBS aesthetic — dark blue field, amber accent.
            (ThemePack::Retro, _) => Palette {
                background: Rgb(0x00, 0x00, 0x2a),
                surface: Rgb(0x00, 0x00, 0x3f),
                text: Rgb(0xd0, 0xd0, 0xd0),
                muted: Rgb(0x80, 0x80, 0x80),
                accent: Rgb(0xff, 0xb0, 0x00),
                error: Rgb(0xff, 0x55, 0x55),
            },
            (ThemePack::HighContrast, Mode::Dark) => Palette {
                background: Rgb(0, 0, 0),
                surface: Rgb(0, 0, 0),
                text: Rgb(0xff, 0xff, 0xff),
                muted: Rgb(0xc0, 0xc0, 0xc0),
                accent: Rgb(0xff, 0xff, 0x00),
                error: Rgb(0xff, 0x40, 0x40),
            },
            (ThemePack::HighContrast, Mode::Light) => Palette {
                background: Rgb(0xff, 0xff, 0xff),
                surface: Rgb(0xff, 0xff, 0xff),
                text: Rgb(0, 0, 0),
                muted: Rgb(0x30, 0x30, 0x30),
                accent: Rgb(0x00, 0x00, 0xcc),
                error: Rgb(0xcc, 0x00, 0x00),
            },
        }
    }

    /// Apply a server accent, but only if it stays readable against the
    /// background — the safety rail. Below 3:1 contrast the server accent
    /// is rejected and the built-in one kept.
    pub fn with_server_accent(mut self, accent: Rgb) -> Palette {
        if accent.contrast(self.background) >= 3.0 {
            self.accent = accent;
        }
        self
    }
}

/// Verify a server theme bundle's Ed25519 signature against the server
/// identity key; returns the decoded bundle only if it's authentic.
pub fn verify_theme_bundle(reply: &ThemeReply, server_key: &[u8; 32]) -> Option<ThemeBundle> {
    let sig: [u8; 64] = reply.signature.as_slice().try_into().ok()?;
    let vk = VerifyingKey::from_bytes(server_key).ok()?;
    vk.verify(&reply.bundle, &Signature::from_bytes(&sig))
        .ok()?;
    postcard::from_bytes(&reply.bundle).ok()
}

/// Fold a verified server bundle into a base palette: applies the accent
/// (contrast-clamped) and nothing else structural. Returns the resolved
/// palette a client should render with.
pub fn resolve(pack: ThemePack, mode: Mode, server: Option<&ThemeBundle>) -> Palette {
    let base = Palette::builtin(pack, mode);
    match server.and_then(|b| b.accent_rgb) {
        Some([r, g, b]) => base.with_server_accent(Rgb(r, g, b)),
        None => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_rail_rejects_unreadable_accent() {
        let dark = Palette::builtin(ThemePack::Clean, Mode::Dark);
        let base_accent = dark.accent;
        // Near-black accent on a dark background: rejected.
        let bad = dark.with_server_accent(Rgb(0x10, 0x10, 0x12));
        assert_eq!(bad.accent, base_accent, "low-contrast accent rejected");
        // A bright accent: accepted.
        let good = dark.with_server_accent(Rgb(0xff, 0x88, 0x00));
        assert_eq!(good.accent, Rgb(0xff, 0x88, 0x00));
    }

    #[test]
    fn light_and_dark_differ() {
        let l = Palette::builtin(ThemePack::Clean, Mode::Light);
        let d = Palette::builtin(ThemePack::Clean, Mode::Dark);
        assert!(l.background.luminance() > d.background.luminance());
    }

    #[test]
    fn verify_rejects_bad_signature() {
        use rabbithole_identity::keys::IdentityKey;
        let key = IdentityKey::generate();
        let mut bundle = ThemeBundle::new("Test");
        bundle.accent_rgb = Some([1, 2, 3]);
        let bytes = postcard::to_allocvec(&bundle).unwrap();
        let sig = key.sign(&bytes);
        let good = ThemeReply::new(bytes.clone(), sig.0.to_vec());
        assert!(verify_theme_bundle(&good, &key.public().0).is_some());

        // Wrong key rejects.
        let other = IdentityKey::generate();
        assert!(verify_theme_bundle(&good, &other.public().0).is_none());

        // Tampered bundle rejects.
        let mut tampered = good.clone();
        tampered.bundle[0] ^= 0xff;
        assert!(verify_theme_bundle(&tampered, &key.public().0).is_none());
    }
}
