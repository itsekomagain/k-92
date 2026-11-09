//! Backup engine: scans source folders and commits an incremental snapshot.
//!
//! Correctness rules:
//! * Source files are only ever opened for reading.
//! * Links/reparse points are never followed.
//! * Source-side problems skip that item (recorded in the manifest); repository-side problems
//!   abort the whole backup without committing anything.
//! * Before copying, a quick scan sizes the job and checks the destination has room; the room is
//!   re-checked while copying, so a full drive stops the backup with a clear error.
//! * Progress is journaled (`checkpoint`), so an interrupted backup resumes without re-reading
//!   files it already stored.

mod checkpoint;
pub mod exclude;

pub use exclude::{COMMON_JUNK, ExcludeRules};

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

use k92_core::dto::{BackupReport, EntryKind, JobPhase, ProgressEvent, SkipReason, SkippedItemDto};
use k92_core::{CancelToken, Error, IoContext, ObjectHash, ProgressFn, RelPath, Result};
use k92_repository::manifest::{
    SNAPSHOT_FORMAT, SNAPSHOT_FORMAT_VERSION, SkippedItem, SnapshotStats,
};
use k92_repository::{
    Entry, HostInfo, RepoWriter, Repository, SMALL_OBJECT, Snapshot, SourceRoot, StoreError,
    StoredObject,
};
use k92_windows::FileClass;

#[derive(Debug, Clone)]
pub struct SourceSpec {
    pub path: PathBuf,
    pub label: Option<String>,
}

