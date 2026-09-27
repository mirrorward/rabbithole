//! Private, bounded checkpoints for legacy uploads. Metadata authenticates a
//! locally stored prefix, not the remote sender's file. Only a newly authorized
//! offer may claim one. Files remain durable while claimed; dropping a caller
//! (including cancellation during blocking IO) releases its exclusive lease.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

pub(crate) const MAX_FILE: u64 = 64 * 1024 * 1024;
const MAX_TOTAL: u64 = 256 * 1024 * 1024;
const MAX_RECORDS: usize = 128;
const MAX_META: u64 = 4096;
const TTL: u64 = 30 * 60;
const MAX_SCAN: usize = MAX_RECORDS * 4;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Protocol {
    Zmodem,
    // Append only: the Zmodem discriminant is persisted in version-1 records.
    Hotline,
}

/// Database identities prevent a newly created folder at an old path from
/// inheriting that folder's unfinished uploads. No target text is a disk path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Target {
    pub protocol: Protocol,
    pub account: i64,
    pub area: i64,
    pub parent: Option<i64>,
    pub name: String,
}

/// Resume metadata, without a remote content hash. ZMODEM knows it at offer;
/// Hotline binds the complete DATA length when its first DATA header arrives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Offer {
    pub length: Option<u64>,
    pub mtime: Option<u64>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Record {
    version: u8,
    target: Target,
    offer: Offer,
    len: u64,
    digest: [u8; 32],
    expires: u64,
}

#[derive(Default)]
struct Index {
    records: BTreeMap<String, Record>,
    claimed: BTreeMap<String, u64>,
    failed: bool,
}

struct Store {
    dir: PathBuf,
    key: [u8; 32],
    index: Mutex<Index>,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    #[cfg(test)]
    append_pause: Mutex<Option<TestPause>>,
    #[cfg(test)]
    failure_pause: Mutex<Option<TestPause>>,
    #[cfg(test)]
    claim_started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

#[cfg(test)]
type TestPause = (
    tokio::sync::oneshot::Sender<()>,
    std::sync::mpsc::Receiver<()>,
);

#[derive(Clone)]
pub(crate) struct Staging(Arc<Store>);

struct Claim {
    store: Arc<Store>,
    id: String,
    cap: u64,
    hasher: Mutex<blake3::Hasher>,
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.store.index.lock().claimed.remove(&self.id);
    }
}

#[derive(Clone)]
pub(crate) struct Lease(Arc<Claim>);

enum ClaimMode {
    Exact(Offer),
    Hotline { resume: bool },
}

impl Staging {
    pub async fn open(data_dir: &Path, seed: &[u8; 32]) -> Result<Self> {
        let dir = data_dir.join("legacy-upload-staging");
        let key = blake3::derive_key("rhp-legacy-upload-staging-v1", seed);
        tokio::task::spawn_blocking(move || {
            Self::open_with_clock(
                dir,
                key,
                Arc::new(|| {
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                }),
            )
        })
        .await?
    }

