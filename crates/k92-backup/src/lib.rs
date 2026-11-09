//! Backup engine: scans source folders and commits an incremental snapshot.
//!
//! Correctness rules:
//! * Source files are only ever opened for reading.
//! * Links/reparse points are never followed.
//! * Source-side problems skip that item (recorded in the manifest); repository-side problems
//!   abort the whole backup without committing anything.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use k92_core::dto::{BackupReport, EntryKind, JobPhase, ProgressEvent, SkipReason, SkippedItemDto};
use k92_core::{CancelToken, Error, IoContext, ProgressFn, RelPath, Result};
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
}

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

    let mut job = Job {
        writer: &writer,
        cancel,
        progress,
        parent_index: HashMap::new(),
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
    progress(event(
        JobPhase::Committing,
        job.stats.files,
        job.bytes_done,
        None,
    ));
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

struct Job<'a> {
    writer: &'a RepoWriter,
    cancel: &'a CancelToken,
    progress: ProgressFn<'a>,
    parent_index: HashMap<(String, RelPath), Entry>,
    rehash_all: bool,
    entries: Vec<Entry>,
    skipped: Vec<SkippedItem>,
    stats: SnapshotStats,
    files_copied: u64,
    files_unchanged: u64,
    bytes_done: u64,
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
            // Directories are pushed in reverse so the walk proceeds in name order.
            children.reverse();
            for (name, path) in children {
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
                        stack.push((path, child_rel));
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
        }
        Ok(())
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
                && let Some(prev) = self.parent_index.get(&(source_key.to_owned(), rel.clone()))
                && prev.size == size
                && prev.modified_ms == mtime
                && mtime.is_some()
                && let Some(hash) = prev.hash
                && self.writer.repo().has_object(&hash, size)
            {
                self.files_unchanged += 1;
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
        (self.progress)(event(
            JobPhase::Copying,
            self.files_copied + self.files_unchanged,
            self.bytes_done,
            Some(rel.to_string()),
        ));
    }
}

fn event(phase: JobPhase, files: u64, bytes: u64, path: Option<String>) -> ProgressEvent {
    ProgressEvent {
        phase,
        files_done: files,
        files_total: None,
        bytes_done: bytes,
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