impl SourceSpec {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        SourceSpec {
            path: path.into(),
            label: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BackupOptions {
    pub sources: Vec<SourceSpec>,
    /// Store compressible files zstd-compressed (on format v2 repositories). Default on.
    pub compress: bool,
    /// Files copied at the same time. `None` picks a value from the CPU count.
    pub threads: Option<usize>,
    /// Ignore size+mtime shortcuts and re-read every file ("deep backup").
    pub rehash_all: bool,
    /// Folders, name patterns and size limits to leave out.
    pub exclude: ExcludeRules,
    /// Free-space query for the destination. `None` asks the OS; tests substitute their own.
    pub available_space: Option<fn(&Path) -> io::Result<u64>>,
}

impl Default for BackupOptions {
    fn default() -> Self {
        BackupOptions {
            sources: Vec::new(),
            compress: true,
            threads: None,
            rehash_all: false,
            exclude: ExcludeRules::default(),
            available_space: None,
        }
    }
}

/// Files at least this large go through a single "big file" lane, so a spinning backup drive
/// writes at most one large stream at a time while small files are copied in parallel.
const BIG_FILE: u64 = 8 * 1024 * 1024;
const PROGRESS_TICK: Duration = Duration::from_millis(250);
/// Small files sit in an open pack until it is committed; commit at least this often so the
/// resume journal keeps up during long backups.
const PACK_FLUSH_EVERY: Duration = Duration::from_secs(15);
const PACK_FLUSH_FILES: usize = 5000;

/// File types that are already compressed; trying again only costs CPU time.
const COMPRESSED_EXTENSIONS: &[&str] = &[
    "7z", "aac", "apk", "avi", "avif", "bz2", "cab", "docx", "dmg", "epub", "flac", "gif", "gz",
    "heic", "heif", "jar", "jpeg", "jpg", "lz4", "m4a", "m4v", "mkv", "mov", "mp3", "mp4", "msi",
    "odp", "ods", "odt", "ogg", "opus", "png", "pptx", "rar", "tgz", "webm", "webp", "wma", "wmv",
    "xlsx", "xz", "zip", "zst",
];

fn worth_compressing(name: &str) -> bool {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    !ext.is_some_and(|e| COMPRESSED_EXTENSIONS.contains(&e.as_str()))
}

/// Space kept free on the destination beyond the data itself (plus room for the manifest).
const SPACE_RESERVE: u64 = 64 * 1024 * 1024;
/// While copying, free space is re-queried at least this often.
const SPACE_RECHECK_BYTES: u64 = 1024 * 1024 * 1024;
const SCAN_PROGRESS_EVERY: u64 = 1000;

/// Runs one backup and commits a snapshot. Takes the repository writer lock for its duration.
pub fn run_backup(
    repo: &Repository,
    opts: &BackupOptions,
    cancel: &CancelToken,
    progress: ProgressFn<'_>,
) -> Result<BackupReport> {
    let started = Instant::now();
    let started_at = now_rfc3339();
    progress(event(JobPhase::Preparing, 0, 0, None));

    let roots = validate_sources(repo, &opts.sources)?;
    let writer = repo.writer()?;
    let parent = repo.latest_valid_snapshot()?;
    let id = writer.next_snapshot_id()?;
    let checkpoint_path = repo.checkpoint_path();
    let bytes_read = AtomicU64::new(0);

    let mut job = Job {
        writer: &writer,
        cancel,
        progress,
        parent_index: HashMap::new(),
        resume_index: if opts.rehash_all {
            checkpoint::Index::new()
        } else {
            checkpoint::load(&checkpoint_path)
        },
        known_sizes: HashSet::new(),
        journal: None,
        space: SpaceGuard {
            query: opts.available_space.unwrap_or(k92_windows::available_space),
            root: repo.root().to_owned(),
            reserve: SPACE_RESERVE,
            known_free: 0,
            written_since: 0,
        },
        files_total: None,
        bytes_total: None,
        bytes_needed: 0,
        rehash_all: opts.rehash_all,
        compress: opts.compress,
        exclude: &opts.exclude,
        entries: Vec::new(),
        skipped: Vec::new(),
        stats: SnapshotStats::default(),
        files_copied: 0,
        files_unchanged: 0,
        bytes_read: &bytes_read,
        dispatched_bytes: 0,
        pool: None,
        unflushed: Vec::new(),
        last_pack_flush: Instant::now(),
    };
    if let Some(p) = &parent {
        for e in p.entries.iter().filter(|e| e.kind == EntryKind::File) {
            if let Some(src) = p.source(e.source) {
                job.parent_index
                    .insert((source_key(&src.path), e.path.clone()), e.clone());
            }
        }
    }
    job.known_sizes = job
        .parent_index
        .values()
        .filter_map(|e| Some((e.size, e.modified_ms?)))
        .chain(job.resume_index.values().map(|r| (r.size, r.mtime)))
        .collect();
    if !job.resume_index.is_empty() {
        tracing::info!(
            files = job.resume_index.len(),
            "resuming an interrupted backup"
        );
    }

    // Size the job and make sure it fits before writing anything.
    let mut totals = ScanTotals::default();
    for root in &roots {
        job.prescan(root, &mut totals)?;
    }
    job.files_total = Some(totals.files);
    job.bytes_total = Some(totals.bytes_to_read);
    job.bytes_needed = totals.bytes_needed;
    job.space.reserve = SPACE_RESERVE + totals.files.saturating_mul(512);
    job.space.check_upfront(totals.bytes_needed)?;

    job.journal = Some(checkpoint::Journal::open(&checkpoint_path)?);
    let small_workers = opts.threads.unwrap_or_else(default_threads).max(1);
    let abort = cancel.child();
    std::thread::scope(|scope| -> Result<()> {
        let (small_tx, small_rx) = mpsc::sync_channel::<Work>(small_workers * 16);
        let (big_tx, big_rx) = mpsc::sync_channel::<Work>(1);
        let (done_tx, done_rx) = mpsc::channel::<Done>();
        let small_rx = Arc::new(Mutex::new(small_rx));
        let big_rx = Arc::new(Mutex::new(big_rx));
        let lanes = (0..small_workers)
            .map(|_| small_rx.clone())
            .chain(std::iter::once(big_rx));
        for rx in lanes {
            let done = done_tx.clone();
            let (writer, abort, bytes_read) = (&writer, &abort, &bytes_read);
            scope.spawn(move || copy_worker(writer, rx, done, abort, bytes_read));
        }
        drop(done_tx);
        job.pool = Some(Pool {
            small: small_tx,
            big: big_tx,
            done: done_rx,
            in_flight: 0,
        });

        let result = (|| {
            for root in &roots {
                job.scan_source(root)?;
            }
            job.drain(true)
        })();
        if result.is_err() {
            abort.cancel();
        }
        // Close the queues and collect what the workers finish; completed files are still
        // journaled so a retry does not read them again.
        if let Some(pool) = job.pool.take() {
            let Pool {
                small, big, done, ..
            } = pool;
            drop((small, big));
            for d in done {
                if result.is_err() {
                    job.journal_only(d);
                }
            }
        }
        if result.is_err() {
            // Keep the small files stored so far: commit their pack, then journal them.
            if writer.flush_pack().is_ok() {
                let _ = job.journal_unflushed();
            }
        }
        result
    })?;

    // Files in the parent (for the same sources) that no longer exist.
    let entries: Vec<Entry> = std::mem::take(&mut job.entries)
        .into_iter()
        .flatten()
        .collect();
    let now_keys: HashSet<(String, &RelPath)> = entries
        .iter()
        .filter(|e| e.kind == EntryKind::File)
        .map(|e| (source_key(&roots[e.source as usize].path), &e.path))
        .collect();
    let current_sources: HashSet<String> = roots.iter().map(|r| source_key(&r.path)).collect();
    let files_removed = job
        .parent_index
        .keys()
        .filter(|(s, p)| current_sources.contains(s) && !now_keys.contains(&(s.clone(), p)))
        .count() as u64;

    cancel.check()?;
    progress(job.event(JobPhase::Committing, job.files_done(), None));
    job.stats.skipped = job.skipped.len() as u64;
    let snapshot = Snapshot {
        format: SNAPSHOT_FORMAT.into(),
        format_version: SNAPSHOT_FORMAT_VERSION,
        id: id.clone(),
        repository_id: repo.id().into(),
        parent_id: parent.map(|p| p.id),
        started_at,
        completed_at: now_rfc3339(),
        host: HostInfo::current(),
        sources: roots.iter().map(|r| r.root.clone()).collect(),
        entries,
        skipped: std::mem::take(&mut job.skipped),
        stats: job.stats.clone(),
    };
    writer.commit_snapshot(&snapshot)?;
    if let Some(j) = job.journal.take() {
        j.finish();
    }

    Ok(BackupReport {
        snapshot_id: id.to_string(),
        files: snapshot.stats.files,
        dirs: snapshot.stats.dirs,
        bytes: snapshot.stats.bytes,
        files_copied: job.files_copied,
        files_unchanged: job.files_unchanged,
        files_removed,
        excluded: snapshot.stats.excluded,
        new_objects: snapshot.stats.new_objects,
        new_bytes: snapshot.stats.new_bytes,
        new_stored_bytes: snapshot.stats.new_stored_bytes,
        skipped: snapshot
            .skipped
            .iter()
            .map(|s| SkippedItemDto {
                source_path: snapshot.sources[s.source as usize].path.clone(),
                path: s.path.clone(),
                reason: s.reason,
                detail: s.detail.clone(),
            })
            .collect(),
        duration_ms: started.elapsed().as_millis() as u64,
    })
}

struct ValidRoot {
    path: PathBuf,
    root: SourceRoot,
}

fn validate_sources(repo: &Repository, sources: &[SourceSpec]) -> Result<Vec<ValidRoot>> {
    if sources.is_empty() {
        return Err(Error::InvalidRequest(
            "choose at least one folder to back up".into(),
        ));
    }
    let repo_canon = fs::canonicalize(repo.root()).ctx("resolve", repo.root())?;
    let mut out: Vec<ValidRoot> = Vec::new();
    let mut canon_roots: Vec<PathBuf> = Vec::new();
    for (i, s) in sources.iter().enumerate() {
        let abs = std::path::absolute(&s.path).ctx("resolve", &s.path)?;
        let meta = fs::symlink_metadata(&abs).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => Error::NotFound(format!("source folder {}", abs.display())),
            _ => Error::io_path("open source folder", &abs, e),
        })?;
        if k92_windows::classify(&meta) != FileClass::Directory {
            return Err(Error::InvalidRequest(format!(
                "{} is not a folder",
                abs.display()
            )));
        }
        let canon = fs::canonicalize(&abs).ctx("resolve", &abs)?;
        if canon.starts_with(&repo_canon) || repo_canon.starts_with(&canon) {
            return Err(Error::InvalidRequest(format!(
                "{} overlaps the backup destination; choose a destination outside your source folders",
                abs.display()
            )));
        }
        if let Some(other) = canon_roots
            .iter()
            .find(|o| canon.starts_with(o) || o.starts_with(&canon))
        {
            return Err(Error::InvalidRequest(format!(
                "{} overlaps {}; choose non-nested folders",
                abs.display(),
                other.display()
            )));
        }
        canon_roots.push(canon);
        let label = s
            .label
            .clone()
            .or_else(|| abs.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| abs.display().to_string());
        out.push(ValidRoot {
            root: SourceRoot {
                id: i as u32,
                path: abs.display().to_string(),
                label,
            },
            path: abs,
        });
    }
    Ok(out)
}