    fn open_with_clock(
        dir: PathBuf,
        key: [u8; 32],
        clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Result<Self> {
        fs::create_dir_all(&dir)?;
        if !fs::symlink_metadata(&dir)?.file_type().is_dir() {
            bail!("staging root is not a private directory");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        }
        let dir = fs::canonicalize(dir)?;
        let store = Arc::new(Store {
            dir,
            key,
            index: Mutex::new(Index::default()),
            clock,
            #[cfg(test)]
            append_pause: Mutex::new(None),
            #[cfg(test)]
            failure_pause: Mutex::new(None),
            #[cfg(test)]
            claim_started: Mutex::new(None),
        });
        let mut index = store.index.lock();
        let now = (store.clock)();
        let mut paths = Vec::new();
        for entry in fs::read_dir(&store.dir)?.take(MAX_SCAN + 1) {
            paths.push(entry?.path());
        }
        if paths.len() > MAX_SCAN {
            bail!("staging directory entry limit exceeded");
        }
        paths.sort();
        for path in &paths {
            if path.extension().and_then(|e| e.to_str()) != Some("meta") {
                continue;
            }
            let Some(id) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .filter(|s| valid_id(s))
            else {
                continue;
            };
            let record = match store.read_record(path) {
                Ok(record) => Some(record),
                Err(error) if storage_error(&error) => return Err(error),
                Err(_) => None,
            }
            .filter(|record| {
                record.version == 1
                    && record.expires > now
                    && record.expires <= now.saturating_add(TTL)
                    && record.len <= MAX_FILE
                    && record
                        .offer
                        .length
                        .is_none_or(|len| record.len <= len && len <= MAX_FILE)
                    && target_id(&record.target).is_ok_and(|expected| expected == id)
            });
            if let Some(record) = record {
                let total: u64 = index.records.values().map(|r| r.len).sum();
                if index.records.len() < MAX_RECORDS
                    && total.saturating_add(record.len) <= MAX_TOTAL
                {
                    match store.read_prefix(id, &record) {
                        Ok(_) => {
                            index.records.insert(id.to_owned(), record);
                            continue;
                        }
                        Err(error) if storage_error(&error) => return Err(error),
                        Err(_) => {}
                    }
                }
            }
            store.remove_files(id)?;
        }
        // A failed metadata commit can leave a temporary file or data with no
        // metadata. Neither is resumable; only our exact opaque names are swept.
        for path in paths {
            let Some(id) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .filter(|s| valid_id(s))
            else {
                continue;
            };
            let ext = path.extension().and_then(|e| e.to_str());
            if ext == Some("tmp") || (ext == Some("part") && !index.records.contains_key(id)) {
                remove_file(&path)?;
            }
        }
        drop(index);
        Ok(Self(store))
    }

    pub async fn claim(&self, target: Target, offer: Offer, cap: u64) -> Result<(Lease, Vec<u8>)> {
        self.claim_inner(target, ClaimMode::Exact(offer), cap).await
    }

    /// Hotline asks for RFLT before its DATA header is available. Atomically
    /// claim the saved offer or reset a fresh upload; never reset an active one.
    pub async fn claim_hotline(
        &self,
        target: Target,
        resume: bool,
        cap: u64,
    ) -> Result<(Lease, Vec<u8>)> {
        if target.protocol != Protocol::Hotline {
            bail!("invalid Hotline staging protocol");
        }
        self.claim_inner(target, ClaimMode::Hotline { resume }, cap)
            .await
    }

    async fn claim_inner(
        &self,
        target: Target,
        mode: ClaimMode,
        cap: u64,
    ) -> Result<(Lease, Vec<u8>)> {
        let store = self.0.clone();
        tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            if let Some(started) = store.claim_started.lock().take() {
                let _ = started.send(());
            }
            let mut index = store.index.lock();
            let result = (|| -> Result<(String, Vec<u8>)> {
                if index.failed {
                    bail!("staging IO failed; restart after repairing storage");
                }
                let id = target_id(&target)?;
                let now = (store.clock)();
                store.expire(&mut index, now)?;
                if index.claimed.contains_key(&id) {
                    bail!("this destination already has an active upload");
                }
                let (offer, resume) = match &mode {
                    ClaimMode::Exact(offer) => (offer.clone(), true),
                    ClaimMode::Hotline { resume } => (
                        index
                            .records
                            .get(&id)
                            .filter(|_| *resume)
                            .map(|r| r.offer.clone())
                            .unwrap_or(Offer {
                                length: None,
                                mtime: None,
                            }),
                        *resume,
                    ),
                };
                if cap > MAX_FILE || offer.length.is_some_and(|len| len > cap) {
                    bail!("staging file limit exceeded");
                }
                let reserved: u64 = index
                    .records
                    .iter()
                    .filter(|(other, _)| *other != &id)
                    .map(|(other, record)| index.claimed.get(other).copied().unwrap_or(record.len))
                    .sum();
                if reserved.saturating_add(cap) > MAX_TOTAL {
                    bail!("staging byte reservation limit reached");
                }
                let loaded =
                    match index.records.get(&id).filter(|r| {
                        resume && r.target == target && r.offer == offer && r.len <= cap
                    }) {
                        Some(record) => match store.read_prefix(&id, record) {
                            Ok(data) => Some(data),
                            Err(error) if storage_error(&error) => return Err(error),
                            Err(_) => None,
                        },
                        None => None,
                    };
                let data = match loaded {
                    Some(data) => data,
                    None => {
                        if index.records.contains_key(&id) {
                            store.remove_files(&id)?;
                            index.records.remove(&id);
                        }
                        if index.records.len() >= MAX_RECORDS {
                            bail!("staging record limit reached");
                        }
                        let record = Record {
                            version: 1,
                            target,
                            offer,
                            len: 0,
                            digest: *blake3::hash(&[]).as_bytes(),
                            expires: now.saturating_add(TTL),
                        };
                        let file = private_create(&store.path(&id, "part"))?;
                        file.sync_all()?;
                        store.write_record(&id, &record)?;
                        index.records.insert(id.clone(), record);
                        Vec::new()
                    }
                };
                index.claimed.insert(id.clone(), cap);
                Ok((id, data))
            })();
            store.latch_error(&mut index, &result);
            drop(index);
            let (id, data) = result?;
            let mut hasher = blake3::Hasher::new();
            hasher.update(&data);
            // Return the RAII lease from the blocking job itself. If its await
            // is cancelled, dropping the completed job output releases it.
            Ok((
                Lease(Arc::new(Claim {
                    store: store.clone(),
                    id,
                    cap,
                    hasher: Mutex::new(hasher),
                })),
                data,
            ))
        })
        .await?
    }
}

