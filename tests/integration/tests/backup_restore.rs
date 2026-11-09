//! End-to-end backup and restore tests. Everything runs inside fresh temp directories.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use k92_backup::{BackupOptions, SourceSpec, run_backup};
use k92_core::dto::{BackupReport, ConflictKind, ConflictPolicy, SkipReason};
use k92_core::{CancelToken, Error, RelPath, no_progress};
use k92_repository::retention::{RetentionPolicy, plan};
use k92_repository::{Repository, Snapshot};
use k92_restore::{
    RestoreRequest, RestoreTarget, SnapshotView, file_versions, plan_restore, restore,
};

struct Env {
    _dir: tempfile::TempDir,
    src: PathBuf,
    dest: PathBuf,
    out: PathBuf,
    repo: Repository,
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("Documents");
    let dest = dir.path().join("USB");
    let out = dir.path().join("Restored");
    for d in [&src, &dest, &out] {
        fs::create_dir_all(d).unwrap();
    }
    let repo = Repository::init(&dest).unwrap();
    Env {
        _dir: dir,
        src,
        dest,
        out,
        repo,
    }
}

impl Env {
    fn write(&self, rel: &str, data: impl AsRef<[u8]>) {
        let p = self.src.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, data).unwrap();
    }

    fn opts(&self) -> BackupOptions {
        BackupOptions {
            sources: vec![SourceSpec::new(&self.src)],
            rehash_all: false,
        }
    }

    fn backup(&self) -> BackupReport {
        // Ensure mtimes differ between rapid successive edits on coarse-timestamp filesystems.
        run_backup(&self.repo, &self.opts(), &CancelToken::new(), &no_progress).unwrap()
    }

    fn snap(&self, id: &str) -> Snapshot {
        let id = self.repo.resolve_snapshot(id).unwrap();
        self.repo.load_snapshot(&id).unwrap()
    }

    fn restore_to_out(
        &self,
        snap: &Snapshot,
        path: &str,
        policy: ConflictPolicy,
    ) -> k92_core::dto::RestoreReport {
        let req = RestoreRequest {
            items: vec![(0, RelPath::parse(path).unwrap())],
            target: RestoreTarget::Folder(self.out.clone()),
            policy,
        };
        restore(&self.repo, snap, &req, &CancelToken::new(), &no_progress).unwrap()
    }
}

/// Bumps the mtime so size+mtime change detection sees the edit even within one clock tick.
fn touch_later(p: &Path) {
    let f = fs::File::options().write(true).open(p).unwrap();
    let t = fs::metadata(p).unwrap().modified().unwrap() + std::time::Duration::from_secs(2);
    f.set_modified(t).unwrap();
}

#[test]
fn milestone_backup_modify_backup_restore_earlier_version() {
    let e = env();
    e.write("report.txt", "version one");
    let first = e.backup();
    assert_eq!(first.files, 1);

    e.write("report.txt", "version two, longer");
    touch_later(&e.src.join("report.txt"));
    let second = e.backup();
    assert_eq!(second.files_copied, 1);
    assert_ne!(first.snapshot_id, second.snapshot_id);

    let old = e.snap(&first.snapshot_id);
    let rep = e.restore_to_out(&old, "report.txt", ConflictPolicy::Fail);
    assert_eq!(rep.restored, 1, "{rep:?}");
    assert_eq!(
        fs::read_to_string(e.out.join("report.txt")).unwrap(),
        "version one"
    );
    // source untouched
    assert_eq!(
        fs::read_to_string(e.src.join("report.txt")).unwrap(),
        "version two, longer"
    );
}

