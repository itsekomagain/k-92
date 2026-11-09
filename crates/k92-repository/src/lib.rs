//! The K-92 on-disk backup repository. See `docs/backup-format/repository-format.md`.

pub mod descriptor;
mod fsutil;
pub mod manifest;
pub mod pack;
pub mod retention;
mod verify;

use std::collections::HashSet;
use std::fs::{self, File, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use chrono::{SecondsFormat, Utc};

use k92_core::dto::DamagedSnapshot;
use k92_core::{CancelToken, Error, IoContext, ObjectHash, Result};

pub use descriptor::Descriptor;
pub use manifest::{Entry, HostInfo, Snapshot, SnapshotId, SourceRoot};
pub use pack::SMALL_OBJECT;

use pack::{Encoding, OpenPack, PackIndex, PackLoc};

/// Folder created inside the user's chosen destination.
pub const REPO_DIR_NAME: &str = "K-92 Backup";
const DESCRIPTOR_FILE: &str = "k92-repository.json";
const IO_BUF: usize = 1024 * 1024;
/// Files smaller than this are stored raw: compressing them cannot save a disk cluster.
const MIN_COMPRESS_SIZE: usize = 4096;
/// The start of each file is test-compressed; at most this much is used.
const COMPRESS_SAMPLE: usize = 256 * 1024;
/// zstd level: about 3x faster than a USB hard drive can write, on one core.
const ZSTD_LEVEL: i32 = 3;

pub(crate) fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

const README: &str = "\
This folder contains K-92 backups.

Do not rename, edit or delete files in here by hand.

To restore files without the K-92 app, use the portable `k92` tool:
    k92 snapshots --repo \"<this folder>\"
    k92 restore --repo \"<this folder>\" --snapshot latest --path \"<file or folder>\" --to <folder>

Layout: objects/ holds larger file contents named by their BLAKE3 hash, packs/ holds
small files bundled together, snapshots/ holds one JSON manifest per backup. The format is documented in the K-92 source repository under
docs/backup-format/repository-format.md.
";

/// An opened repository. Cheap to clone. Reading needs no lock.
#[derive(Debug, Clone)]
pub struct Repository {
    root: PathBuf,
    descriptor: Descriptor,
    /// Pack indexes, loaded on first use and shared between clones.
    packs: Arc<RwLock<Option<PackIndex>>>,
}

/// Outcome of storing one object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredObject {
    pub hash: ObjectHash,
    pub size: u64,
    /// False if identical content was already in the repository.
    pub is_new: bool,
    /// Bytes written to the backup drive for a new object (smaller than `size` if compressed).
    pub stored_size: u64,
}

/// Where and how an object is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectFile {
    Raw { path: PathBuf, len: u64 },
    Zstd { path: PathBuf, len: u64 },
    Packed { path: PathBuf, loc: PackLoc },
}

/// Distinguishes problems reading the *source* (skip the file, keep going) from
/// problems writing the *repository* (abort the backup).
#[derive(Debug)]
pub enum StoreError {
    Source(io::Error),
    Repo(Error),
}

impl Repository {
    /// Creates a new repository at `<dest>/K-92 Backup`.
    pub fn init(dest: &Path) -> Result<Self> {
        if !dest.is_dir() {
            return Err(Error::NotFound(format!("destination {}", dest.display())));
        }
        let root = dest.join(REPO_DIR_NAME);
        if root.join(DESCRIPTOR_FILE).exists() {
            return Err(Error::InvalidRequest(format!(
                "{} already contains a K-92 backup; open it instead",
                dest.display()
            )));
        }
        for d in ["", "objects", "packs", "snapshots", "staging"] {
            fs::create_dir_all(root.join(d)).ctx("create", &root.join(d))?;
        }
        let staging = root.join("staging");
        fsutil::write_atomic(&staging, &root.join("README.txt"), README.as_bytes())?;
        let descriptor = Descriptor::new();
        let json = serde_json::to_vec_pretty(&descriptor).expect("descriptor serializes");
        // The descriptor is the commit point of repository creation.
        fsutil::write_atomic(&staging, &root.join(DESCRIPTOR_FILE), &json)?;
        tracing::info!(root = %root.display(), id = %descriptor.repository_id, "created repository");
        Ok(Repository::new(root, descriptor))
    }

