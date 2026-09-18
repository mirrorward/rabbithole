//! Where downloads go, and who gets to say.
//!
//! The person does, through native panels; the webview never does. A webview
//! that could hand the shell a path to write would turn any bug in the SPA
//! (or any burrow that found one) into "write this file anywhere I like". So
//! the rules here are: the shell owns the preference file, a folder is only
//! ever chosen in a native folder panel, a one-off destination only in a
//! native save panel, and what the webview supplies (a file name, a burrow's
//! name) is reduced to a single safe path component before it touches the
//! filesystem.
//!
//! The behaviour asked for: a download **asks where to save** unless a
//! download folder has been set; with a folder set it goes there without
//! asking, optionally into **a folder per burrow**.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The person's download preferences, persisted by the shell.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadPrefs {
    /// Where downloads go without asking. `None`: ask each time.
    #[serde(default)]
    pub folder: Option<PathBuf>,
    /// Put each burrow's files in a folder of its own.
    #[serde(default)]
    pub per_burrow: bool,
    /// Offer what you download from a burrow to other people on that burrow
    /// (see [`crate::seeding`]). Off unless the person turns it on.
    #[serde(default)]
    pub seed: bool,
}

/// What to do with one download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// No folder is set: show a save panel opening on `dir`, offering `name`.
    Ask { dir: PathBuf, name: String },
    /// A folder is set: write here. Already unique; never an existing file.
    Write(PathBuf),
}

/// Decide where a download goes. Pure apart from the existence checks that
/// keep [`Destination::Write`] from landing on a file that is already there.
pub fn plan(
    prefs: &DownloadPrefs,
    system_downloads: &Path,
    burrow: &str,
    name: &str,
) -> Destination {
    let name = sanitize_name(name);
    let under = |root: &Path| {
        if prefs.per_burrow {
            root.join(sanitize_component(burrow))
        } else {
            root.to_path_buf()
        }
    };
    match &prefs.folder {
        Some(folder) => Destination::Write(unique_path(&under(folder), &name)),
        None => Destination::Ask {
            dir: under(system_downloads),
            name,
        },
    }
}

/// Reduce a server-supplied filename to a bare, safe basename so it can't escape
/// the downloads directory. Strips path separators and rejects `..`, leading
/// dots, and — for Windows — any name containing a `:` (drive-relative prefixes
/// like `C:evil.exe` PATH-resolve off the target dir, and `report.txt:stream`
/// opens an NTFS alternate data stream), falling back to a fixed safe name.
pub fn sanitize_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
    let unsafe_name = base.is_empty()
        || base == "."
        || base == ".."
        || base.starts_with('.')
        || base.contains(':');
    if unsafe_name {
        "download.bin".to_string()
    } else {
        base.to_string()
    }
}

/// A burrow's name as one folder name. Burrows name themselves, so this is
/// hostile input: separators, colons and control characters become spaces,
/// leading dots go (no hidden folders, no `..`), and the result is capped so a
/// long name cannot build an unusable path.
pub fn sanitize_component(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '\0' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    // Runs of dots are what is left of `../..` once the slashes are gone.
    let cleaned = cleaned
        .split_whitespace()
        .filter(|word| !word.chars().all(|c| c == '.'))
        .collect::<Vec<_>>()
        .join(" ");
    let cleaned = cleaned.trim_start_matches('.').trim();
    let capped: String = cleaned.chars().take(64).collect();
    let capped = capped.trim();
    if capped.is_empty() {
        "burrow".to_string()
    } else {
        capped.to_string()
    }
}

/// The first path for `name` in `dir` that nothing occupies yet: `name`, then
/// `stem (2).ext`, `stem (3).ext`, and so on, the way Finder does it.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let as_path = Path::new(name);
    let stem = as_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_string());
    let ext = as_path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    (2u32..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|candidate| !candidate.exists())
        .expect("an unbounded counter finds a free name")
}