impl Lease {
    /// Bind the assembled Hotline DATA length once, before checkpointing any
    /// bytes. Subsequent resumes must carry exactly the remaining length. This
    /// does not renew expiry: metadata-only negotiations are not progress.
    pub async fn bind_hotline_length(&self, length: u64) -> Result<bool> {
        let claim = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let mut index = claim.store.index.lock();
            let result = (|| -> Result<bool> {
                if index.failed {
                    bail!("staging IO failed; restart after repairing storage");
                }
                let old = index
                    .records
                    .get(&claim.id)
                    .context("staging was discarded")?;
                if old.target.protocol != Protocol::Hotline {
                    bail!("invalid Hotline staging protocol");
                }
                if length > claim.cap || length < old.len {
                    return Ok(false);
                }
                if let Some(expected) = old.offer.length {
                    return Ok(expected == length);
                }
                let mut record = old.clone();
                record.offer.length = Some(length);
                claim.store.write_record(&claim.id, &record)?;
                index.records.insert(claim.id.clone(), record);
                Ok(true)
            })();
            claim.store.latch_error(&mut index, &result);
            result
        })
        .await?
    }

    /// Commit only newly validated contiguous bytes. Sync data first; if
    /// metadata replacement fails, the old signed prefix still names safe data.
    pub async fn append(&self, offset: u64, bytes: &[u8]) -> Result<()> {
        let claim = self.0.clone();
        let bytes = bytes.to_vec();
        tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            if let Some((entered, release)) = claim.store.append_pause.lock().take() {
                let _ = entered.send(());
                let _ = release.recv();
            }
            let mut index = claim.store.index.lock();
            let result = (|| -> Result<()> {
                if index.failed {
                    bail!("staging IO failed; restart after repairing storage");
                }
                let old = index
                    .records
                    .get(&claim.id)
                    .context("staging was discarded")?;
                let len = offset
                    .checked_add(bytes.len() as u64)
                    .context("staging size overflow")?;
                if old.len != offset
                    || len > claim.cap
                    || old.offer.length.is_some_and(|expected| len > expected)
                {
                    bail!("invalid staging offset or length");
                }
                if bytes.is_empty() {
                    return Ok(()); // no progress: do not extend retention
                }
                let total: u64 = index.records.values().map(|r| r.len).sum();
                if total.saturating_add(bytes.len() as u64) > MAX_TOTAL {
                    bail!("staging byte limit reached");
                }
                let mut record = old.clone();
                let path = claim.store.path(&claim.id, "part");
                regular_file(&path)?;
                let mut file = OpenOptions::new().write(true).open(path)?;
                file.set_len(offset)?;
                file.seek(SeekFrom::Start(offset))?;
                file.write_all(&bytes)?;
                file.sync_all()?;
                drop(file);
                let mut next_hash = claim.hasher.lock().clone();
                next_hash.update(&bytes);
                record.len = len;
                record.digest = *next_hash.finalize().as_bytes();
                record.expires = (claim.store.clock)().saturating_add(TTL);
                claim.store.write_record(&claim.id, &record)?;
                index.records.insert(claim.id.clone(), record);
                *claim.hasher.lock() = next_hash;
                Ok(())
            })();
            claim.store.latch_error(&mut index, &result);
            result
        })
        .await?
    }

    pub async fn discard(&self) -> Result<()> {
        let claim = self.0.clone();
        tokio::task::spawn_blocking(move || {
            // Cleanup is still allowed after an IO failure, but publishing any
            // new failure remains serialized with every claim and checkpoint.
            let mut index = claim.store.index.lock();
            let result = (|| -> Result<()> {
                claim.store.remove_files(&claim.id)?;
                index.records.remove(&claim.id);
                Ok(())
            })();
            claim.store.latch_error(&mut index, &result);
            result
        })
        .await?
    }
}

impl Store {
    fn latch_error<T>(&self, index: &mut Index, result: &Result<T>) {
        if result
            .as_ref()
            .err()
            .is_some_and(|e| e.downcast_ref::<io::Error>().is_some())
        {
            #[cfg(test)]
            if let Some((entered, release)) = self.failure_pause.lock().take() {
                let _ = entered.send(());
                let _ = release.recv();
            }
            // A failure after rename may have committed more than memory knows.
            // Keep this latch under the index lock, so already queued operations
            // cannot mutate the snapshot before restart revalidates storage.
            index.failed = true;
        }
    }

