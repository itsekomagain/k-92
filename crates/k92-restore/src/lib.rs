//! Restore engine: browsing, searching, version history and conflict-safe restoration.
//!
//! Safety rules:
//! * Never silently overwrite: existing files are only replaced under `ConflictPolicy::Overwrite`.
//! * Directories are never deleted or replaced.
//! * Every restored file is hashed from disk and must match the snapshot before it is renamed
//!   into place.
//! * Target paths are built only from validated `RelPath`s, so they cannot escape the target.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use chrono::{Local, TimeZone};

use k92_core::dto::{
    BrowseEntry, ConflictKind, ConflictPolicy, EntryKind, FileVersion, JobPhase, ProgressEvent,
    RestoreConflict, RestoreFailure, RestorePlan, RestoreReport,
};
use k92_core::{CancelToken, Error, IoContext, ObjectHash, ProgressFn, RelPath, Result};
use k92_repository::manifest::source_key;
use k92_repository::{Entry, Repository, Snapshot};

// ---------------------------------------------------------------------------------------------
// Browsing

/// Directory index over one snapshot for fast listing.
pub struct SnapshotView<'a> {
    snap: &'a Snapshot,
    by_path: HashMap<(u32, &'a RelPath), usize>,
    children: HashMap<(u32, RelPath), Vec<usize>>,
}

impl<'a> SnapshotView<'a> {
    pub fn new(snap: &'a Snapshot) -> Self {
        let mut by_path = HashMap::with_capacity(snap.entries.len());
        let mut children: HashMap<(u32, RelPath), Vec<usize>> = HashMap::new();
        for (i, e) in snap.entries.iter().enumerate() {
            by_path.insert((e.source, &e.path), i);
            if let Some(parent) = e.path.parent() {
                children.entry((e.source, parent)).or_default().push(i);
            }
        }
        SnapshotView {
            snap,
            by_path,
            children,
        }
    }

    pub fn snapshot(&self) -> &Snapshot {
        self.snap
    }

    pub fn entry(&self, source: u32, path: &RelPath) -> Option<&'a Entry> {
        self.by_path
            .get(&(source, path))
            .map(|&i| &self.snap.entries[i])
    }

    /// Lists a directory: folders first, then files, each alphabetically (case-insensitive).
    pub fn list_dir(&self, source: u32, path: &RelPath) -> Result<Vec<BrowseEntry>> {
        match self.entry(source, path) {
            Some(e) if e.kind == EntryKind::Dir => {}
            Some(_) => return Err(Error::InvalidRequest(format!("{path} is a file"))),
            None => {
                return Err(Error::NotFound(format!(
                    "{path} in snapshot {}",
                    self.snap.id
                )));
            }
        }
        let mut out: Vec<BrowseEntry> = self
            .children
            .get(&(source, path.clone()))
            .map(|v| {
                v.iter()
                    .map(|&i| to_browse(&self.snap.entries[i]))
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by(|a, b| {
            (a.kind != EntryKind::Dir, a.name.to_lowercase())
                .cmp(&(b.kind != EntryKind::Dir, b.name.to_lowercase()))
        });
        Ok(out)
    }

    /// Case-insensitive substring search on file and folder names.
    pub fn search(&self, query: &str, limit: usize) -> Vec<BrowseEntry> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        self.snap
            .entries
            .iter()
            .filter(|e| {
                e.path
                    .file_name()
                    .is_some_and(|n| n.to_lowercase().contains(&q))
            })
            .take(limit)
            .map(to_browse)
            .collect()
    }

    /// All entries under `path` (inclusive), parents before children.
    fn subtree(&self, source: u32, path: &RelPath) -> Vec<&'a Entry> {
        let mut v: Vec<&Entry> = self
            .snap
            .entries
            .iter()
            .filter(|e| e.source == source && e.path.starts_with(path))
            .collect();
        v.sort_by(|a, b| a.path.cmp(&b.path));
        v
    }
}

