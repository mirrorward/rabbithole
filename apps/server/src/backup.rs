//! Wave 13: operator backups — consistent point-in-time snapshots, offline
//! restore, and verification.
//!
//! # What a snapshot holds
//!
//! `ctl backup <dest-dir>` writes a timestamped subdirectory of `dest-dir`
//! containing everything a burrow needs to come back as itself:
//!
//! - `burrow.db` — the database, captured through SQLite's online-backup
//!   path (`VACUUM INTO` on the live pool). It runs in a read transaction,
//!   so the copy is a consistent point-in-time image even with concurrent
//!   writers under WAL. `VACUUM INTO` was chosen over a second `rusqlite`
//!   backup connection because the pool already speaks sqlx and the `INTO`
//!   target binds as an ordinary SQL parameter — no second driver, no
//!   filename splicing, and the copy arrives already defragmented.
//! - `identity/` — the Ed25519 signing seed and TLS material, so the
//!   restored server keeps its federation identity and pinned fingerprint.
//! - `federation/approved_peers.json` — versioned, admin-approved peer
//!   origin-key tuples.
//! - `federation/origin_keys.json` — versioned provenance-bearing origin-key
//!   bindings (direct peer or explicit operator trust), so board-flood trust
//!   survives a restore rather than being relearned from relayed traffic.
//! - `federation/catalog.bin` — the signed local file catalog (preserves
//!   the generation chain peers have seen).
//! - `blobs/` — the content-addressed store. Blobs are immutable once
//!   finalized, so plain file copies are safe; a mid-write staging file
//!   (`*.tmp`, still being uploaded) is deliberately skipped — its upload
//!   either completes after the snapshot (and rides the next backup) or
//!   never existed as content. `.refs` sidecars are advisory counters and
//!   are copied as-is.
//!
//! The database is captured *first*, then the blobs: any blob the DB
//! snapshot references already exists on disk at that point, so it lands in
//! the copy (blobs written after the DB snapshot are harmless extras).
//!
//! Every file is listed in `MANIFEST.json` with its size and BLAKE3 hash;
//! `ctl backup-verify <snapshot-dir>` re-hashes everything and runs
//! `PRAGMA integrity_check` against the snapshot database (opened
//! read-only).
//!
//! # Operator flow for restore
//!
//! A running server cannot safely swap its own database out from under the
//! pool, so `ctl restore` always refuses. Restore runs **offline**:
//!
//! 1. stop the server
//! 2. `burrow restore <snapshot-dir> --data-dir <dir>`
//! 3. start the server
//!
//! The restore verifies the manifest hashes (refusing on any mismatch),
//! moves the current data directory aside to `<data_dir>.pre-restore-<ts>`
//! (never deletes it), and copies exactly the manifest-listed files into a
//! fresh data directory. If a live ctl socket answers under `--data-dir`,
//! the restore refuses to run.

use std::fs;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::Shared;

/// Manifest file name inside a snapshot directory.
pub const MANIFEST_NAME: &str = "MANIFEST.json";

const MANIFEST_VERSION: u32 = 1;

/// The snapshot's table of contents: every backed-up file with its BLAKE3
/// hash and size, plus provenance.
#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    /// RFC 3339 UTC creation time.
    pub created_at: String,
    /// The burrow version that wrote the snapshot.
    pub workspace_version: String,
    pub files: Vec<ManifestFile>,
}