    fn path(&self, id: &str, ext: &str) -> PathBuf {
        self.dir.join(format!("{id}.{ext}"))
    }

    fn read_record(&self, path: &Path) -> Result<Record> {
        if regular_file(path)?.len() > MAX_META {
            bail!("staging metadata too large");
        }
        let bytes = fs::read(path)?;
        if bytes.len() < 32 {
            bail!("truncated staging metadata");
        }
        let tag: [u8; 32] = bytes[..32].try_into()?;
        if blake3::keyed_hash(&self.key, &bytes[32..]) != blake3::Hash::from_bytes(tag) {
            bail!("staging metadata authentication failed");
        }
        Ok(postcard::from_bytes(&bytes[32..])?)
    }

    fn read_prefix(&self, id: &str, record: &Record) -> Result<Vec<u8>> {
        let path = self.path(id, "part");
        let size = regular_file(&path)?.len();
        if record.len > MAX_FILE || size < record.len || size > MAX_FILE {
            bail!("invalid staging size");
        }
        let mut data = Vec::with_capacity(record.len as usize);
        File::open(&path)?.take(record.len).read_to_end(&mut data)?;
        if data.len() as u64 != record.len || blake3::hash(&data).as_bytes() != &record.digest {
            bail!("staging prefix integrity failed");
        }
        // Tail bytes written before an interrupted metadata commit are never
        // advertised. Truncate now so they cannot count toward a later append.
        if size > record.len {
            OpenOptions::new()
                .write(true)
                .open(path)?
                .set_len(record.len)?;
        }
        Ok(data)
    }

    fn write_record(&self, id: &str, record: &Record) -> Result<()> {
        let body = postcard::to_allocvec(record)?;
        if body.len() as u64 + 32 > MAX_META {
            bail!("staging metadata too large");
        }
        let tmp = self.path(id, "tmp");
        let result = (|| -> Result<()> {
            remove_file(&tmp)?;
            let mut file = private_create(&tmp)?;
            file.write_all(blake3::keyed_hash(&self.key, &body).as_bytes())?;
            file.write_all(&body)?;
            file.sync_all()?;
            drop(file);
            // Same-directory atomic replacement, as in fed_catalog/cache.rs;
            // std::fs::rename replaces existing files on Windows too.
            fs::rename(&tmp, self.path(id, "meta"))?;
            sync_directory(&self.dir)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = remove_file(&tmp);
        }
        result
    }

    fn remove_files(&self, id: &str) -> Result<()> {
        // Removing the authority first leaves an unresumable orphan after a
        // crash; never a signed record whose payload has already been removed.
        remove_file(&self.path(id, "meta"))?;
        sync_directory(&self.dir)?;
        remove_file(&self.path(id, "part"))?;
        remove_file(&self.path(id, "tmp"))?;
        Ok(())
    }

    fn expire(&self, index: &mut Index, now: u64) -> Result<()> {
        let expired: Vec<_> = index
            .records
            .iter()
            .filter(|(id, r)| {
                !index.claimed.contains_key(*id)
                    && (r.expires <= now || r.expires > now.saturating_add(TTL))
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.remove_files(&id)?;
            index.records.remove(&id);
        }
        Ok(())
    }
}

fn storage_error(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<io::Error>()
        .is_some_and(|e| e.kind() != io::ErrorKind::NotFound)
}
fn target_id(target: &Target) -> Result<String> {
    if target.account <= 0
        || target.area <= 0
        || target.parent.is_some_and(|id| id <= 0)
        || target.name.is_empty()
        || target.name.len() > 128
        || target.name.contains(['/', '\\'])
        || matches!(target.name.as_str(), "." | "..")
        || target.name.chars().any(char::is_control)
    {
        bail!("invalid staging target");
    }
    Ok(blake3::hash(&postcard::to_allocvec(target)?)
        .to_hex()
        .to_string())
}
fn valid_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn regular_file(path: &Path) -> Result<fs::Metadata> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() {
        bail!("staging path is not a regular file");
    }
    Ok(meta)
}
fn private_create(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}
fn remove_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}
fn sync_directory(path: &Path) -> io::Result<()> {
    // Rust's standard library exposes directory fsync on Unix. On Windows
    // flushed files + same-directory replacement cover process-crash recovery;
    // no claim of power-loss directory durability is made there.
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