    /// Opens a repository given either its own folder or the folder that contains `K-92 Backup`.
    /// The repository is treated as untrusted input.
    pub fn open(path: &Path) -> Result<Self> {
        let root = if path.join(DESCRIPTOR_FILE).is_file() {
            path.to_path_buf()
        } else if path.join(REPO_DIR_NAME).join(DESCRIPTOR_FILE).is_file() {
            path.join(REPO_DIR_NAME)
        } else if !path.exists() {
            return Err(Error::NotFound(format!(
                "backup location {}",
                path.display()
            )));
        } else {
            return Err(Error::NotARepository(path.display().to_string()));
        };
        let p = root.join(DESCRIPTOR_FILE);
        let bytes = fs::read(&p).ctx("read", &p)?;
        if bytes.len() > 64 * 1024 {
            return Err(Error::Corrupt("descriptor too large".into()));
        }
        let descriptor: Descriptor = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Corrupt(format!("{DESCRIPTOR_FILE}: {e}")))?;
        descriptor.validate()?;
        for d in ["objects", "snapshots"] {
            if !root.join(d).is_dir() {
                return Err(Error::Corrupt(format!("missing {d}/ folder")));
            }
        }
        Ok(Repository::new(root, descriptor))
    }

    fn new(root: PathBuf, descriptor: Descriptor) -> Self {
        Repository {
            root,
            descriptor,
            packs: Arc::new(RwLock::new(None)),
        }
    }

    fn packs_dir(&self) -> PathBuf {
        self.root.join("packs")
    }

    /// Runs `f` on the pack index, loading it from disk the first time.
    fn with_packs<R>(&self, f: impl FnOnce(&PackIndex) -> R) -> R {
        if let Some(idx) = self
            .packs
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            return f(idx);
        }
        let mut w = self.packs.write().unwrap_or_else(|p| p.into_inner());
        let idx = w.get_or_insert_with(|| PackIndex::load(&self.packs_dir()));
        f(idx)
    }

    fn with_packs_mut<R>(&self, f: impl FnOnce(&mut PackIndex) -> R) -> R {
        let mut w = self.packs.write().unwrap_or_else(|p| p.into_inner());
        let idx = w.get_or_insert_with(|| PackIndex::load(&self.packs_dir()));
        f(idx)
    }

    /// Forgets the cached pack index (it is re-read on next use).
    fn reload_packs(&self) {
        *self.packs.write().unwrap_or_else(|p| p.into_inner()) = None;
    }

    pub fn pack_location(&self, hash: &ObjectHash) -> Option<PackLoc> {
        self.with_packs(|p| p.objects.get(hash).cloned())
    }

    /// Packs whose index could not be read: (pack id, problem).
    pub fn damaged_packs(&self) -> Vec<(String, String)> {
        self.with_packs(|p| p.damaged.clone())
    }

    /// Returns `Some(path)` if `dest` (or `dest/K-92 Backup`) holds a repository.
    pub fn discover(dest: &Path) -> Option<PathBuf> {
        Repository::open(dest).ok().map(|r| r.root)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn id(&self) -> &str {
        &self.descriptor.repository_id
    }

    pub fn descriptor(&self) -> &Descriptor {
        &self.descriptor
    }

    fn snapshots_dir(&self) -> PathBuf {
        self.root.join("snapshots")
    }

    /// Progress journal of an unfinished backup (advisory; see `k92-backup::checkpoint`).
    /// Lives outside `staging/`, which is cleared whenever a writer opens the repository.
    pub fn checkpoint_path(&self) -> PathBuf {
        self.root.join("checkpoint.jsonl")
    }

    fn staging_dir(&self) -> PathBuf {
        self.root.join("staging")
    }

    pub fn object_path(&self, hash: &ObjectHash) -> PathBuf {
        let hex = hash.to_hex();
        self.root.join("objects").join(&hex[..2]).join(hex)
    }

    pub fn compressed_object_path(&self, hash: &ObjectHash) -> PathBuf {
        self.object_path(hash).with_extension("zst")
    }

    /// Finds an object on disk. A raw copy with the right size wins; otherwise a compressed
    /// copy; otherwise whatever raw file exists (so a wrong-size raw file shows as damaged).
    pub fn locate_object(&self, hash: &ObjectHash, size: u64) -> Option<ObjectFile> {
        if let Some(loc) = self.pack_location(hash) {
            return Some(ObjectFile::Packed {
                path: pack::pack_path(&self.packs_dir(), &loc.pack_id),
                loc,
            });
        }
        let raw = self.object_path(hash);
        let raw_len = fs::metadata(&raw)
            .ok()
            .filter(|m| m.is_file())
            .map(|m| m.len());
        if raw_len == Some(size) {
            return Some(ObjectFile::Raw {
                path: raw,
                len: size,
            });
        }
        let zst = self.compressed_object_path(hash);
        if let Some(m) = fs::metadata(&zst).ok().filter(|m| m.is_file()) {
            return Some(ObjectFile::Zstd {
                path: zst,
                len: m.len(),
            });
        }
        raw_len.map(|len| ObjectFile::Raw { path: raw, len })
    }

    /// True if the object exists and looks intact (cheap check; no hashing).
    pub fn has_object(&self, hash: &ObjectHash, size: u64) -> bool {
        match self.locate_object(hash, size) {
            Some(ObjectFile::Raw { len, .. }) => len == size,
            Some(ObjectFile::Zstd { len, .. }) => len > 0,
            Some(ObjectFile::Packed { loc, .. }) => loc.size == size,
            None => false,
        }
    }

    /// New objects may be written compressed.
    pub fn supports_compression(&self) -> bool {
        self.descriptor.supports_compression()
    }

    /// Committed snapshot ids, oldest first. Files that are not valid snapshot names are ignored.
    pub fn snapshot_ids(&self) -> Result<Vec<SnapshotId>> {
        let dir = self.snapshots_dir();
        let mut ids = Vec::new();
        for ent in fs::read_dir(&dir).ctx("list", &dir)? {
            let ent = ent.ctx("list", &dir)?;
            let name = ent.file_name();
            let Some(name) = name.to_str() else { continue };
            if let Some(stem) = name.strip_suffix(".json")
                && let Ok(id) = SnapshotId::parse(stem)
            {
                ids.push(id);
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Loads and fully validates one snapshot.
    pub fn load_snapshot(&self, id: &SnapshotId) -> Result<Snapshot> {
        let p = self.snapshots_dir().join(id.file_name());
        let bytes = fs::read(&p).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => Error::NotFound(format!("snapshot {id}")),
            _ => Error::io_path("read", &p, e),
        })?;
        manifest::decode(&bytes, id, self.id())
    }

    /// Resolves `"latest"` or a full id.
    pub fn resolve_snapshot(&self, spec: &str) -> Result<SnapshotId> {
        if spec == "latest" {
            return self
                .latest_valid_snapshot()?
                .map(|s| s.id)
                .ok_or_else(|| Error::NotFound("no snapshots yet".into()));
        }
        let id =
            SnapshotId::parse(spec).map_err(|_| Error::NotFound(format!("snapshot {spec}")))?;
        Ok(id)
    }

    /// Loads all snapshots, separating valid ones from damaged ones. Oldest first.
    pub fn load_all(&self) -> Result<(Vec<Snapshot>, Vec<DamagedSnapshot>)> {
        let mut ok = Vec::new();
        let mut bad = Vec::new();
        for id in self.snapshot_ids()? {
            match self.load_snapshot(&id) {
                Ok(s) => ok.push(s),
                Err(e) => bad.push(DamagedSnapshot {
                    id: id.to_string(),
                    error: e.to_string(),
                }),
            }
        }
        Ok((ok, bad))
    }

    /// The newest snapshot that validates. Damaged newer manifests are skipped.
    pub fn latest_valid_snapshot(&self) -> Result<Option<Snapshot>> {
        for id in self.snapshot_ids()?.iter().rev() {
            match self.load_snapshot(id) {
                Ok(s) => return Ok(Some(s)),
                Err(e) => tracing::warn!(%id, error = %e, "skipping damaged snapshot"),
            }
        }
        Ok(None)
    }

    /// Streams an object into `out`, verifying its hash. On `Err`, discard whatever was written.
    pub fn read_object_into(
        &self,
        hash: &ObjectHash,
        out: &mut dyn Write,
        cancel: &CancelToken,
    ) -> Result<u64> {
        // A hash lives either in a pack (small files) or as its own file.
        let raw = self.object_path(hash);
        let zst = self.compressed_object_path(hash);
        let (p, compressed, f): (PathBuf, bool, Box<dyn Read>) =
            if let Some(loc) = self.pack_location(hash) {
                let p = pack::pack_path(&self.packs_dir(), &loc.pack_id);
                let mut f = File::open(&p).map_err(|e| match e.kind() {
                    io::ErrorKind::NotFound => Error::Corrupt(format!("missing pack for {hash}")),
                    _ => Error::io_path("open", &p, e),
                })?;
                f.seek(SeekFrom::Start(loc.offset)).ctx("read", &p)?;
                (p, loc.encoding == Encoding::Zstd, Box::new(f.take(loc.len)))
            } else if raw.is_file() {
                let f = File::open(&raw).ctx("open", &raw)?;
                (raw, false, Box::new(f))
            } else if zst.is_file() {
                let f = File::open(&zst).ctx("open", &zst)?;
                (zst, true, Box::new(f))
            } else {
                return Err(Error::Corrupt(format!("missing object {hash}")));
            };
        let mut reader: Box<dyn Read> = if compressed {
            Box::new(
                zstd::stream::read::Decoder::new(f)
                    .map_err(|e| Error::Corrupt(format!("object {hash}: {e}")))?,
            )
        } else {
            Box::new(f)
        };
        let mut hasher = blake3::Hasher::new();
        let mut buf = vec![0u8; IO_BUF];
        let mut total = 0u64;
        loop {
            cancel.check()?;
            let n = match reader.read(&mut buf) {
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                // Undecodable data in a file that is still there is damage, not an I/O problem.
                Err(e) if compressed && p.is_file() => {
                    return Err(Error::Corrupt(format!(
                        "object {hash} cannot be decompressed: {e}"
                    )));
                }
                Err(e) => return Err(Error::io_path("read", &p, e)),
            };
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n])
                .map_err(|e| Error::io("write restored data", e))?;
            total += n as u64;
        }
        if hasher.finalize().as_bytes() != &hash.0 {
            return Err(Error::Corrupt(format!(
                "object {hash} does not match its hash"
            )));
        }
        Ok(total)
    }

    /// Re-hashes an object on disk. `Ok(false)` means corrupt.
    pub fn check_object(&self, hash: &ObjectHash) -> Result<bool> {
        match self.read_object_into(hash, &mut io::sink(), &CancelToken::new()) {
            Ok(_) => Ok(true),
            Err(Error::Corrupt(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Acquires the exclusive writer lock (invariant I6). Fails fast with `Busy`.
    pub fn writer(&self) -> Result<RepoWriter> {
        let lock_path = self.root.join("lock");
        let mut f = File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .ctx("open", &lock_path)?;
        match f.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(Error::Busy),
            Err(TryLockError::Error(e)) => return Err(Error::io_path("lock", &lock_path, e)),
        }
        // Diagnostics only; the OS lock is what matters.
        let _ = f.set_len(0);
        let _ = writeln!(
            f,
            "pid={} host={}",
            std::process::id(),
            HostInfo::current().hostname
        );
        let w = RepoWriter {
            repo: self.clone(),
            _lock: f,
            pack: Mutex::new(None),
        };
        w.clear_staging()?;
        Ok(w)
    }

    pub fn verify(&self, deep: bool, cancel: &CancelToken) -> Result<k92_core::dto::VerifyReport> {
        verify::verify(self, deep, cancel)
    }

    /// Total size of everything under the repository folder.
    pub fn disk_usage(&self) -> Result<u64> {
        fn walk(p: &Path) -> io::Result<u64> {
            let mut total = 0;
            for e in fs::read_dir(p)? {
                let e = e?;
                let m = e.metadata()?;
                total += if m.is_dir() {
                    walk(&e.path())?
                } else {
                    m.len()
                };
            }
            Ok(total)
        }
        walk(&self.root).ctx("measure", &self.root)
    }
}

/// Holds the writer lock. All mutations go through here.
pub struct RepoWriter {
    repo: Repository,
    _lock: File,
    /// Small objects collect here until the pack is full or flushed.
    pack: Mutex<Option<OpenPack>>,
}

impl std::fmt::Debug for RepoWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepoWriter")
            .field("repo", &self.repo)
            .finish()
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GcReport {
    pub objects_deleted: u64,
    pub bytes_freed: u64,
    pub objects_kept: u64,
    /// Packs kept because some of their objects are still used (space reclaimed by a later
    /// repack, not yet implemented).
    pub packs_partly_used: u64,
}

impl RepoWriter {
    pub fn repo(&self) -> &Repository {
        &self.repo
    }

    fn clear_staging(&self) -> Result<()> {
        let dir = self.repo.staging_dir();
        fs::create_dir_all(&dir).ctx("create", &dir)?;
        for e in fs::read_dir(&dir).ctx("list", &dir)? {
            let e = e.ctx("list", &dir)?;
            if e.file_type().is_ok_and(|t| t.is_file()) {
                let _ = fs::remove_file(e.path());
            }
        }
        Ok(())
    }

    /// Streams `src` into the object store: hash + write to staging, fsync, rename (I1, I3).
    /// With `compress`, data that compresses well is stored zstd-compressed (format v2 only).
    pub fn store_object(
        &self,
        src: &mut dyn Read,
        compress: bool,
        cancel: &CancelToken,
        on_bytes: &mut dyn FnMut(u64),
    ) -> std::result::Result<StoredObject, StoreError> {
        let staging = self.repo.staging_dir();
        let tmp = fsutil::staging_file_path(&staging);
        let compress = compress && self.repo.supports_compression();
        let result = self.store_inner(src, &tmp, compress, cancel, on_bytes);
        if !matches!(result, Ok((_, true))) {
            let _ = fs::remove_file(&tmp);
        }
        result.map(|(o, _)| o)
    }

    fn store_inner(
        &self,
        src: &mut dyn Read,
        tmp: &Path,
        compress: bool,
        cancel: &CancelToken,
        on_bytes: &mut dyn FnMut(u64),
    ) -> std::result::Result<(StoredObject, bool), StoreError> {
        let repo_err = StoreError::Repo;
        let mut buf = vec![0u8; IO_BUF];
        let mut hasher = blake3::Hasher::new();
        let mut size = 0u64;

        // The first chunk decides the encoding.
        cancel.check().map_err(repo_err)?;
        let first = read_full(src, &mut buf).map_err(StoreError::Source)?;
        let zstd_mode = compress
            && first >= MIN_COMPRESS_SIZE
            && worth_compressing(&buf[..first.min(COMPRESS_SAMPLE)]);

        let file = File::create_new(tmp).ctx("create", tmp).map_err(repo_err)?;
        let mut sink = if zstd_mode {
            let mut enc = zstd::stream::write::Encoder::new(file, ZSTD_LEVEL)
                .ctx("compress", tmp)
                .map_err(repo_err)?;
            enc.include_checksum(true)
                .ctx("compress", tmp)
                .map_err(repo_err)?;
            Sink::Zstd(enc)
        } else {
            Sink::Raw(file)
        };

        let mut n = first;
        loop {
            if n > 0 {
                hasher.update(&buf[..n]);
                sink.write_all(&buf[..n])
                    .ctx("write", tmp)
                    .map_err(repo_err)?;
                size += n as u64;
                on_bytes(n as u64);
            }
            if n < buf.len() {
                // A short read from read_full means end of file.
                break;
            }
            cancel.check().map_err(repo_err)?;
            n = read_full(src, &mut buf).map_err(StoreError::Source)?;
        }
        let out = sink.finish().ctx("write", tmp).map_err(repo_err)?;

        let hash = ObjectHash(*hasher.finalize().as_bytes());
        if self.repo.has_object(&hash, size) {
            return Ok((
                StoredObject {
                    hash,
                    size,
                    is_new: false,
                    stored_size: 0,
                },
                false,
            ));
        }
        out.sync_all().ctx("flush", tmp).map_err(repo_err)?;
        let stored_size = out.metadata().map(|m| m.len()).unwrap_or(size);
        drop(out);
        let dest = if zstd_mode {
            self.repo.compressed_object_path(&hash)
        } else {
            self.repo.object_path(&hash)
        };
        let parent = dest.parent().expect("object path has parent");
        fs::create_dir_all(parent)
            .ctx("create", parent)
            .map_err(repo_err)?;
        // If a truncated/damaged object exists (wrong size), this replaces it with good data.
        fsutil::rename_with_retry(tmp, &dest)
            .ctx("store", &dest)
            .map_err(repo_err)?;
        if zstd_mode {
            // A damaged raw copy (wrong size) would otherwise shadow the good compressed one.
            let raw = self.repo.object_path(&hash);
            if fs::metadata(&raw).is_ok_and(|m| m.len() != size) {
                let _ = fs::remove_file(&raw);
            }
        }
        fsutil::sync_dir(parent);
        Ok((
            StoredObject {
                hash,
                size,
                is_new: true,
                stored_size,
            },
            true,
        ))
    }

    /// Stores a small object (already read into memory) in the current pack. The object is
    /// durable only after the pack is committed: by `flush_pack`, when the pack fills up,
    /// or by `commit_snapshot`.
    pub fn store_small(&self, data: &[u8], compress: bool) -> Result<StoredObject> {
        let size = data.len() as u64;
        let hash = ObjectHash(*blake3::hash(data).as_bytes());
        let existing = StoredObject {
            hash,
            size,
            is_new: false,
            stored_size: 0,
        };
        if self.repo.has_object(&hash, size) {
            return Ok(existing);
        }
        let compressed = (compress
            && self.repo.supports_compression()
            && data.len() >= MIN_COMPRESS_SIZE
            && worth_compressing(&data[..data.len().min(COMPRESS_SAMPLE)]))
        .then(|| zstd::bulk::compress(data, ZSTD_LEVEL).ok())
        .flatten()
        .filter(|c| c.len() * 10 < data.len() * 9);
        let (blob, encoding) = match &compressed {
            Some(c) => (c.as_slice(), Encoding::Zstd),
            None => (data, Encoding::Raw),
        };

        let mut guard = self.pack.lock().unwrap_or_else(|p| p.into_inner());
        if guard.as_ref().is_some_and(|p| p.hashes.contains(&hash))
            || self.repo.pack_location(&hash).is_some()
        {
            return Ok(existing);
        }
        if guard.is_none() {
            *guard = Some(OpenPack::create(&self.repo.staging_dir())?);
        }
        let p = guard.as_mut().expect("pack open");
        p.append(hash, size, encoding, blob)?;
        if p.offset >= pack::PACK_TARGET_SIZE {
            let full = guard.take().expect("pack open");
            self.commit_pack(full)?;
        }
        Ok(StoredObject {
            hash,
            size,
            is_new: true,
            stored_size: blob.len() as u64,
        })
    }

    /// Commits the current pack, making every small object stored so far durable.
    pub fn flush_pack(&self) -> Result<()> {
        let open = self.pack.lock().unwrap_or_else(|p| p.into_inner()).take();
        match open {
            Some(p) if !p.entries.is_empty() => self.commit_pack(p),
            Some(p) => {
                let _ = fs::remove_file(&p.tmp);
                Ok(())
            }
            None => Ok(()),
        }
    }

    fn commit_pack(&self, p: OpenPack) -> Result<()> {
        let id = p.id.clone();
        let entries = p.commit(&self.repo.staging_dir(), &self.repo.packs_dir())?;
        self.repo.with_packs_mut(|idx| idx.add(&id, &entries));
        Ok(())
    }

    /// The id the next snapshot will get: now, but always after the newest existing one.
    pub fn next_snapshot_id(&self) -> Result<SnapshotId> {
        let ids = self.repo.snapshot_ids()?;
        Ok(SnapshotId::generate(Utc::now(), ids.last()))
    }

    /// Commit point (I2): manifest written to staging, fsynced, renamed into `snapshots/`.
    /// The caller must have stored every referenced object through this writer first.
    pub fn commit_snapshot(&self, snap: &Snapshot) -> Result<()> {
        snap.validate(&snap.id, self.repo.id())?;
        // Small objects must be durable before the manifest that references them (I3).
        self.flush_pack()?;
        for e in &snap.entries {
            if let Some(h) = &e.hash
                && !self.repo.has_object(h, e.size)
            {
                return Err(Error::Corrupt(format!(
                    "refusing to commit: object for {} missing",
                    e.path
                )));
            }
        }
        let dest = self.repo.snapshots_dir().join(snap.id.file_name());
        if dest.exists() {
            return Err(Error::InvalidRequest(format!(
                "snapshot {} already exists",
                snap.id
            )));
        }
        let bytes = manifest::encode(snap)?;
        fsutil::write_atomic(&self.repo.staging_dir(), &dest, &bytes)?;
        tracing::info!(id = %snap.id, files = snap.stats.files, "committed snapshot");
        Ok(())
    }

    /// Deletes one snapshot manifest. Object space is reclaimed by `gc` (I9).
    pub fn delete_snapshot(&self, id: &SnapshotId) -> Result<()> {
        let p = self.repo.snapshots_dir().join(id.file_name());
        fs::remove_file(&p).ctx("delete", &p)?;
        fsutil::sync_dir(&self.repo.snapshots_dir());
        Ok(())
    }

    /// Deletes objects no committed snapshot references. If *any* manifest is unreadable,
    /// nothing is deleted (I8): a damaged manifest might still reference objects.
    pub fn gc(&self, cancel: &CancelToken) -> Result<GcReport> {
        let (snaps, damaged) = self.repo.load_all()?;
        if let Some(d) = damaged.first() {
            return Err(Error::Corrupt(format!(
                "cleanup skipped because snapshot {} is damaged ({}); run verify",
                d.id, d.error
            )));
        }
        let referenced: HashSet<ObjectHash> = snaps
            .iter()
            .flat_map(|s| s.entries.iter().filter_map(|e| e.hash))
            .collect();
        let mut report = GcReport::default();
        let objects = self.repo.root.join("objects");
        for fan in fs::read_dir(&objects).ctx("list", &objects)? {
            let fan = fan.ctx("list", &objects)?.path();
            if !fan.is_dir() {
                continue;
            }
            for obj in fs::read_dir(&fan).ctx("list", &fan)? {
                cancel.check()?;
                let obj = obj.ctx("list", &fan)?;
                let name = obj.file_name();
                let Some(hash) = name
                    .to_str()
                    .map(|n| n.strip_suffix(".zst").unwrap_or(n))
                    .and_then(|n| ObjectHash::from_hex(n).ok())
                else {
                    continue; // unknown file: never delete what we do not understand
                };
                if referenced.contains(&hash) {
                    report.objects_kept += 1;
                } else {
                    let len = obj.metadata().map(|m| m.len()).unwrap_or(0);
                    fs::remove_file(obj.path()).ctx("delete", &obj.path())?;
                    report.objects_deleted += 1;
                    report.bytes_freed += len;
                }
            }
        }
        self.gc_packs(&referenced, &mut report, cancel)?;
        Ok(report)
    }
}

enum Sink {
    Raw(File),
    Zstd(zstd::stream::write::Encoder<'static, File>),
}

impl Sink {
    fn write_all(&mut self, b: &[u8]) -> io::Result<()> {
        match self {
            Sink::Raw(f) => f.write_all(b),
            Sink::Zstd(e) => e.write_all(b),
        }
    }

    fn finish(self) -> io::Result<File> {
        match self {
            Sink::Raw(f) => Ok(f),
            Sink::Zstd(e) => e.finish(),
        }
    }
}

/// Fills `buf` as far as possible; returns fewer bytes than `buf.len()` only at end of input.
fn read_full(src: &mut dyn Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match src.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Quick test on a sample: store compressed only if it saves at least 10%.
fn worth_compressing(sample: &[u8]) -> bool {
    zstd::bulk::compress(sample, 1).is_ok_and(|c| c.len() * 10 < sample.len() * 9)
}

impl RepoWriter {
    /// Deletes packs none of whose objects are referenced, and leftover packs without an index.
    /// Packs with a damaged index are never touched.
    fn gc_packs(
        &self,
        referenced: &HashSet<ObjectHash>,
        report: &mut GcReport,
        cancel: &CancelToken,
    ) -> Result<()> {
        self.flush_pack()?;
        let dir = self.repo.packs_dir();
        let idx = PackIndex::load(&dir);
        let mut changed = false;
        for (id, hashes) in &idx.packs {
            cancel.check()?;
            let used = hashes.iter().filter(|h| referenced.contains(h)).count() as u64;
            if used > 0 {
                report.objects_kept += used;
                if used < hashes.len() as u64 {
                    report.packs_partly_used += 1;
                }
                continue;
            }
            let pp = pack::pack_path(&dir, id);
            let len = fs::metadata(&pp).map(|m| m.len()).unwrap_or(0);
            // Index first: a pack without an index is ignored by readers and removed later.
            let ip = pack::index_path(&dir, id);
            fs::remove_file(&ip).ctx("delete", &ip)?;
            fs::remove_file(&pp).ctx("delete", &pp)?;
            report.objects_deleted += hashes.len() as u64;
            report.bytes_freed += len;
            changed = true;
        }
        for id in &idx.orphans {
            let pp = pack::pack_path(&dir, id);
            let len = fs::metadata(&pp).map(|m| m.len()).unwrap_or(0);
            fs::remove_file(&pp).ctx("delete", &pp)?;
            report.bytes_freed += len;
            changed = true;
        }
        if changed {
            fsutil::sync_dir(&dir);
            self.repo.reload_packs();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
