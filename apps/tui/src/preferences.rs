//! Local appearance only: never stores credentials, endpoints, or server themes.
//! Auto follows the startup COLORFGBG terminal hint where supported, otherwise
//! dark. It does not infer the desktop OS theme or query/consume terminal input.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rabbithole_core::theme::{self, Mode, Palette, Rgb, ThemePack};
use rabbithole_proto::welcome::ThemeBundle;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Pack {
    #[default]
    Clean,
    Retro,
    HighContrast,
}

impl Pack {
    pub fn next(self) -> Self {
        match self {
            Self::Clean => Self::Retro,
            Self::Retro => Self::HighContrast,
            Self::HighContrast => Self::Clean,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Clean => "Clean",
            Self::Retro => "Retro",
            Self::HighContrast => "High Contrast",
        }
    }

    fn theme_pack(self) -> ThemePack {
        match self {
            Self::Clean => ThemePack::Clean,
            Self::Retro => ThemePack::Retro,
            Self::HighContrast => ThemePack::HighContrast,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModePreference {
    #[default]
    Auto,
    Light,
    Dark,
}

impl ModePreference {
    pub fn next(self) -> Self {
        match self {
            Self::Auto => Self::Light,
            Self::Light => Self::Dark,
            Self::Dark => Self::Auto,
        }
    }

    fn resolve(self, terminal_hint: Option<Mode>) -> Mode {
        match self {
            Self::Auto => terminal_hint.unwrap_or(Mode::Dark),
            Self::Light => Mode::Light,
            Self::Dark => Mode::Dark,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct Preferences {
    pub pack: Pack,
    pub mode: ModePreference,
}

pub struct Appearance {
    pub preferences: Preferences,
    pub path: Option<PathBuf>,
    terminal_hint: Option<Mode>,
}

impl Appearance {
    pub fn load(path: Option<PathBuf>, terminal_hint: Option<Mode>) -> (Self, Option<String>) {
        let (preferences, warning) = match path.as_deref() {
            Some(path) => match read(path) {
                Ok(preferences) => (preferences, None),
                Err(_) => (
                    Preferences::default(),
                    Some(format!(
                        "Could not read appearance settings at {}; using defaults. Check the file or choose --preferences FILE.",
                        path.display()
                    )),
                ),
            },
            None => (
                Preferences::default(),
                Some("No appearance settings directory; choices last this session. Use --preferences FILE to save them.".into()),
            ),
        };
        (
            Self {
                preferences,
                path,
                terminal_hint,
            },
            warning,
        )
    }

    pub fn palette(&self, server: Option<&ThemeBundle>) -> Palette {
        let pack = self.preferences.pack.theme_pack();
        let mode = self.preferences.mode.resolve(self.terminal_hint);
        let base = Palette::builtin(pack, mode);
        // High Contrast is a deliberate accessibility choice. Never replace
        // its accent, even with an authenticated server's otherwise safe color.
        if pack == ThemePack::HighContrast {
            return base;
        }
        let mut resolved = theme::resolve(pack, mode, server);
        // Accents label chat authors and controls, so require text contrast
        // using linearized sRGB (not just the shared core's decoration rail).
        if contrast(resolved.accent, resolved.background) < 4.5 {
            resolved.accent = base.accent;
        }
        resolved
    }

    pub fn label(&self) -> String {
        let mode = match self.preferences.mode {
            ModePreference::Light => "Light",
            ModePreference::Dark => "Dark",
            ModePreference::Auto => match self.terminal_hint {
                Some(Mode::Light) => "Auto Light",
                Some(Mode::Dark) => "Auto Dark",
                None => "Auto Dark fallback",
            },
        };
        format!("{} · {mode}", self.preferences.pack.name())
    }

    /// Apply immediately, report persistence honestly, and permit a retry on
    /// the next choice after an unwritable/missing directory has been repaired.
    pub fn save_status(&self) -> String {
        match self
            .path
            .as_deref()
            .map(|path| write(path, &self.preferences))
        {
            Some(Ok(())) => format!("{} · saved", self.label()),
            _ => "Appearance not saved; check --preferences FILE".into(),
        }
    }
}

pub fn default_path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("rabbithole").join("tui.toml"))
}

/// rxvt's COLORFGBG is fg;bg, or fg;pixmap;bg. Only the standard ANSI
/// 0–15 background hints are understood; customized/extended colors cannot
/// reliably reveal luminance. Standard bright colors except gray imply light.
/// Source: rxvt(1), ENVIRONMENT (COLORFGBG).
pub fn terminal_mode(value: Option<&str>) -> Option<Mode> {
    let fields: Vec<_> = value?.split(';').collect();
    if !(2..=3).contains(&fields.len()) {
        return None;
    }
    let background: u8 = fields.last()?.parse().ok()?;
    match background {
        0..=6 | 8 => Some(Mode::Dark),
        7 | 9..=15 => Some(Mode::Light),
        _ => None,
    }
}

fn read(path: &Path) -> Result<Preferences> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Preferences::default()),
        Err(err) => return Err(err.into()),
    };
    let mut text = String::new();
    file.take(4097).read_to_string(&mut text)?;
    if text.len() > 4096 {
        bail!("appearance settings exceed 4 KiB");
    }
    Ok(toml::from_str(&text)?)
}

fn write(path: &Path, preferences: &Preferences) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(toml::to_string(preferences)?.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .context("save appearance settings")?;
    Ok(())
}

fn contrast(a: Rgb, b: Rgb) -> f64 {
    let luminance = |Rgb(r, g, b): Rgb| {
        let linear = |c: u8| {
            let c = f64::from(c) / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
    };
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_hint_and_manual_precedence_are_explicit() {
        for hint in ["15;0", "7;default;8", "7;6"] {
            assert_eq!(terminal_mode(Some(hint)), Some(Mode::Dark));
        }
        for hint in ["0;15", "0;default;7", "0;11"] {
            assert_eq!(terminal_mode(Some(hint)), Some(Mode::Light));
        }
        for hint in ["", "15", "0;256", "0;16", "0;default", "0;-1", "1;2;3;4"] {
            assert_eq!(terminal_mode(Some(hint)), None);
        }
        assert_eq!(ModePreference::Auto.resolve(None), Mode::Dark);
        assert_eq!(ModePreference::Auto.resolve(Some(Mode::Light)), Mode::Light);
        assert_eq!(ModePreference::Dark.resolve(Some(Mode::Light)), Mode::Dark);
        assert_eq!(ModePreference::Light.resolve(Some(Mode::Dark)), Mode::Light);
    }

    #[test]
    fn all_choices_round_trip_and_auto_remains_automatic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/tui.toml");
        let (mut appearance, warning) = Appearance::load(Some(path.clone()), Some(Mode::Light));
        assert!(warning.is_none());
        assert!(!path.exists(), "reading defaults must not create a file");
        for pack in [Pack::Clean, Pack::Retro, Pack::HighContrast] {
            for mode in [
                ModePreference::Auto,
                ModePreference::Light,
                ModePreference::Dark,
            ] {
                appearance.preferences = Preferences { pack, mode };
                assert!(appearance.save_status().ends_with("· saved"));
                let (restored, warning) = Appearance::load(Some(path.clone()), Some(Mode::Dark));
                assert!(warning.is_none());
                assert_eq!(restored.preferences, appearance.preferences);
                assert_eq!(
                    restored.preferences.mode.resolve(restored.terminal_hint),
                    mode.resolve(Some(Mode::Dark))
                );
            }
        }
        assert_eq!(Pack::Clean.next().next().next(), Pack::Clean);
        assert_eq!(
            ModePreference::Auto.next().next().next(),
            ModePreference::Auto
        );
    }

    #[test]
    fn corrupt_or_oversized_settings_recover_without_overwriting_until_a_choice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tui.toml");
        for text in [
            "pack = [".to_owned(),
            "mode = 'unknown'".to_owned(),
            "#".repeat(4097),
        ] {
            std::fs::write(&path, &text).unwrap();
            let (appearance, warning) = Appearance::load(Some(path.clone()), None);
            assert_eq!(appearance.preferences, Preferences::default());
            assert!(warning.is_some());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
            assert!(appearance.save_status().ends_with("· saved"));
            assert_eq!(read(&path).unwrap(), Preferences::default());
        }
    }