impl Manifest {
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// One backed-up file, path relative to the snapshot directory.
#[derive(Debug, Serialize, Deserialize)]
pub struct ManifestFile {
    pub path: String,
    pub size: u64,
    pub blake3: String,
}

/// What `snapshot` produced.
#[derive(Debug)]
pub struct SnapshotOutcome {
    pub dir: PathBuf,
    pub files: usize,
    pub total_bytes: u64,
}

/// What `restore_offline` did.
#[derive(Debug)]
pub struct RestoreOutcome {
    pub files: usize,
    pub total_bytes: u64,
    /// Where the previous data directory went (when one existed).
    pub moved_aside: Option<PathBuf>,
}

/// Create a consistent point-in-time snapshot of the running burrow into a
/// timestamped subdirectory of `dest`. See the module docs for what is
/// captured and why the ordering (DB first, blobs second) is safe.
pub async fn snapshot(shared: &Shared, dest: &Path) -> Result<SnapshotOutcome> {
    let data_dir = shared.config.read().data_dir.clone();
    fs::create_dir_all(dest)
        .with_context(|| format!("creating backup destination {}", dest.display()))?;
    let snap = timestamped_subdir(dest)?;

    // The database first, through SQLite's online-backup path.
    rabbithole_store_server::vacuum_into(&shared.pool, &snap.join("burrow.db"))
        .await
        .context("VACUUM INTO (online database backup)")?;

    // Everything else is plain (immutable or tiny) files: copy + hash off
    // the async runtime.
    let outcome =
        tokio::task::spawn_blocking(move || copy_and_manifest(&data_dir, &snap)).await??;
    Ok(outcome)
}

/// Re-hash every manifest-listed file in `dir`, refusing on any mismatch,
/// missing file, or size drift. Returns the parsed manifest on success.
/// (The database `PRAGMA integrity_check` is separate — see the
/// `backup-verify` ctl command — so this stays usable offline and sync.)
pub fn verify_snapshot(dir: &Path) -> Result<Manifest> {
    let manifest_path = dir.join(MANIFEST_NAME);
    let raw = fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest: Manifest = serde_json::from_str(&raw)
        .with_context(|| format!("parsing {}", manifest_path.display()))?;
    if manifest.version != MANIFEST_VERSION {
        bail!(
            "unsupported manifest version {} (this burrow writes {MANIFEST_VERSION})",
            manifest.version
        );
    }
    for file in &manifest.files {
        let rel = safe_rel_path(&file.path)?;
        let path = dir.join(&rel);
        let (hash, size) =
            hash_file(&path).with_context(|| format!("hashing snapshot file {}", file.path))?;
        if size != file.size {
            bail!(
                "size mismatch for {}: manifest says {}, file is {size}",
                file.path,
                file.size
            );
        }
        if hash != file.blake3 {
            bail!("hash mismatch for {}: snapshot is corrupt", file.path);
        }
    }
    Ok(manifest)
}

/// Offline restore: verify the snapshot, move any existing data directory
/// aside (never delete), and copy the manifest-listed files into place.
/// Refuses when a live burrow answers on the data dir's ctl socket.
pub fn restore_offline(snapshot: &Path, data_dir: &Path) -> Result<RestoreOutcome> {
    refuse_if_running(data_dir)?;
    let manifest = verify_snapshot(snapshot)
        .with_context(|| format!("snapshot {} failed verification", snapshot.display()))?;

    let moved_aside = if data_dir.exists() {
        let aside = aside_path(data_dir)?;
        fs::rename(data_dir, &aside).with_context(|| {
            format!(
                "moving current data dir aside ({} -> {})",
                data_dir.display(),
                aside.display()
            )
        })?;
        Some(aside)
    } else {
        None
    };

    fs::create_dir_all(data_dir)?;
    for file in &manifest.files {
        let rel = safe_rel_path(&file.path)?;
        let src = snapshot.join(&rel);
        let dst = data_dir.join(&rel);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        // fs::copy preserves permission bits, so owner-only secrets
        // (identity seed, TLS key) stay owner-only.
        fs::copy(&src, &dst)
            .with_context(|| format!("restoring {} -> {}", src.display(), dst.display()))?;
    }
    Ok(RestoreOutcome {
        files: manifest.files.len(),
        total_bytes: manifest.total_bytes(),
        moved_aside,
    })
}

/// A restore under a live server would swap the database out from under
/// its pool: refuse when the data dir's ctl socket answers. A *stale*
/// socket (crashed server) does not connect and does not block.
fn refuse_if_running(data_dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let sock = data_dir.join("ctl.sock");
        if std::os::unix::net::UnixStream::connect(&sock).is_ok() {
            bail!(
                "a burrow is running on {} (its ctl socket answered); \
                 stop the server before restoring",
                data_dir.display()
            );
        }
    }
    #[cfg(not(unix))]
    let _ = data_dir;
    Ok(())
}