fn to_browse(e: &Entry) -> BrowseEntry {
    BrowseEntry {
        source: e.source,
        name: e.path.file_name().unwrap_or("").to_owned(),
        path: e.path.to_string(),
        kind: e.kind,
        size: e.size,
        modified_ms: e.modified_ms,
        hash: e.hash.map(|h| h.to_hex()),
    }
}

/// Distinct versions of one file across all valid snapshots, newest first.
/// `source_path` is the absolute source folder as recorded in the snapshots.
pub fn file_versions(
    repo: &Repository,
    source_path: &str,
    path: &RelPath,
) -> Result<Vec<FileVersion>> {
    let key = source_key(source_path);
    let (snaps, _) = repo.load_all()?;
    let mut versions: Vec<FileVersion> = Vec::new();
    let mut last_hash: Option<ObjectHash> = None;
    for s in &snaps {
        let Some(src) = s.sources.iter().find(|r| source_key(&r.path) == key) else {
            last_hash = None;
            continue;
        };
        let found = s
            .entries
            .iter()
            .find(|e| e.source == src.id && &e.path == path && e.kind == EntryKind::File);
        match found {
            Some(e) if last_hash == e.hash => {
                if let Some(v) = versions.last_mut() {
                    v.snapshot_count += 1;
                }
            }
            Some(e) => {
                versions.push(FileVersion {
                    snapshot_id: s.id.to_string(),
                    snapshot_time: s.completed_at.clone(),
                    snapshot_count: 1,
                    size: e.size,
                    modified_ms: e.modified_ms,
                    hash: e.hash.map(|h| h.to_hex()).unwrap_or_default(),
                });
                last_hash = e.hash;
            }
            None => last_hash = None, // deleted in this snapshot
        }
    }
    versions.reverse();
    Ok(versions)
}

// ---------------------------------------------------------------------------------------------
// Restore

#[derive(Debug, Clone)]
pub enum RestoreTarget {
    /// Back to the folder recorded in the snapshot.
    Original,
    /// Into another folder. The selected item keeps its own name inside it.
    Folder(PathBuf),
}

#[derive(Debug, Clone)]
pub struct RestoreRequest {
    /// `(source id, path)` pairs; a path may be a file or a folder (restored recursively).
    pub items: Vec<(u32, RelPath)>,
    pub target: RestoreTarget,
    pub policy: ConflictPolicy,
}

struct PlannedItem<'a> {
    entry: &'a Entry,
    target: PathBuf,
    conflict: Option<ConflictKind>,
}

struct Plan<'a> {
    items: Vec<PlannedItem<'a>>,
    target_root: PathBuf,
}

fn build_plan<'a>(view: &SnapshotView<'a>, req: &RestoreRequest) -> Result<Plan<'a>> {
    if req.items.is_empty() {
        return Err(Error::InvalidRequest("nothing selected to restore".into()));
    }
    let snap = view.snapshot();
    let mut items: BTreeMap<PathBuf, &'a Entry> = BTreeMap::new();
    let mut target_root = PathBuf::new();
    for (source, sel) in &req.items {
        let src = snap
            .source(*source)
            .ok_or_else(|| Error::NotFound(format!("source {source} in snapshot")))?;
        if view.entry(*source, sel).is_none() {
            return Err(Error::NotFound(format!("{sel} in snapshot {}", snap.id)));
        }

        // (base folder on disk, prefix of manifest paths that maps onto it)
        let (base, strip) = match &req.target {
            RestoreTarget::Original => (original_root(&src.path)?, RelPath::root()),
            RestoreTarget::Folder(dest) => {
                let dest = std::path::absolute(dest).ctx("resolve", dest)?;
                match sel.parent() {
                    Some(parent) => (dest, parent),
                    // Whole source: put it in a folder named after the source label.
                    None => (
                        dest.join(safe_component(&src.label, *source)),
                        RelPath::root(),
                    ),
                }
            }
        };
        if target_root.as_os_str().is_empty() {
            target_root = base.clone();
        }
        for e in view.subtree(*source, sel) {
            let rel = e
                .path
                .strip_prefix(&strip)
                .expect("subtree entries share the prefix");
            if cfg!(windows) && rel.components().any(k92_core::is_windows_reserved_name) {
                return Err(Error::InvalidPath(format!("{rel}: reserved Windows name")));
            }
            let target = rel.under(&base);
            debug_assert!(target.starts_with(&base));
            items.entry(target).or_insert(e);
        }
    }

    let mut planned = Vec::with_capacity(items.len());
    for (target, entry) in items {
        let conflict = detect_conflict(entry, &target)?;
        planned.push(PlannedItem {
            entry,
            target,
            conflict,
        });
    }
    Ok(Plan {
        items: planned,
        target_root,
    })
}