#[test]
fn full_backup_and_restore_whole_tree_matches_hashes() {
    let e = env();
    e.write("a.txt", "alpha");
    e.write("sub/b.bin", vec![7u8; 5000]);
    e.write("sub/deeper/c.md", "# c");
    fs::create_dir_all(e.src.join("empty/nested-empty")).unwrap();
    e.write("Résumé 履歴書 🎉.docx", "unicode");

    let r = e.backup();
    assert_eq!(r.files, 4);
    assert_eq!(r.dirs, 5); // root, sub, sub/deeper, empty, empty/nested-empty
    let snap = e.snap("latest");
    let rep = e.restore_to_out(&snap, "", ConflictPolicy::Fail);
    assert!(rep.failed.is_empty(), "{rep:?}");
    let restored_root = e.out.join("Documents");
    for f in [
        "a.txt",
        "sub/b.bin",
        "sub/deeper/c.md",
        "Résumé 履歴書 🎉.docx",
    ] {
        assert_eq!(
            blake3::hash(&fs::read(e.src.join(f)).unwrap()),
            blake3::hash(&fs::read(restored_root.join(f)).unwrap()),
            "{f}"
        );
    }
    assert!(
        restored_root.join("empty/nested-empty").is_dir(),
        "empty dirs restored"
    );
    // mtime preserved
    let a = fs::metadata(e.src.join("a.txt"))
        .unwrap()
        .modified()
        .unwrap();
    let b = fs::metadata(restored_root.join("a.txt"))
        .unwrap()
        .modified()
        .unwrap();
    let diff = a.duration_since(b).unwrap_or_else(|e| e.duration());
    assert!(diff.as_millis() < 2, "mtime differs by {diff:?}");
}

#[test]
fn incremental_backup_reuses_unchanged_files() {
    let e = env();
    for i in 0..20 {
        e.write(&format!("f{i}.txt"), format!("content {i}"));
    }
    let first = e.backup();
    assert_eq!(first.files_copied, 20);
    assert_eq!(first.new_objects, 20);

    e.write("f3.txt", "changed!!");
    touch_later(&e.src.join("f3.txt"));
    e.write("new.txt", "new");
    let second = e.backup();
    assert_eq!(second.files, 21);
    assert_eq!(second.files_unchanged, 19);
    assert_eq!(second.files_copied, 2);
    assert_eq!(second.new_objects, 2);

    // Deep mode re-reads everything but stores nothing new.
    let deep = run_backup(
        &e.repo,
        &BackupOptions {
            rehash_all: true,
            ..e.opts()
        },
        &CancelToken::new(),
        &no_progress,
    )
    .unwrap();
    assert_eq!(deep.files_copied, 21);
    assert_eq!(deep.new_objects, 0);
}

#[test]
fn deleted_files_remain_in_older_snapshots_and_versions_are_listed() {
    let e = env();
    e.write("doc.txt", "v1");
    let s1 = e.backup();
    e.write("doc.txt", "v2!");
    touch_later(&e.src.join("doc.txt"));
    let _s2 = e.backup();
    let _s3 = e.backup(); // unchanged
    fs::remove_file(e.src.join("doc.txt")).unwrap();
    let s4 = e.backup();
    assert_eq!(s4.files_removed, 1);

    let latest = e.snap("latest");
    assert!(
        SnapshotView::new(&latest)
            .entry(0, &RelPath::parse("doc.txt").unwrap())
            .is_none()
    );

    let versions = file_versions(
        &e.repo,
        &e.src.display().to_string(),
        &RelPath::parse("doc.txt").unwrap(),
    )
    .unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].snapshot_count, 2, "v2 lives in s2 and s3");
    assert_eq!(versions[1].snapshot_id, s1.snapshot_id);

    let rep = e.restore_to_out(&e.snap(&s1.snapshot_id), "doc.txt", ConflictPolicy::Fail);
    assert_eq!(rep.restored, 1);
    assert_eq!(fs::read_to_string(e.out.join("doc.txt")).unwrap(), "v1");
}

#[test]
fn renamed_files_are_deduplicated() {
    let e = env();
    e.write("old-name.txt", "same bytes");
    e.backup();
    fs::rename(e.src.join("old-name.txt"), e.src.join("new-name.txt")).unwrap();
    let r = e.backup();
    assert_eq!(r.new_objects, 0, "content already stored");
    assert_eq!(r.files_removed, 1);
    let snap = e.snap("latest");
    let names: Vec<_> = SnapshotView::new(&snap)
        .list_dir(0, &RelPath::root())
        .unwrap()
        .into_iter()
        .map(|b| b.name)
        .collect();
    assert_eq!(names, vec!["new-name.txt"]);
}