/// Walk the data dir, copy the backed-up files into `snap`, hash what was
/// written, and drop `MANIFEST.json` next to them.
fn copy_and_manifest(data_dir: &Path, snap: &Path) -> Result<SnapshotOutcome> {
    // (absolute source, snapshot-relative destination) pairs.
    let mut plan: Vec<(PathBuf, String)> = Vec::new();

    // Identity: signing seed + TLS material (flat directory).
    let identity = data_dir.join("identity");
    if identity.is_dir() {
        for entry in fs::read_dir(&identity)? {
            let path = entry?.path();
            if path.is_file() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    plan.push((path.clone(), format!("identity/{name}")));
                }
            }
        }
    }

    // Federation: approved peers, pinned origin keys, and the signed catalog.
    for name in ["approved_peers.json", "origin_keys.json", "catalog.bin"] {
        let path = data_dir.join("federation").join(name);
        if path.is_file() {
            plan.push((path, format!("federation/{name}")));
        }
    }

    // Blob store: content-addressed and immutable once finalized. Skip
    // `*.tmp` staging files — those are uploads still in flight.
    let blobs = data_dir.join("blobs");
    if blobs.is_dir() {
        let mut stack = vec![blobs.clone()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir)? {
                let path = entry?.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_none_or(|e| e != "tmp") {
                    let rel = path
                        .strip_prefix(&blobs)
                        .expect("blob path under blobs root");
                    plan.push((path.clone(), format!("blobs/{}", rel_to_slash(rel)?)));
                }
            }
        }
    }

    // Copy, then hash the *written* copies (so the manifest attests to what
    // is actually in the snapshot). The database is already in place.
    let mut files: Vec<ManifestFile> = Vec::with_capacity(plan.len() + 1);
    for (src, rel) in &plan {
        let dst = snap.join(safe_rel_path(rel)?);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, &dst)
            .with_context(|| format!("copying {} -> {}", src.display(), dst.display()))?;
        let (blake3, size) = hash_file(&dst)?;
        files.push(ManifestFile {
            path: rel.clone(),
            size,
            blake3,
        });
    }
    let (blake3, size) = hash_file(&snap.join("burrow.db"))?;
    files.push(ManifestFile {
        path: "burrow.db".into(),
        size,
        blake3,
    });
    files.sort_by(|a, b| a.path.cmp(&b.path));

    let manifest = Manifest {
        version: MANIFEST_VERSION,
        created_at: chrono::Utc::now().to_rfc3339(),
        workspace_version: env!("CARGO_PKG_VERSION").into(),
        files,
    };
    let total_bytes = manifest.total_bytes();
    let file_count = manifest.files.len();
    fs::write(
        snap.join(MANIFEST_NAME),
        serde_json::to_vec_pretty(&manifest)?,
    )?;

    Ok(SnapshotOutcome {
        dir: snap.to_path_buf(),
        files: file_count,
        total_bytes,
    })
}

// ---------------------------------------------------------------------------
// The burrow's own backup folder, as the console and `ctl backup` (with no
// destination) use it.
// ---------------------------------------------------------------------------

/// Where snapshots go when nobody names a destination: `backup_dir`, under
/// the data directory when it is relative.
pub fn backups_dir(config: &rabbithole_server_core::ServerConfig) -> PathBuf {
    crate::resolve_dir(&config.data_dir, &config.backup_dir)
}

/// What a snapshot's manifest says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotInfo {
    /// The directory name (`snapshot-20260918-224103`).
    pub name: String,
    pub created_at: String,
    pub workspace_version: String,
    pub files: u64,
    pub total_bytes: u64,
}

/// A directory name as this module writes them: `snapshot-`, then digits and
/// dashes. Anything else in the backup folder is not ours to list or remove.
pub fn snapshot_name_is_acceptable(name: &str) -> bool {
    name.len() <= 64
        && name.strip_prefix("snapshot-").is_some_and(|rest| {
            !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit() || b == b'-')
        })
}

