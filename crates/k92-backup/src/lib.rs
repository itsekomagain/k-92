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

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use k92_core::dto::{BackupReport, EntryKind, JobPhase, ProgressEvent, SkipReason, SkippedItemDto};
use k92_core::{CancelToken, Error, IoContext, ObjectHash, ProgressFn, RelPath, Result};
use k92_repository::manifest::{
    SNAPSHOT_FORMAT, SNAPSHOT_FORMAT_VERSION, SkippedItem, SnapshotStats,
};
use k92_repository::{Entry, HostInfo, RepoWriter, Repository, Snapshot, SourceRoot, StoreError};
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

#[derive(Debug, Clone, Default)]
pub struct BackupOptions {
    pub sources: Vec<SourceSpec>,
    /// Ignore size+mtime shortcuts and re-read every file ("deep backup").
    pub rehash_all: bool,
    /// Free-space query for the destination. `None` asks the OS; tests substitute their own.
    pub available_space: Option<fn(&Path) -> io::Result<u64>>,
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
        entries: Vec::new(),
        skipped: Vec::new(),
        stats: SnapshotStats::default(),
        files_copied: 0,
        files_unchanged: 0,
        bytes_done: 0,
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
    for root in &roots {
        job.scan_source(root)?;
    }

    // Files in the parent (for the same sources) that no longer exist.
    let now_keys: HashSet<(String, &RelPath)> = job
        .entries
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
    progress(job.event(JobPhase::Committing, job.stats.files, None));
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
        entries: std::mem::take(&mut job.entries),
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
        new_objects: snapshot.stats.new_objects,
        new_bytes: snapshot.stats.new_bytes,
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
    entries: Vec<Entry>,
    skipped: Vec<SkippedItem>,
    stats: SnapshotStats,
    files_copied: u64,
    files_unchanged: u64,
    bytes_done: u64,
}

#[derive(Clone, Copy)]
enum Known {
    Parent(ObjectHash),
    Resumed { hash: ObjectHash, new_object: bool },
}

impl Known {
    fn hash(self) -> ObjectHash {
        match self {
            Known::Parent(h) | Known::Resumed { hash: h, .. } => h,
        }
    }
}

enum FileOutcome {
    Stored(Entry),
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
        let key = source_key(&root.path);
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
                match k92_windows::classify(&meta) {
                    FileClass::Directory => {
                        self.push_dir(sid, child_rel.clone(), &meta);
                        subdirs.push((path, child_rel));
                    }
                    FileClass::Regular => match self.backup_file(&key, sid, &path, &child_rel)? {
                        FileOutcome::Stored(e) => {
                            self.stats.files += 1;
                            self.stats.bytes += e.size;
                            self.entries.push(e);
                        }
                        FileOutcome::Skip(reason, detail) => {
                            self.skip(sid, display, reason, detail)
                        }
                        FileOutcome::Vanished => {}
                    },
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
                    FileClass::Directory => stack.push((path, child_rel)),
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
        })
    }

    fn push_dir(&mut self, source: u32, path: RelPath, meta: &fs::Metadata) {
        self.stats.dirs += 1;
        self.entries.push(Entry {
            source,
            path,
            kind: EntryKind::Dir,
            size: 0,
            modified_ms: mtime_ms(meta),
            hash: None,
            readonly: false,
        });
    }

    fn backup_file(
        &mut self,
        source_key: &str,
        sid: u32,
        path: &Path,
        rel: &RelPath,
    ) -> Result<FileOutcome> {
        // Retry once if the file changes while we read it.
        for attempt in 0..2 {
            let before = match fs::symlink_metadata(path) {
                Ok(m) => m,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(FileOutcome::Vanished),
                Err(e) => {
                    let (r, d) = classify_io(&e);
                    return Ok(FileOutcome::Skip(r, d));
                }
            };
            let size = before.len();
            let mtime = mtime_ms(&before);
            let readonly = before.permissions().readonly();

            if !self.rehash_all
                && let Some(m) = mtime
                && let Some(known) = self.known(source_key, rel, size, m)
                && self.writer.repo().has_object(&known.hash(), size)
            {
                let hash = known.hash();
                match known {
                    Known::Parent(_) => self.files_unchanged += 1,
                    // Stored by the interrupted run: new relative to the parent snapshot.
                    Known::Resumed { new_object, .. } => {
                        self.files_copied += 1;
                        if new_object {
                            self.stats.new_objects += 1;
                            self.stats.new_bytes += size;
                        }
                    }
                }
                self.report(rel);
                return Ok(FileOutcome::Stored(Entry {
                    source: sid,
                    path: rel.clone(),
                    kind: EntryKind::File,
                    size,
                    modified_ms: mtime,
                    hash: Some(hash),
                    readonly,
                }));
            }

            let remaining = self.bytes_needed.saturating_sub(self.stats.new_bytes);
            self.space.ensure(size, remaining)?;
            let mut file = match File::open(path) {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(FileOutcome::Vanished),
                Err(e) => {
                    let (r, d) = classify_io(&e);
                    return Ok(FileOutcome::Skip(r, d));
                }
            };
            let mut bytes = 0u64;
            let stored = match self
                .writer
                .store_object(&mut file, self.cancel, &mut |n| bytes += n)
            {
                Ok(o) => o,
                Err(StoreError::Source(e)) => {
                    let (r, d) = classify_io(&e);
                    return Ok(FileOutcome::Skip(r, d));
                }
                Err(StoreError::Repo(e)) => return Err(e),
            };
            drop(file);
            self.bytes_done += bytes;

            let after = fs::symlink_metadata(path).ok();
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
            if stored.is_new {
                self.stats.new_objects += 1;
                self.stats.new_bytes += stored.size;
                self.space.written_since += stored.size;
            }
            if let (Some(j), Some(m)) = (self.journal.as_mut(), mtime) {
                j.append(&checkpoint::Record {
                    s: source_key.to_owned(),
                    p: rel.clone(),
                    size: stored.size,
                    mtime: m,
                    hash: stored.hash,
                    new: stored.is_new,
                })?;
            }
            self.files_copied += 1;
            self.report(rel);
            return Ok(FileOutcome::Stored(Entry {
                source: sid,
                path: rel.clone(),
                kind: EntryKind::File,
                size: stored.size,
                modified_ms: mtime,
                hash: Some(stored.hash),
                readonly,
            }));
        }
        unreachable!("loop returns on second attempt")
    }

    fn report(&self, rel: &RelPath) {
        (self.progress)(self.event(
            JobPhase::Copying,
            self.files_copied + self.files_unchanged,
            Some(rel.to_string()),
        ));
    }

    fn event(&self, phase: JobPhase, files: u64, path: Option<String>) -> ProgressEvent {
        ProgressEvent {
            phase,
            files_done: files,
            files_total: self.files_total,
            bytes_done: self.bytes_done,
            bytes_total: self.bytes_total,
            current_path: path,
        }
    }
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