#[test]
fn large_file_spanning_many_buffers() {
    let e = env();
    let data: Vec<u8> = (0..25 * 1024 * 1024u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
        .collect();
    e.write("big.bin", &data);
    let r = e.backup();
    assert_eq!(r.bytes, data.len() as u64);
    let rep = e.restore_to_out(&e.snap("latest"), "big.bin", ConflictPolicy::Fail);
    assert_eq!(rep.restored, 1);
    assert!(fs::read(e.out.join("big.bin")).unwrap() == data);
}

#[test]
fn restore_conflict_policies_never_silently_overwrite() {
    let e = env();
    e.write("x.txt", "backed up");
    e.write("same.txt", "identical");
    e.backup();
    let snap = e.snap("latest");
    fs::write(e.out.join("x.txt"), "user's newer work").unwrap();
    fs::write(e.out.join("same.txt"), "identical").unwrap();

    let req = |policy| RestoreRequest {
        items: vec![
            (0, RelPath::parse("x.txt").unwrap()),
            (0, RelPath::parse("same.txt").unwrap()),
        ],
        target: RestoreTarget::Folder(e.out.clone()),
        policy,
    };
    let preview = plan_restore(&snap, &req(ConflictPolicy::Fail)).unwrap();
    let kinds: Vec<_> = preview.conflicts.iter().map(|c| c.kind).collect();
    assert!(kinds.contains(&ConflictKind::Exists) && kinds.contains(&ConflictKind::Identical));

    // Fail: nothing written at all.
    let r = restore(
        &e.repo,
        &snap,
        &req(ConflictPolicy::Fail),
        &CancelToken::new(),
        &no_progress,
    )
    .unwrap();
    assert_eq!((r.restored, r.conflicts.len()), (0, 1));
    assert_eq!(
        fs::read_to_string(e.out.join("x.txt")).unwrap(),
        "user's newer work"
    );

    // Skip: still untouched; identical counted as already present.
    let r = restore(
        &e.repo,
        &snap,
        &req(ConflictPolicy::Skip),
        &CancelToken::new(),
        &no_progress,
    )
    .unwrap();
    assert_eq!(
        (r.skipped_conflicts, r.already_present, r.restored),
        (1, 1, 0)
    );
    assert_eq!(
        fs::read_to_string(e.out.join("x.txt")).unwrap(),
        "user's newer work"
    );

    // Rename: both versions kept.
    let r = restore(
        &e.repo,
        &snap,
        &req(ConflictPolicy::Rename),
        &CancelToken::new(),
        &no_progress,
    )
    .unwrap();
    assert_eq!(r.renamed, 1);
    assert_eq!(
        fs::read_to_string(e.out.join("x.txt")).unwrap(),
        "user's newer work"
    );
    let renamed: Vec<_> = fs::read_dir(&e.out)
        .unwrap()
        .map(|d| d.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("x (restored "))
        .collect();
    assert_eq!(renamed.len(), 1, "{renamed:?}");
    assert_eq!(
        fs::read_to_string(e.out.join(&renamed[0])).unwrap(),
        "backed up"
    );

    // Overwrite: only when explicitly chosen.
    let r = restore(
        &e.repo,
        &snap,
        &req(ConflictPolicy::Overwrite),
        &CancelToken::new(),
        &no_progress,
    )
    .unwrap();
    assert_eq!(r.overwritten, 1);
    assert_eq!(
        fs::read_to_string(e.out.join("x.txt")).unwrap(),
        "backed up"
    );
    // No temp files left behind.
    assert!(
        fs::read_dir(&e.out).unwrap().all(|d| !d
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".k92"))
    );
}

#[test]
fn restore_to_original_location() {
    let e = env();
    e.write("keep/me.txt", "precious");
    e.backup();
    fs::remove_dir_all(e.src.join("keep")).unwrap();
    let snap = e.snap("latest");
    let req = RestoreRequest {
        items: vec![(0, RelPath::parse("keep").unwrap())],
        target: RestoreTarget::Original,
        policy: ConflictPolicy::Fail,
    };
    let r = restore(&e.repo, &snap, &req, &CancelToken::new(), &no_progress).unwrap();
    assert_eq!(r.restored, 1);
    assert_eq!(
        fs::read_to_string(e.src.join("keep/me.txt")).unwrap(),
        "precious"
    );
}