/// One snapshot's manifest summary, or `None` when `name` is not a readable
/// snapshot in `dir`.
pub fn snapshot_info(dir: &Path, name: &str) -> Option<SnapshotInfo> {
    if !snapshot_name_is_acceptable(name) {
        return None;
    }
    let raw = fs::read_to_string(dir.join(name).join(MANIFEST_NAME)).ok()?;
    let manifest: Manifest = serde_json::from_str(&raw).ok()?;
    Some(SnapshotInfo {
        name: name.to_string(),
        created_at: manifest.created_at.clone(),
        workspace_version: manifest.workspace_version.clone(),
        files: manifest.files.len() as u64,
        total_bytes: manifest.total_bytes(),
    })
}

/// The snapshots in `dir`, oldest first. A folder that does not exist yet
/// holds none.
pub fn list_snapshots(dir: &Path) -> Vec<SnapshotInfo> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<SnapshotInfo> = entries
        .flatten()
        .filter_map(|entry| snapshot_info(dir, entry.file_name().to_str()?))
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Remove the snapshot `name` from `dir`. Only a directory named like a
/// snapshot and holding a manifest is removed: the folder may hold other
/// things of the operator's. Returns whether there was one to remove.
pub fn delete_snapshot(dir: &Path, name: &str) -> Result<bool> {
    if !snapshot_name_is_acceptable(name) {
        return Ok(false);
    }
    let path = dir.join(name);
    if !path.join(MANIFEST_NAME).is_file() {
        return Ok(false);
    }
    fs::remove_dir_all(&path).with_context(|| format!("removing snapshot {}", path.display()))?;
    Ok(true)
}

/// What a full check of a snapshot found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckOutcome {
    pub ok: bool,
    /// `ok` when everything checks out, otherwise the first thing wrong.
    pub detail: String,
    pub files: u64,
    pub total_bytes: u64,
}

/// Check a snapshot fully: every file against its manifest hash, then the
/// database against SQLite's integrity check. A snapshot that fails is an
/// outcome, not an error; `Err` means the check itself could not run.
pub async fn check_snapshot(snapshot: PathBuf) -> Result<CheckOutcome> {
    let verified = tokio::task::spawn_blocking({
        let dir = snapshot.clone();
        move || verify_snapshot(&dir)
    })
    .await?;
    let manifest = match verified {
        Ok(manifest) => manifest,
        Err(error) => {
            return Ok(CheckOutcome {
                ok: false,
                detail: format!("{error:#}"),
                files: 0,
                total_bytes: 0,
            })
        }
    };
    let integrity = rabbithole_store_server::integrity_check(&snapshot.join("burrow.db")).await?;
    let ok = integrity == "ok";
    Ok(CheckOutcome {
        ok,
        detail: if ok {
            "ok".to_string()
        } else {
            format!("database integrity check: {integrity}")
        },
        files: manifest.files.len() as u64,
        total_bytes: manifest.total_bytes(),
    })
}

/// Allocate `dest/snapshot-<utc-stamp>[-n]`, claiming it atomically with
/// `create_dir` so two backups in the same second cannot collide.
fn timestamped_subdir(dest: &Path) -> Result<PathBuf> {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let base = format!("snapshot-{stamp}");
    for n in 0..1000u32 {
        let name = if n == 0 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        let candidate = dest.join(name);
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    bail!(
        "could not allocate a snapshot directory under {}",
        dest.display()
    );
}

/// Sibling path the old data dir is moved to: `<data_dir>.pre-restore-<ts>`.
fn aside_path(data_dir: &Path) -> Result<PathBuf> {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let base = format!("{}.pre-restore-{stamp}", data_dir.display());
    for n in 0..1000u32 {
        let candidate = if n == 0 {
            PathBuf::from(&base)
        } else {
            PathBuf::from(format!("{base}-{n}"))
        };
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    bail!("could not allocate a pre-restore directory next to {base}");
}

/// Manifest paths must stay inside the snapshot/data dir: plain relative
/// components only (no absolutes, no `..`, no prefixes).
fn safe_rel_path(p: &str) -> Result<PathBuf> {
    let path = Path::new(p);
    if p.is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("manifest contains an unsafe path: {p:?}");
    }
    Ok(path.to_path_buf())
}

/// Snapshot-relative path with `/` separators (manifest is platform-neutral).
fn rel_to_slash(rel: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => parts.push(
                s.to_str()
                    .with_context(|| format!("non-UTF-8 path component in {}", rel.display()))?,
            ),
            _ => bail!("unexpected path component in {}", rel.display()),
        }
    }
    Ok(parts.join("/"))
}