/// The original source folder from an (untrusted) manifest: must be absolute and normalized.
fn original_root(recorded: &str) -> Result<PathBuf> {
    let p = PathBuf::from(recorded);
    let bad = !p.is_absolute()
        || p.components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir));
    if bad {
        return Err(Error::InvalidPath(format!(
            "original location {recorded:?} is not usable here; restore to another folder"
        )));
    }
    Ok(p)
}

fn safe_component(label: &str, source: u32) -> String {
    let cleaned: String = label
        .chars()
        .map(|c| {
            if RelPath::is_valid_component(&c.to_string()) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_end_matches(['.', ' ']).to_owned();
    if RelPath::is_valid_component(&cleaned) && !k92_core::is_windows_reserved_name(&cleaned) {
        cleaned
    } else {
        format!("Source {source}")
    }
}

fn detect_conflict(entry: &Entry, target: &Path) -> Result<Option<ConflictKind>> {
    let meta = match fs::symlink_metadata(target) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::io_path("inspect", target, e)),
    };
    let ft = meta.file_type();
    Ok(match entry.kind {
        EntryKind::Dir if ft.is_dir() => None, // merge into existing folder
        EntryKind::Dir => Some(ConflictKind::TypeMismatch),
        EntryKind::File if !ft.is_file() => Some(ConflictKind::TypeMismatch),
        EntryKind::File => {
            let same = meta.len() == entry.size
                && entry
                    .hash
                    .is_some_and(|h| hash_file(target).ok() == Some(h));
            Some(if same {
                ConflictKind::Identical
            } else {
                ConflictKind::Exists
            })
        }
    })
}

fn hash_file(p: &Path) -> io::Result<ObjectHash> {
    let mut f = File::open(p)?;
    let mut h = blake3::Hasher::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(ObjectHash(*h.finalize().as_bytes()))
}

/// Dry run: what would be restored and which conflicts exist. Writes nothing.
pub fn plan_restore(snap: &Snapshot, req: &RestoreRequest) -> Result<RestorePlan> {
    let view = SnapshotView::new(snap);
    let plan = build_plan(&view, req)?;
    let mut out = RestorePlan {
        files: 0,
        dirs: 0,
        bytes: 0,
        target_root: plan.target_root.display().to_string(),
        conflicts: Vec::new(),
    };
    for it in &plan.items {
        match it.entry.kind {
            EntryKind::File => {
                out.files += 1;
                out.bytes += it.entry.size;
            }
            EntryKind::Dir => out.dirs += 1,
        }
        if let Some(kind) = it.conflict {
            out.conflicts.push(RestoreConflict {
                target_path: it.target.display().to_string(),
                kind,
            });
        }
    }
    Ok(out)
}