fn source_key(path: impl AsRef<Path>) -> String {
    k92_repository::manifest::source_key(&path.as_ref().display().to_string())
}

#[derive(Default)]
struct ScanTotals {
    files: u64,
    /// Bytes of files that are new or changed since the last backup (they will be read).
    bytes_to_read: u64,
    /// Upper estimate of new space needed: `bytes_to_read` minus files that look moved or
    /// renamed (same size and modified time as a backed-up file), which deduplicate.
    bytes_needed: u64,
}

struct SpaceGuard {
    query: fn(&Path) -> io::Result<u64>,
    root: PathBuf,
    reserve: u64,
    known_free: u64,
    written_since: u64,
}

impl SpaceGuard {
    /// Free bytes on the destination, or `u64::MAX` if the OS cannot tell (check skipped).
    fn refresh(&mut self) -> u64 {
        self.written_since = 0;
        self.known_free = match (self.query)(&self.root) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "cannot read free space; skipping space check");
                u64::MAX
            }
        };
        self.known_free
    }

    fn check_upfront(&mut self, needed: u64) -> Result<()> {
        let free = self.refresh();
        let needed = needed.saturating_add(self.reserve);
        if free < needed {
            return Err(Error::InsufficientSpace {
                needed,
                available: free,
            });
        }
        Ok(())
    }

    /// Called before writing a file of `upcoming` bytes; `remaining` is the estimated total
    /// still to be written, used for the error message.
    fn ensure(&mut self, upcoming: u64, remaining: u64) -> Result<()> {
        let need = upcoming.saturating_add(self.reserve);
        if self.written_since >= SPACE_RECHECK_BYTES
            || self.known_free.saturating_sub(self.written_since) < need
        {
            let free = self.refresh();
            if free < need {
                return Err(Error::InsufficientSpace {
                    needed: remaining.max(upcoming).saturating_add(self.reserve),
                    available: free,
                });
            }
        }
        Ok(())
    }
}