#[test]
fn corrupted_object_is_detected_on_restore_and_verify() {
    let e = env();
    e.write("good.txt", "good");
    e.write("bad.txt", "will rot");
    e.backup();
    let snap = e.snap("latest");
    let bad = SnapshotView::new(&snap)
        .entry(0, &RelPath::parse("bad.txt").unwrap())
        .unwrap()
        .hash
        .unwrap();
    fs::write(e.repo.object_path(&bad), "will r0t").unwrap(); // same size bit-rot

    let v = e.repo.verify(true, &CancelToken::new()).unwrap();
    assert_eq!(v.corrupt_objects, vec![bad.to_hex()]);

    let rep = e.restore_to_out(&snap, "", ConflictPolicy::Fail);
    assert_eq!(rep.restored, 1);
    assert_eq!(rep.failed.len(), 1);
    assert!(
        rep.failed[0].message.contains("does not match"),
        "{:?}",
        rep.failed
    );
    let root = e.out.join("Documents");
    assert!(
        !root.join("bad.txt").exists(),
        "no corrupt output left behind"
    );
    assert_eq!(fs::read_to_string(root.join("good.txt")).unwrap(), "good");
}

#[test]
fn fresh_install_imports_existing_repository() {
    let e = env();
    e.write("a.txt", "from the old PC");
    e.backup();
    let dest = e.dest.clone();
    // "Reinstall": no app state, only the drive.
    let imported = Repository::open(&dest).unwrap();
    let snap = imported.latest_valid_snapshot().unwrap().unwrap();
    let req = RestoreRequest {
        items: vec![(0, RelPath::parse("a.txt").unwrap())],
        target: RestoreTarget::Folder(e.out.clone()),
        policy: ConflictPolicy::Fail,
    };
    let r = restore(&imported, &snap, &req, &CancelToken::new(), &no_progress).unwrap();
    assert_eq!(r.restored, 1);
    assert!(
        imported
            .verify(true, &CancelToken::new())
            .unwrap()
            .is_healthy()
    );
}

#[test]
fn cancellation_commits_nothing_and_next_run_resumes() {
    let e = env();
    for i in 0..10 {
        e.write(&format!("f{i}.bin"), vec![i as u8; 1000]);
    }
    let cancel = CancelToken::new();
    let seen = AtomicU64::new(0);
    let progress = |_ev: k92_core::dto::ProgressEvent| {
        if seen.fetch_add(1, Ordering::SeqCst) == 5 {
            cancel.cancel();
        }
    };
    let res = run_backup(&e.repo, &e.opts(), &cancel, &progress);
    assert!(matches!(res, Err(Error::Cancelled)), "{res:?}");
    assert!(
        e.repo.snapshot_ids().unwrap().is_empty(),
        "nothing committed"
    );

    let r = e.backup();
    assert_eq!(r.files, 10);
    assert!(
        r.new_objects < 10,
        "objects stored before cancel were reused: {}",
        r.new_objects
    );
    assert!(
        e.repo
            .verify(true, &CancelToken::new())
            .unwrap()
            .is_healthy()
    );
}

#[test]
fn disconnected_destination_aborts_without_snapshot() {
    let e = env();
    for i in 0..5 {
        e.write(&format!("f{i}.txt"), format!("{i}"));
    }
    e.backup();
    for i in 0..5 {
        e.write(&format!("f{i}.txt"), format!("changed {i}"));
        touch_later(&e.src.join(format!("f{i}.txt")));
    }
    let root = e.repo.root().to_path_buf();
    let pulled = std::sync::atomic::AtomicBool::new(false);
    let progress = |_ev: k92_core::dto::ProgressEvent| {
        if !pulled.swap(true, Ordering::SeqCst) {
            // Simulate the USB drive disappearing mid-backup.
            fs::rename(&root, root.with_file_name("gone")).unwrap();
        }
    };
    let res = run_backup(&e.repo, &e.opts(), &CancelToken::new(), &progress);
    assert!(
        res.is_err(),
        "backup must fail when the destination vanishes"
    );
    fs::rename(root.with_file_name("gone"), &root).unwrap();
    assert_eq!(
        e.repo.snapshot_ids().unwrap().len(),
        1,
        "only the earlier snapshot exists"
    );
    assert!(
        e.repo
            .verify(true, &CancelToken::new())
            .unwrap()
            .is_healthy()
    );
}