/// Restores the selection. With `ConflictPolicy::Fail`, any real conflict aborts before
/// anything is written and the conflicts are returned in the report.
pub fn restore(
    repo: &Repository,
    snap: &Snapshot,
    req: &RestoreRequest,
    cancel: &CancelToken,
    progress: ProgressFn<'_>,
) -> Result<RestoreReport> {
    let view = SnapshotView::new(snap);
    let plan = build_plan(&view, req)?;
    let mut report = RestoreReport::default();

    let real_conflicts: Vec<RestoreConflict> = plan
        .items
        .iter()
        .filter_map(|it| match it.conflict {
            Some(ConflictKind::Identical) | None => None,
            Some(kind) => Some(RestoreConflict {
                target_path: it.target.display().to_string(),
                kind,
            }),
        })
        .collect();
    if req.policy == ConflictPolicy::Fail && !real_conflicts.is_empty() {
        report.conflicts = real_conflicts;
        return Ok(report);
    }
    report.conflicts = real_conflicts;

    let stamp = snapshot_stamp(snap);
    let files_total = plan
        .items
        .iter()
        .filter(|i| i.entry.kind == EntryKind::File)
        .count() as u64;
    let mut files_done = 0u64;

    for it in &plan.items {
        cancel.check()?;
        let shown = it.target.display().to_string();
        let fail = |report: &mut RestoreReport, msg: String| {
            report.failed.push(RestoreFailure {
                path: shown.clone(),
                message: msg,
            })
        };
        match (it.entry.kind, it.conflict) {
            (EntryKind::Dir, None) => match fs::create_dir_all(&it.target) {
                Ok(()) => report.dirs_created += 1,
                Err(e) => fail(&mut report, e.to_string()),
            },
            (EntryKind::Dir, Some(_)) => fail(
                &mut report,
                "a file exists where this folder should be; not replaced".into(),
            ),
            (EntryKind::File, conflict) => {
                files_done += 1;
                progress(ProgressEvent {
                    phase: JobPhase::Restoring,
                    files_done,
                    files_total: Some(files_total),
                    bytes_total: None,
                    bytes_done: report.bytes,
                    current_path: Some(shown.clone()),
                });
                let outcome = match (conflict, req.policy) {
                    (None, _) => write_file(repo, it.entry, &it.target, Mode::NoClobber, cancel)
                        .map(|_| Done::Restored),
                    (Some(ConflictKind::Identical), _) => Ok(Done::AlreadyPresent),
                    (Some(_), ConflictPolicy::Skip | ConflictPolicy::Fail) => Ok(Done::Skipped),
                    (Some(_), ConflictPolicy::Rename) => {
                        let alt = renamed_target(&it.target, &stamp);
                        write_file(repo, it.entry, &alt, Mode::NoClobber, cancel)
                            .map(|_| Done::Renamed)
                    }
                    (Some(ConflictKind::TypeMismatch), ConflictPolicy::Overwrite) => {
                        Err(Error::InvalidRequest(
                            "a folder exists here; folders are never replaced".into(),
                        ))
                    }
                    (Some(_), ConflictPolicy::Overwrite) => {
                        write_file(repo, it.entry, &it.target, Mode::Replace, cancel)
                            .map(|_| Done::Overwritten)
                    }
                };
                match outcome {
                    Ok(done) => {
                        match done {
                            Done::Restored => report.restored += 1,
                            Done::Renamed => {
                                report.restored += 1;
                                report.renamed += 1
                            }
                            Done::Overwritten => {
                                report.restored += 1;
                                report.overwritten += 1
                            }
                            Done::AlreadyPresent => report.already_present += 1,
                            Done::Skipped => report.skipped_conflicts += 1,
                        }
                        if matches!(done, Done::Restored | Done::Renamed | Done::Overwritten) {
                            report.bytes += it.entry.size;
                        }
                    }
                    Err(Error::Cancelled) => return Err(Error::Cancelled),
                    Err(e) => fail(&mut report, e.to_string()),
                }
            }
        }
    }
    Ok(report)
}

enum Done {
    Restored,
    Renamed,
    Overwritten,
    AlreadyPresent,
    Skipped,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Fail if anything appears at the target (atomic via hard link where supported).
    NoClobber,
    Replace,
}