struct Job<'a> {
    writer: &'a RepoWriter,
    cancel: &'a CancelToken,
    progress: ProgressFn<'a>,
    parent_index: HashMap<(String, RelPath), Entry>,
    /// Files stored by an interrupted earlier run (from the checkpoint journal).
    resume_index: checkpoint::Index,
    /// (size, modified) of every backed-up file, to spot moved/renamed files while sizing.
    known_sizes: HashSet<(u64, i64)>,
    journal: Option<checkpoint::Journal>,
    space: SpaceGuard,
    files_total: Option<u64>,
    bytes_total: Option<u64>,
    bytes_needed: u64,
    rehash_all: bool,
    compress: bool,
    exclude: &'a ExcludeRules,
    /// In walk order; `None` marks a file still being copied (or skipped in the end).
    entries: Vec<Option<Entry>>,
    skipped: Vec<SkippedItem>,
    stats: SnapshotStats,
    files_copied: u64,
    files_unchanged: u64,
    /// Bytes read from source files so far (updated by the copy workers).
    bytes_read: &'a AtomicU64,
    /// Bytes handed to the workers, for the space estimate.
    dispatched_bytes: u64,
    pool: Option<Pool>,
    /// Journal records for packed files, written once their pack is committed.
    unflushed: Vec<checkpoint::Record>,
    last_pack_flush: Instant,
}

struct Pool {
    small: SyncSender<Work>,
    big: SyncSender<Work>,
    done: Receiver<Done>,
    in_flight: usize,
}

/// One file for a copy worker. `size`/`mtime` come from the scan and are re-checked after reading.
struct Work {
    slot: usize,
    sid: u32,
    key: Arc<str>,
    path: PathBuf,
    rel: RelPath,
    display: String,
    size: u64,
    mtime: Option<i64>,
    readonly: bool,
    compress: bool,
}

struct Done {
    work: Work,
    outcome: Result<FileOutcome>,
}

fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1))
        .unwrap_or(2)
        .clamp(1, 3)
}

#[derive(Clone, Copy)]
enum Known {
    Parent(ObjectHash),
    Resumed {
        hash: ObjectHash,
        new_object: bool,
        stored_size: u64,
    },
}

impl Known {
    fn hash(self) -> ObjectHash {
        match self {
            Known::Parent(h) | Known::Resumed { hash: h, .. } => h,
        }
    }
}

enum FileOutcome {
    /// `packed`: stored in the open pack; durable only after the pack is committed.
    Stored {
        entry: Entry,
        stored: StoredObject,
        packed: bool,
    },
    Skip(SkipReason, Option<String>),
    Vanished,
}