    #[test]
    fn unwritable_destination_keeps_choice_and_recovers_after_repair() {
        let dir = tempfile::tempdir().unwrap();
        let blocked = dir.path().join("not-a-directory");
        std::fs::write(&blocked, "leave this file intact").unwrap();
        let path = blocked.join("tui.toml");
        let (mut appearance, _) = Appearance::load(Some(path.clone()), None);
        appearance.preferences.pack = Pack::HighContrast;
        assert!(appearance.save_status().contains("not saved"));
        assert_eq!(appearance.preferences.pack, Pack::HighContrast);
        assert_eq!(
            std::fs::read_to_string(&blocked).unwrap(),
            "leave this file intact"
        );
        std::fs::remove_file(&blocked).unwrap();
        assert!(appearance.save_status().ends_with("· saved"));
        assert_eq!(read(&path).unwrap().pack, Pack::HighContrast);
        let (session_only, warning) = Appearance::load(None, None);
        assert!(warning.is_some());
        assert!(session_only.save_status().contains("not saved"));
    }

    #[test]
    fn failed_atomic_replacement_preserves_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tui.toml");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("keep"), "unchanged").unwrap();
        assert!(write(&path, &Preferences::default()).is_err());
        assert_eq!(
            std::fs::read_to_string(path.join("keep")).unwrap(),
            "unchanged"
        );
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "temporary file cleaned up"
        );
    }

    #[test]
    fn palettes_keep_text_contrast_and_high_contrast_ignores_server_accent() {
        let (mut appearance, _) = Appearance::load(None, None);
        for pack in [Pack::Clean, Pack::Retro, Pack::HighContrast] {
            for mode in [ModePreference::Light, ModePreference::Dark] {
                appearance.preferences = Preferences { pack, mode };
                let base = appearance.palette(None);
                for color in [base.text, base.muted, base.accent] {
                    assert!(
                        contrast(color, base.background) >= 4.5,
                        "{pack:?}/{mode:?}: {color:?}"
                    );
                }
                for accent in [[20, 22, 27], [250, 251, 252], [255, 136, 0], [0, 0, 180]] {
                    let mut theme = ThemeBundle::new("Verified fixture");
                    theme.accent_rgb = Some(accent);
                    let resolved = appearance.palette(Some(&theme));
                    assert!(contrast(resolved.accent, resolved.background) >= 4.5);
                    assert_eq!(resolved.text, base.text);
                    assert_eq!(resolved.background, base.background);
                    if pack == Pack::HighContrast {
                        assert_eq!(resolved, base);
                    }
                }
            }
        }
    }
}