/// Writes one file: object → temp file beside the target → verify from disk → move into place.
fn write_file(
    repo: &Repository,
    e: &Entry,
    target: &Path,
    mode: Mode,
    cancel: &CancelToken,
) -> Result<()> {
    let hash = e
        .hash
        .ok_or_else(|| Error::Corrupt(format!("{} has no content hash", e.path)))?;
    let parent = target
        .parent()
        .ok_or_else(|| Error::InvalidPath(target.display().to_string()))?;
    fs::create_dir_all(parent).ctx("create folder", parent)?;
    let tmp = parent.join(format!(
        ".k92-restore-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    let res = (|| {
        let f = File::create_new(&tmp).ctx("create", &tmp)?;
        let mut w = BufWriter::new(f);
        repo.read_object_into(&hash, &mut w, cancel)?;
        let f = w
            .into_inner()
            .map_err(|e| Error::io_path("write", &tmp, e.into_error()))?;
        if let Some(ms) = e.modified_ms {
            let t = if ms >= 0 {
                UNIX_EPOCH + Duration::from_millis(ms as u64)
            } else {
                UNIX_EPOCH - Duration::from_millis(ms.unsigned_abs())
            };
            let _ = f.set_modified(t);
        }
        f.sync_all().ctx("flush", &tmp)?;
        drop(f);
        // Verify what actually landed on disk.
        if hash_file(&tmp).ctx("verify", &tmp)? != hash {
            return Err(Error::Corrupt(format!(
                "restored data for {} failed verification",
                e.path
            )));
        }
        match mode {
            Mode::NoClobber => place_no_clobber(&tmp, target)?,
            Mode::Replace => {
                if target.exists() {
                    let _ = k92_windows::set_readonly(target, false);
                }
                fs::rename(&tmp, target).ctx("replace", target)?;
            }
        }
        if e.readonly {
            let _ = k92_windows::set_readonly(target, true);
        }
        Ok(())
    })();
    let _ = fs::remove_file(&tmp);
    res
}

fn place_no_clobber(tmp: &Path, target: &Path) -> Result<()> {
    match fs::hard_link(tmp, target) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(Error::InvalidRequest(format!(
            "{} appeared during restore; left untouched",
            target.display()
        ))),
        // Filesystems without hard links (FAT32/exFAT): check then rename.
        Err(_) => {
            if fs::symlink_metadata(target).is_ok() {
                return Err(Error::InvalidRequest(format!(
                    "{} appeared during restore; left untouched",
                    target.display()
                )));
            }
            fs::rename(tmp, target).ctx("move into place", target)
        }
    }
}

fn snapshot_stamp(snap: &Snapshot) -> String {
    snap.id
        .timestamp()
        .map(|t| {
            Local
                .from_utc_datetime(&t.naive_utc())
                .format("%Y-%m-%d %H%M")
                .to_string()
        })
        .unwrap_or_else(|| "backup".into())
}

/// `report.docx` → `report (restored 2026-11-09 1030).docx`, then ` 2`, ` 3`, … if taken.
fn renamed_target(target: &Path, stamp: &str) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name.as_str(), ""),
    };
    let parent = target.parent().unwrap_or(Path::new(""));
    for n in 1.. {
        let suffix = if n == 1 {
            String::new()
        } else {
            format!(" {n}")
        };
        let cand = parent.join(format!("{stem} (restored {stamp}{suffix}){ext}"));
        if fs::symlink_metadata(&cand).is_err() {
            return cand;
        }
    }
    unreachable!()
}

/// Convenience used by the CLI and desktop app: write `out` as a single file (preview/export).
pub fn export_file(repo: &Repository, entry: &Entry, out: &mut dyn Write) -> Result<u64> {
    let hash = entry
        .hash
        .ok_or_else(|| Error::InvalidRequest("not a file".into()))?;
    repo.read_object_into(&hash, out, &CancelToken::new())
}