#[test]
fn concurrent_backup_is_rejected_as_busy() {
    let e = env();
    e.write("a", "a");
    let _held = e.repo.writer().unwrap();
    let res = run_backup(&e.repo, &e.opts(), &CancelToken::new(), &no_progress);
    assert!(matches!(res, Err(Error::Busy)));
}

#[test]
fn destination_inside_source_is_rejected() {
    let e = env();
    let inner = e.src.join("backups");
    fs::create_dir_all(&inner).unwrap();
    let repo = Repository::init(&inner).unwrap();
    let res = run_backup(&repo, &e.opts(), &CancelToken::new(), &no_progress);
    assert!(matches!(res, Err(Error::InvalidRequest(_))), "{res:?}");
}

#[test]
fn retention_then_gc_keeps_remaining_snapshots_restorable() {
    let e = env();
    let mut ids = Vec::new();
    for i in 0..4 {
        e.write("f.txt", format!("version {i}"));
        touch_later(&e.src.join("f.txt"));
        ids.push(e.backup().snapshot_id);
    }
    // Pretend we are far in the future with an aggressive policy: only the newest survives.
    let all = e.repo.snapshot_ids().unwrap();
    let policy = RetentionPolicy {
        keep_all_hours: 0,
        daily_days: 0,
        weekly_weeks: Some(0),
    };
    let doomed = plan(
        &all,
        chrono::Utc::now() + chrono::Duration::days(365),
        &policy,
    );
    assert_eq!(doomed.len(), 3);
    let w = e.repo.writer().unwrap();
    for id in &doomed {
        w.delete_snapshot(id).unwrap();
    }
    let gc = w.gc(&CancelToken::new()).unwrap();
    assert_eq!(gc.objects_deleted, 3);
    drop(w);
    assert!(
        e.repo
            .verify(true, &CancelToken::new())
            .unwrap()
            .is_healthy()
    );
    let rep = e.restore_to_out(&e.snap("latest"), "f.txt", ConflictPolicy::Fail);
    assert_eq!(rep.restored, 1);
    assert_eq!(
        fs::read_to_string(e.out.join("f.txt")).unwrap(),
        "version 3"
    );
}

#[test]
fn search_finds_files_by_name() {
    let e = env();
    e.write("Projects/Budget 2026.xlsx", "x");
    e.write("Projects/notes.txt", "y");
    e.backup();
    let snap = e.snap("latest");
    let hits = SnapshotView::new(&snap).search("budget", 10);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, "Projects/Budget 2026.xlsx");
}

#[cfg(unix)]
#[test]
fn symlinks_are_not_followed_and_are_reported() {
    let e = env();
    let outside = e.dest.parent().unwrap().join("outside-secret");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.txt"), "do not back up").unwrap();
    std::os::unix::fs::symlink(&outside, e.src.join("link-out")).unwrap();
    e.write("normal.txt", "ok");
    let r = e.backup();
    assert_eq!(r.files, 1);
    assert_eq!(r.skipped.len(), 1);
    assert_eq!(r.skipped[0].reason, SkipReason::ReparsePoint);
}

#[cfg(unix)]
#[test]
fn unreadable_file_is_skipped_not_fatal() {
    use std::os::unix::fs::PermissionsExt;
    let e = env();
    e.write("ok.txt", "ok");
    e.write("private.txt", "secret");
    let p = e.src.join("private.txt");
    fs::set_permissions(&p, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::File::open(&p).is_ok() {
        eprintln!("running as root; permission test not meaningful");
        return;
    }
    let r = e.backup();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(r.files, 1);
    assert_eq!(r.skipped.len(), 1);
    assert_eq!(r.skipped[0].reason, SkipReason::AccessDenied);
}

#[cfg(windows)]
#[test]
fn locked_file_is_skipped_and_reported() {
    use std::os::windows::fs::OpenOptionsExt;
    let e = env();
    e.write("ok.txt", "ok");
    e.write("locked.pst", "mail");
    let _hold = fs::File::options()
        .read(true)
        .share_mode(0)
        .open(e.src.join("locked.pst"))
        .unwrap();
    let r = e.backup();
    assert_eq!(r.files, 1);
    assert_eq!(r.skipped[0].reason, SkipReason::Locked);
}