/// Streaming BLAKE3 of a file: (hex hash, size).
fn hash_file(path: &Path) -> Result<(String, u64)> {
    let mut f =
        fs::File::open(path).with_context(|| format!("opening {} to hash", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = [0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok((hasher.finalize().to_hex().to_string(), size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_rel_path_rejects_escapes() {
        assert!(safe_rel_path("blobs/ab/cd/abcd").is_ok());
        assert!(safe_rel_path("").is_err());
        assert!(safe_rel_path("/etc/passwd").is_err());
        assert!(safe_rel_path("../outside").is_err());
        assert!(safe_rel_path("blobs/../../outside").is_err());
    }

    #[test]
    fn only_our_own_snapshot_directories_are_listed_or_removed() {
        assert!(snapshot_name_is_acceptable("snapshot-20260918-224103"));
        assert!(snapshot_name_is_acceptable("snapshot-20260918-224103-1"));
        assert!(!snapshot_name_is_acceptable("snapshot-"));
        assert!(!snapshot_name_is_acceptable("snapshot-../identity"));
        assert!(!snapshot_name_is_acceptable("burrow.db"));
        assert!(!snapshot_name_is_acceptable("../snapshot-1"));

        let dir = tempfile::tempdir().unwrap();
        assert!(list_snapshots(dir.path()).is_empty());
        let write = |name: &str, with_manifest: bool| {
            let d = dir.path().join(name);
            fs::create_dir_all(&d).unwrap();
            if with_manifest {
                let manifest = Manifest {
                    version: MANIFEST_VERSION,
                    created_at: "2026-09-18T22:41:03Z".into(),
                    workspace_version: "0.222.0".into(),
                    files: vec![ManifestFile {
                        path: "burrow.db".into(),
                        size: 4096,
                        blake3: "00".repeat(32),
                    }],
                };
                fs::write(
                    d.join(MANIFEST_NAME),
                    serde_json::to_vec(&manifest).unwrap(),
                )
                .unwrap();
            }
        };
        write("snapshot-20260918-224103", true);
        write("snapshot-20260917-090000", true);
        write("snapshot-20260919-000000", false);
        write("notes", true);
        let listed = list_snapshots(dir.path());
        assert_eq!(
            listed.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["snapshot-20260917-090000", "snapshot-20260918-224103"],
            "oldest first, a manifest-less folder and a stranger skipped"
        );
        assert_eq!(listed[1].files, 1);
        assert_eq!(listed[1].total_bytes, 4096);
        assert_eq!(listed[1].workspace_version, "0.222.0");

        assert!(!delete_snapshot(dir.path(), "notes").unwrap());
        assert!(
            dir.path().join("notes").is_dir(),
            "a stranger is left alone"
        );
        assert!(!delete_snapshot(dir.path(), "snapshot-20260919-000000").unwrap());
        assert!(delete_snapshot(dir.path(), "snapshot-20260917-090000").unwrap());
        assert!(!dir.path().join("snapshot-20260917-090000").exists());
        assert_eq!(list_snapshots(dir.path()).len(), 1);
    }

    #[test]
    fn timestamped_subdirs_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let a = timestamped_subdir(dir.path()).unwrap();
        let b = timestamped_subdir(dir.path()).unwrap();
        assert_ne!(a, b);
        assert!(a.is_dir() && b.is_dir());
    }
}