/// Read the preferences file. Missing or unreadable is the default (ask).
pub fn load(path: &Path) -> DownloadPrefs {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Write the preferences file, creating its folder.
pub fn store(path: &Path, prefs: &DownloadPrefs) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(prefs).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_no_folder_set_a_download_asks() {
        let sys = Path::new("/Users/x/Downloads");
        assert_eq!(
            plan(&DownloadPrefs::default(), sys, "The Warren", "readme.txt"),
            Destination::Ask {
                dir: sys.to_path_buf(),
                name: "readme.txt".into()
            }
        );
        // The panel opens in the burrow's own folder when that is wanted.
        let prefs = DownloadPrefs {
            folder: None,
            per_burrow: true,
            ..Default::default()
        };
        assert_eq!(
            plan(&prefs, sys, "The Warren", "readme.txt"),
            Destination::Ask {
                dir: sys.join("The Warren"),
                name: "readme.txt".into()
            }
        );
    }

    #[test]
    fn with_a_folder_set_it_goes_there_and_never_over_a_file() {
        let root = tempfile::tempdir().unwrap();
        let prefs = DownloadPrefs {
            folder: Some(root.path().to_path_buf()),
            per_burrow: true,
            ..Default::default()
        };
        let sys = Path::new("/nowhere");
        let first = plan(&prefs, sys, "The Warren", "lister.lha");
        let Destination::Write(first) = first else {
            panic!("a set folder never asks");
        };
        assert_eq!(first, root.path().join("The Warren").join("lister.lha"));
        std::fs::create_dir_all(first.parent().unwrap()).unwrap();
        std::fs::write(&first, b"x").unwrap();
        assert_eq!(
            plan(&prefs, sys, "The Warren", "lister.lha"),
            Destination::Write(root.path().join("The Warren").join("lister (2).lha"))
        );
        // Without per-burrow folders it is the folder itself.
        let flat = DownloadPrefs {
            folder: Some(root.path().to_path_buf()),
            per_burrow: false,
            ..Default::default()
        };
        assert_eq!(
            plan(&flat, sys, "The Warren", "a.txt"),
            Destination::Write(root.path().join("a.txt"))
        );
    }

    #[test]
    fn a_burrow_cannot_name_its_way_out_of_the_folder() {
        assert_eq!(sanitize_component("The Warren"), "The Warren");
        assert_eq!(sanitize_component("../../etc"), "etc");
        assert_eq!(sanitize_component(".."), "burrow");
        assert_eq!(sanitize_component("  .hidden  "), "hidden");
        assert_eq!(sanitize_component("a/b\\c:d"), "a b c d");
        assert_eq!(sanitize_component("tab\there\nnewline"), "tab here newline");
        assert_eq!(sanitize_component(""), "burrow");
        assert_eq!(sanitize_component(&"x".repeat(200)).chars().count(), 64);
        // And neither can a file name.
        assert_eq!(sanitize_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_name(".."), "download.bin");
        assert_eq!(sanitize_name("C:evil.exe"), "download.bin");
        let root = Path::new("/d");
        let prefs = DownloadPrefs {
            folder: Some(root.to_path_buf()),
            per_burrow: true,
            ..Default::default()
        };
        let Destination::Write(p) = plan(&prefs, root, "../..", "../../x.txt") else {
            panic!()
        };
        assert!(p.starts_with("/d"), "{p:?} stays under the folder");
        assert_eq!(p, Path::new("/d/burrow/x.txt"));
    }

    #[test]
    fn preferences_survive_a_round_trip_and_a_missing_file_means_ask() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("downloads.json");
        assert_eq!(load(&path), DownloadPrefs::default());
        let prefs = DownloadPrefs {
            folder: Some(PathBuf::from("/Volumes/Big/Warren")),
            per_burrow: true,
            ..Default::default()
        };
        store(&path, &prefs).unwrap();
        assert_eq!(load(&path), prefs);
        // A file written before seeding existed loads with it off: opting in
        // is something a person does, never something an upgrade does.
        std::fs::write(&path, r#"{"folder":null,"per_burrow":true}"#).unwrap();
        assert!(!load(&path).seed);
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(load(&path), DownloadPrefs::default(), "garbage means ask");
    }
}