impl Job<'_> {
    fn skip(&mut self, source: u32, path: String, reason: SkipReason, detail: Option<String>) {
        tracing::debug!(%path, ?reason, ?detail, "skipped");
        self.skipped.push(SkippedItem {
            source,
            path,
            reason,
            detail,
        });
    }

    fn scan_source(&mut self, root: &ValidRoot) -> Result<()> {
        let sid = root.root.id;
        let key: Arc<str> = source_key(&root.path).into();
        let meta = fs::metadata(&root.path).ctx("read source folder", &root.path)?;
        self.push_dir(sid, RelPath::root(), &meta);

        let mut stack = vec![(root.path.clone(), RelPath::root())];
        while let Some((dir, rel)) = stack.pop() {
            self.cancel.check()?;
            let mut children = match read_dir_sorted(&dir) {
                Ok(c) => c,
                Err(e) if rel.is_root() => {
                    // Source root unreadable: abort rather than record "everything deleted".
                    return Err(Error::io_path("read source folder", &dir, e));
                }
                Err(e) => {
                    let (reason, detail) = classify_io(&e);
                    self.skip(sid, rel.to_string(), reason, detail);
                    continue;
                }
            };
            // Files are processed in name order; subfolders are queued and then pushed in
            // reverse so they are also visited in name order.
            let mut subdirs = Vec::new();
            for (name, path) in children.drain(..) {
                self.cancel.check()?;
                let display = join_display(&rel, &name);
                let Some(name_str) = name.to_str() else {
                    self.skip(
                        sid,
                        display,
                        SkipReason::InvalidName,
                        Some("not valid Unicode".into()),
                    );
                    continue;
                };
                let Ok(child_rel) = rel.join(name_str) else {
                    self.skip(sid, display, SkipReason::InvalidName, None);
                    continue;
                };
                let meta = match fs::symlink_metadata(&path) {
                    Ok(m) => m,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => {
                        let (reason, detail) = classify_io(&e);
                        self.skip(sid, display, reason, detail);
                        continue;
                    }
                };
                let class = k92_windows::classify(&meta);
                let left_out = match class {
                    FileClass::Directory => self.exclude.excludes_dir(&path, name_str),
                    FileClass::Regular => self.exclude.excludes_file(&path, name_str, meta.len()),
                    _ => self.exclude.excludes_dir(&path, name_str),
                };
                if left_out {
                    self.stats.excluded += 1;
                    continue;
                }
                match class {
                    FileClass::Directory => {
                        self.push_dir(sid, child_rel.clone(), &meta);
                        subdirs.push((path, child_rel));
                    }
                    FileClass::Regular => {
                        let compress = self.compress && worth_compressing(name_str);
                        self.handle_file(Work {
                            slot: 0,
                            sid,
                            key: key.clone(),
                            path,
                            rel: child_rel,
                            display,
                            size: meta.len(),
                            mtime: mtime_ms(&meta),
                            readonly: meta.permissions().readonly(),
                            compress,
                        })?;
                    }
                    FileClass::Link => self.skip(sid, display, SkipReason::ReparsePoint, None),
                    FileClass::CloudPlaceholder => {
                        self.skip(sid, display, SkipReason::CloudPlaceholder, None)
                    }
                    FileClass::Other => self.skip(sid, display, SkipReason::UnsupportedType, None),
                }
            }
            stack.extend(subdirs.into_iter().rev());
        }
        Ok(())
    }

    /// Walks a source like `scan_source` but only reads metadata, to size the job.
    /// Problems are ignored here; the real scan reports them.
    fn prescan(&mut self, root: &ValidRoot, totals: &mut ScanTotals) -> Result<()> {
        let key = source_key(&root.path);
        let mut stack = vec![(root.path.clone(), RelPath::root())];
        while let Some((dir, rel)) = stack.pop() {
            self.cancel.check()?;
            let Ok(children) = read_dir_sorted(&dir) else {
                continue;
            };
            for (name, path) in children {
                let Some(child_rel) = name.to_str().and_then(|n| rel.join(n).ok()) else {
                    continue;
                };
                let Ok(meta) = fs::symlink_metadata(&path) else {
                    continue;
                };
                match k92_windows::classify(&meta) {
                    FileClass::Directory => {
                        if !self.exclude.excludes_dir(&path, &name.to_string_lossy()) {
                            stack.push((path, child_rel));
                        }
                    }
                    FileClass::Regular
                        if self.exclude.excludes_file(
                            &path,
                            &name.to_string_lossy(),
                            meta.len(),
                        ) => {}
                    FileClass::Regular => {
                        let size = meta.len();
                        let mtime = mtime_ms(&meta);
                        totals.files += 1;
                        let unchanged =
                            mtime.is_some_and(|m| self.known(&key, &child_rel, size, m).is_some());
                        if self.rehash_all || !unchanged {
                            totals.bytes_to_read += size;
                        }
                        let looks_known =
                            mtime.is_some_and(|m| self.known_sizes.contains(&(size, m)));
                        if !looks_known {
                            totals.bytes_needed += size;
                        }
                        if totals.files.is_multiple_of(SCAN_PROGRESS_EVERY) {
                            (self.progress)(ProgressEvent {
                                phase: JobPhase::Scanning,
                                files_done: totals.files,
                                files_total: None,
                                bytes_done: 0,
                                bytes_total: None,
                                current_path: Some(child_rel.to_string()),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    /// A previous backup or the interrupted run already stored this exact file version.
    fn known(&self, key: &str, rel: &RelPath, size: u64, mtime: i64) -> Option<Known> {
        let k = (key.to_owned(), rel.clone());
        if let Some(prev) = self.parent_index.get(&k)
            && prev.size == size
            && prev.modified_ms == Some(mtime)
            && let Some(hash) = prev.hash
        {
            return Some(Known::Parent(hash));
        }
        let r = self.resume_index.get(&k)?;
        (r.size == size && r.mtime == mtime).then_some(Known::Resumed {
            hash: r.hash,
            new_object: r.new,
            stored_size: r.stored,
        })
    }

    fn push_dir(&mut self, source: u32, path: RelPath, meta: &fs::Metadata) {
        self.stats.dirs += 1;
        self.entries.push(Some(Entry {
            source,
            path,
            kind: EntryKind::Dir,
            size: 0,
            modified_ms: mtime_ms(meta),
            hash: None,
            readonly: false,
        }));
    }

    /// Records an unchanged file directly, or queues it for a copy worker.
    fn handle_file(&mut self, mut w: Work) -> Result<()> {
        if !self.rehash_all
            && let Some(m) = w.mtime
            && let Some(known) = self.known(&w.key, &w.rel, w.size, m)
            && self.writer.repo().has_object(&known.hash(), w.size)
        {
            match known {
                Known::Parent(_) => self.files_unchanged += 1,
                // Stored by the interrupted run: new relative to the parent snapshot.
                Known::Resumed {
                    new_object,
                    stored_size,
                    ..
                } => {
                    self.files_copied += 1;
                    if new_object {
                        self.stats.new_objects += 1;
                        self.stats.new_bytes += w.size;
                        self.stats.new_stored_bytes += stored_size;
                    }
                }
            }
            self.stats.files += 1;
            self.stats.bytes += w.size;
            self.report(&w.rel);
            self.entries.push(Some(Entry {
                source: w.sid,
                path: w.rel,
                kind: EntryKind::File,
                size: w.size,
                modified_ms: w.mtime,
                hash: Some(known.hash()),
                readonly: w.readonly,
            }));
            return self.drain(false);
        }

        let remaining = self.bytes_needed.saturating_sub(self.dispatched_bytes);
        self.space.ensure(w.size, remaining)?;
        self.space.written_since += w.size;
        self.dispatched_bytes += w.size;
        w.slot = self.entries.len();
        self.entries.push(None);
        let pool = self.pool.as_mut().expect("workers running");
        let lane = if w.size >= BIG_FILE {
            &pool.big
        } else {
            &pool.small
        };
        // Workers only stop after the queues close, so a full queue drains.
        lane.send(w).map_err(|_| Error::Io {
            context: "backup workers stopped".into(),
            source: io::Error::other("worker channel closed"),
        })?;
        pool.in_flight += 1;
        self.drain(false)
    }

    /// Processes finished copies. With `wait`, blocks until every queued file is done.
    fn drain(&mut self, wait: bool) -> Result<()> {
        loop {
            let pool = self.pool.as_mut().expect("workers running");
            if pool.in_flight == 0 {
                return Ok(());
            }
            let d = if wait {
                match pool.done.recv_timeout(PROGRESS_TICK) {
                    Ok(d) => d,
                    Err(RecvTimeoutError::Timeout) => {
                        self.cancel.check()?;
                        self.maybe_flush_packs()?;
                        // Keep the progress bar moving during long files.
                        (self.progress)(self.event(JobPhase::Copying, self.files_done(), None));
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => return Ok(()),
                }
            } else {
                match pool.done.try_recv() {
                    Ok(d) => d,
                    Err(_) => return Ok(()),
                }
            };
            pool.in_flight -= 1;
            self.finish(d)?;
        }
    }

    fn finish(&mut self, d: Done) -> Result<()> {
        let Done { work: w, outcome } = d;
        match outcome {
            Err(e) => Err(self.explain_repo_error(e)),
            Ok(FileOutcome::Stored {
                entry,
                stored,
                packed,
            }) => {
                if stored.is_new {
                    self.stats.new_objects += 1;
                    self.stats.new_bytes += stored.size;
                    self.stats.new_stored_bytes += stored.stored_size;
                }
                self.journal(&w, &stored, packed)?;
                self.maybe_flush_packs()?;
                self.files_copied += 1;
                self.stats.files += 1;
                self.stats.bytes += entry.size;
                self.report(&w.rel);
                self.entries[w.slot] = Some(entry);
                Ok(())
            }
            Ok(FileOutcome::Skip(reason, detail)) => {
                self.skip(w.sid, w.display, reason, detail);
                Ok(())
            }
            Ok(FileOutcome::Vanished) => Ok(()),
        }
    }

    /// After a failure: keep the resume journal complete for files that did get stored.
    fn journal_only(&mut self, d: Done) {
        if let Ok(FileOutcome::Stored { stored, packed, .. }) = &d.outcome {
            let _ = self.journal(&d.work, stored, *packed);
        }
    }

    /// Records a stored file in the resume journal. Packed files wait for their pack commit.
    fn journal(&mut self, w: &Work, stored: &StoredObject, packed: bool) -> Result<()> {
        let Some(m) = w.mtime else { return Ok(()) };
        let rec = checkpoint::Record {
            s: w.key.to_string(),
            p: w.rel.clone(),
            size: stored.size,
            mtime: m,
            hash: stored.hash,
            new: stored.is_new,
            stored: stored.stored_size,
        };
        if packed {
            self.unflushed.push(rec);
        } else if let Some(j) = self.journal.as_mut() {
            j.append(&rec)?;
        }
        Ok(())
    }

    /// Commits the open pack now and then, so the journal covers small files too.
    fn maybe_flush_packs(&mut self) -> Result<()> {
        if self.unflushed.is_empty()
            || (self.unflushed.len() < PACK_FLUSH_FILES
                && self.last_pack_flush.elapsed() < PACK_FLUSH_EVERY)
        {
            return Ok(());
        }
        self.writer.flush_pack()?;
        self.last_pack_flush = Instant::now();
        self.journal_unflushed()
    }

    /// Only call right after a successful `flush_pack`.
    fn journal_unflushed(&mut self) -> Result<()> {
        if let Some(j) = self.journal.as_mut() {
            for rec in self.unflushed.drain(..) {
                j.append(&rec)?;
            }
        }
        Ok(())
    }

    /// A full disk reported by the OS mid-write becomes the friendly "not enough space" error.
    fn explain_repo_error(&mut self, e: Error) -> Error {
        let full = matches!(&e, Error::Io { source, .. }
            if source.kind() == io::ErrorKind::StorageFull
                || matches!(source.raw_os_error(), Some(112) | Some(28)));
        if full {
            let available = self.space.refresh().min(u64::MAX / 2);
            Error::InsufficientSpace {
                needed: self
                    .bytes_needed
                    .saturating_sub(self.stats.new_bytes)
                    .saturating_add(self.space.reserve),
                available,
            }
        } else {
            e
        }
    }

    fn files_done(&self) -> u64 {
        self.files_copied + self.files_unchanged
    }

    fn report(&self, rel: &RelPath) {
        (self.progress)(self.event(JobPhase::Copying, self.files_done(), Some(rel.to_string())));
    }

    fn event(&self, phase: JobPhase, files: u64, path: Option<String>) -> ProgressEvent {
        ProgressEvent {
            phase,
            files_done: files,
            files_total: self.files_total,
            bytes_done: self.bytes_read.load(Ordering::Relaxed),
            bytes_total: self.bytes_total,
            current_path: path,
        }
    }
}

/// Copy thread: takes files from its lane until the queue closes.
fn copy_worker(
    writer: &RepoWriter,
    rx: Arc<Mutex<Receiver<Work>>>,
    done: Sender<Done>,
    abort: &CancelToken,
    bytes_read: &AtomicU64,
) {
    loop {
        let next = rx.lock().map(|r| r.recv());
        let Ok(Ok(work)) = next else { return };
        let outcome = if abort.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            copy_file(writer, &work, abort, bytes_read)
        };
        if done.send(Done { work, outcome }).is_err() {
            return;
        }
    }
}

/// Reads one source file into the repository. Source problems skip the file; repository
/// problems are errors. Retries once if the file changes while being read.
fn copy_file(
    writer: &RepoWriter,
    w: &Work,
    cancel: &CancelToken,
    bytes_read: &AtomicU64,
) -> Result<FileOutcome> {
    let (mut size, mut mtime, mut readonly) = (w.size, w.mtime, w.readonly);
    for attempt in 0..2 {
        if attempt > 0 {
            match fs::symlink_metadata(&w.path) {
                Ok(m) => {
                    (size, mtime, readonly) = (m.len(), mtime_ms(&m), m.permissions().readonly())
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(FileOutcome::Vanished),
                Err(e) => {
                    let (r, d) = classify_io(&e);
                    return Ok(FileOutcome::Skip(r, d));
                }
            }
        }
        let mut file = match File::open(&w.path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(FileOutcome::Vanished),
            Err(e) => {
                let (r, d) = classify_io(&e);
                return Ok(FileOutcome::Skip(r, d));
            }
        };
        let packed = size < SMALL_OBJECT;
        let stored = if packed {
            // Small file: read it whole (one byte extra to notice growth), then pack it.
            cancel.check()?;
            let mut data = Vec::with_capacity(size as usize + 1);
            if let Err(e) = (&mut file).take(SMALL_OBJECT).read_to_end(&mut data) {
                let (r, d) = classify_io(&e);
                return Ok(FileOutcome::Skip(r, d));
            }
            bytes_read.fetch_add(data.len() as u64, Ordering::Relaxed);
            if data.len() as u64 != size {
                // Changed while reading: the size check below retries or skips it.
                StoredObject {
                    hash: k92_core::ObjectHash([0; 32]),
                    size: data.len() as u64,
                    is_new: false,
                    stored_size: 0,
                }
            } else {
                writer.store_small(&data, w.compress)?
            }
        } else {
            match writer.store_object(&mut file, w.compress, cancel, &mut |n| {
                bytes_read.fetch_add(n, Ordering::Relaxed);
            }) {
                Ok(o) => o,
                Err(StoreError::Source(e)) => {
                    let (r, d) = classify_io(&e);
                    return Ok(FileOutcome::Skip(r, d));
                }
                Err(StoreError::Repo(e)) => return Err(e),
            }
        };
        drop(file);

        let after = fs::symlink_metadata(&w.path).ok();
        let unchanged = after
            .as_ref()
            .is_some_and(|a| a.len() == size && mtime_ms(a) == mtime && stored.size == size);
        if !unchanged {
            if attempt == 0 {
                continue;
            }
            return Ok(FileOutcome::Skip(
                SkipReason::ChangedDuringRead,
                Some("file was being modified; it will be retried next backup".into()),
            ));
        }
        return Ok(FileOutcome::Stored {
            entry: Entry {
                source: w.sid,
                path: w.rel.clone(),
                kind: EntryKind::File,
                size: stored.size,
                modified_ms: mtime,
                hash: Some(stored.hash),
                readonly,
            },
            stored,
            packed,
        });
    }
    unreachable!("loop returns on second attempt")
}

fn event(phase: JobPhase, files: u64, bytes: u64, path: Option<String>) -> ProgressEvent {
    ProgressEvent {
        phase,
        files_done: files,
        files_total: None,
        bytes_done: bytes,
        bytes_total: None,
        current_path: path,
    }
}

fn read_dir_sorted(dir: &Path) -> io::Result<Vec<(std::ffi::OsString, PathBuf)>> {
    let mut v = Vec::new();
    for e in fs::read_dir(dir)? {
        let e = e?;
        v.push((e.file_name(), e.path()));
    }
    v.sort();
    Ok(v)
}

fn join_display(rel: &RelPath, name: &std::ffi::OsStr) -> String {
    let n = name.to_string_lossy();
    if rel.is_root() {
        n.into_owned()
    } else {
        format!("{rel}/{n}")
    }
}

fn classify_io(e: &io::Error) -> (SkipReason, Option<String>) {
    if k92_windows::is_locked_error(e) {
        (SkipReason::Locked, Some("in use by another program".into()))
    } else if e.kind() == io::ErrorKind::PermissionDenied {
        (SkipReason::AccessDenied, None)
    } else {
        (SkipReason::ReadError, Some(e.to_string()))
    }
}

fn mtime_ms(m: &fs::Metadata) -> Option<i64> {
    let t = m.modified().ok()?;
    Some(match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    })
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
