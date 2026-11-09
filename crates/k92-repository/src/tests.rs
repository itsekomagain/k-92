//! Tests for the repository invariants (I1–I9 in repository-format.md).

use std::fs;
use std::io::Cursor;

use k92_core::dto::EntryKind;
use k92_core::{CancelToken, Error, ObjectHash, RelPath};

use crate::manifest::{SNAPSHOT_FORMAT, SnapshotStats};
use crate::*;

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn store(w: &RepoWriter, data: &[u8]) -> StoredObject {
    w.store_object(
        &mut Cursor::new(data.to_vec()),
        &CancelToken::new(),
        &mut |_| {},
    )
    .unwrap_or_else(|e| panic!("{e:?}"))
}

fn snapshot_with(w: &RepoWriter, files: &[(&str, &StoredObject)]) -> Snapshot {
    Snapshot {
        format: SNAPSHOT_FORMAT.into(),
        format_version: 1,
        id: w.next_snapshot_id().unwrap(),
        repository_id: w.repo().id().into(),
        parent_id: None,
        started_at: now_rfc3339(),
        completed_at: now_rfc3339(),
        host: HostInfo::current(),
        sources: vec![SourceRoot {
            id: 0,
            path: "/src".into(),
            label: "src".into(),
        }],
        entries: files
            .iter()
            .map(|(p, o)| Entry {
                source: 0,
                path: RelPath::parse(p).unwrap(),
                kind: EntryKind::File,
                size: o.size,
                modified_ms: Some(0),
                hash: Some(o.hash),
                readonly: false,
            })
            .collect(),
        skipped: vec![],
        stats: SnapshotStats {
            files: files.len() as u64,
            ..Default::default()
        },
    }
}

#[test]
fn init_then_open_from_root_or_parent() {
    let d = tmp();
    let r = Repository::init(d.path()).unwrap();
    assert_eq!(Repository::open(d.path()).unwrap().id(), r.id());
    assert_eq!(Repository::open(r.root()).unwrap().id(), r.id());
    assert!(r.root().join("README.txt").is_file());
    // Second init refuses instead of clobbering.
    assert!(Repository::init(d.path()).is_err());
}

#[test]
fn open_rejects_non_repositories_and_future_versions() {
    let d = tmp();
    assert!(matches!(
        Repository::open(d.path()),
        Err(Error::NotARepository(_))
    ));
    assert!(matches!(
        Repository::open(&d.path().join("nope")),
        Err(Error::NotFound(_))
    ));

    let r = Repository::init(d.path()).unwrap();
    let p = r.root().join("k92-repository.json");
    let mut desc: serde_json::Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
    desc["formatVersion"] = 99.into();
    fs::write(&p, serde_json::to_vec(&desc).unwrap()).unwrap();
    assert!(matches!(
        Repository::open(d.path()),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn objects_are_content_addressed_and_deduplicated() {
    let d = tmp();
    let r = Repository::init(d.path()).unwrap();
    let w = r.writer().unwrap();
    let a = store(&w, b"hello");
    let b = store(&w, b"hello");
    assert!(a.is_new);
    assert!(!b.is_new);
    assert_eq!(a.hash, b.hash);
    assert_eq!(a.hash.0, *blake3::hash(b"hello").as_bytes());
    assert_eq!(fs::read(r.object_path(&a.hash)).unwrap(), b"hello");
    // staging is empty after successful stores
    assert_eq!(fs::read_dir(r.root().join("staging")).unwrap().count(), 0);
}

#[test]
fn single_writer_lock() {
    let d = tmp();
    let r = Repository::init(d.path()).unwrap();
    let w = r.writer().unwrap();
    assert!(matches!(r.writer(), Err(Error::Busy)));
    drop(w);
    assert!(r.writer().is_ok());
}

#[test]
fn commit_and_reload_snapshot() {
    let d = tmp();
    let r = Repository::init(d.path()).unwrap();
    let w = r.writer().unwrap();
    let o = store(&w, b"data");
    let s = snapshot_with(&w, &[("a.txt", &o)]);
    w.commit_snapshot(&s).unwrap();
    drop(w);
    let reopened = Repository::open(d.path()).unwrap();
    assert_eq!(reopened.snapshot_ids().unwrap(), vec![s.id.clone()]);
    assert_eq!(reopened.load_snapshot(&s.id).unwrap(), s);
}

#[test]
fn commit_refuses_snapshot_with_missing_objects() {
    let d = tmp();
    let r = Repository::init(d.path()).unwrap();
    let w = r.writer().unwrap();
    let fake = StoredObject {
        hash: ObjectHash([7; 32]),
        size: 3,
        is_new: true,
    };
    let s = snapshot_with(&w, &[("a.txt", &fake)]);
    assert!(w.commit_snapshot(&s).is_err());
    assert!(r.snapshot_ids().unwrap().is_empty());
}

#[test]
fn interrupted_write_leaves_no_visible_snapshot_and_is_cleaned() {
    let d = tmp();
    let r = Repository::init(d.path()).unwrap();
    // Simulate a crash mid-manifest-write: a partial file in staging.
    fs::write(r.root().join("staging/abc.tmp"), b"{\"checksum\":\"blake3:").unwrap();
    assert!(r.snapshot_ids().unwrap().is_empty());
    let _w = r.writer().unwrap();
    assert_eq!(fs::read_dir(r.root().join("staging")).unwrap().count(), 0);
}

#[test]
fn cancelled_store_leaves_nothing_behind() {
    let d = tmp();
    let r = Repository::init(d.path()).unwrap();
    let w = r.writer().unwrap();
    let c = CancelToken::new();
    c.cancel();
    let res = w.store_object(&mut Cursor::new(vec![1u8; 10]), &c, &mut |_| {});
    assert!(matches!(res, Err(StoreError::Repo(Error::Cancelled))));
    assert_eq!(fs::read_dir(r.root().join("staging")).unwrap().count(), 0);
}

#[test]
fn damaged_manifest_is_reported_skipped_and_blocks_gc() {
    let d = tmp();
    let r = Repository::init(d.path()).unwrap();
    let w = r.writer().unwrap();
    let o1 = store(&w, b"one");
    let s1 = snapshot_with(&w, &[("a", &o1)]);
    w.commit_snapshot(&s1).unwrap();
    let o2 = store(&w, b"two");
    let s2 = snapshot_with(&w, &[("a", &o2)]);
    w.commit_snapshot(&s2).unwrap();
    let orphan = store(&w, b"orphan");

    // Corrupt the newest manifest (bit rot).
    let p = r.root().join("snapshots").join(s2.id.file_name());
    let mut bytes = fs::read(&p).unwrap();
    let n = bytes.len() - 10;
    bytes[n] ^= 0x20;
    fs::write(&p, bytes).unwrap();

    let (ok, bad) = r.load_all().unwrap();
    assert_eq!(ok.len(), 1);
    assert_eq!(bad.len(), 1);
    assert_eq!(r.latest_valid_snapshot().unwrap().unwrap().id, s1.id);
    // I8: GC must not delete anything while a manifest is unreadable.
    assert!(w.gc(&CancelToken::new()).is_err());
    assert!(r.object_path(&o2.hash).exists());
    assert!(r.object_path(&orphan.hash).exists());
}

#[test]
fn gc_deletes_only_unreferenced_objects() {
    let d = tmp();
    let r = Repository::init(d.path()).unwrap();
    let w = r.writer().unwrap();
    let keep = store(&w, b"keep");
    let gone = store(&w, b"gone");
    let s1 = snapshot_with(&w, &[("k", &keep), ("g", &gone)]);
    w.commit_snapshot(&s1).unwrap();
    let s2 = snapshot_with(&w, &[("k", &keep)]);
    w.commit_snapshot(&s2).unwrap();

    // Still referenced by s1: nothing deleted.
    assert_eq!(w.gc(&CancelToken::new()).unwrap().objects_deleted, 0);
    w.delete_snapshot(&s1.id).unwrap();
    let rep = w.gc(&CancelToken::new()).unwrap();
    assert_eq!(rep.objects_deleted, 1);
    assert!(r.object_path(&keep.hash).exists());
    assert!(!r.object_path(&gone.hash).exists());
}

#[test]
fn read_and_verify_detect_object_corruption() {
    let d = tmp();
    let r = Repository::init(d.path()).unwrap();
    let w = r.writer().unwrap();
    let a = store(&w, b"aaaaaaaa");
    let b = store(&w, b"bbbbbbbb");
    let s = snapshot_with(&w, &[("a", &a), ("b", &b)]);
    w.commit_snapshot(&s).unwrap();
    drop(w);

    assert!(r.verify(true, &CancelToken::new()).unwrap().is_healthy());

    // Same-size bit flip: only deep verify and reads catch it.
    fs::write(r.object_path(&a.hash), b"aaaaaaaX").unwrap();
    assert!(r.verify(false, &CancelToken::new()).unwrap().is_healthy());
    let deep = r.verify(true, &CancelToken::new()).unwrap();
    assert_eq!(deep.corrupt_objects, vec![a.hash.to_hex()]);
    assert_eq!(deep.affected_snapshots, vec![s.id.to_string()]);
    let mut out = Vec::new();
    assert!(matches!(
        r.read_object_into(&a.hash, &mut out, &CancelToken::new()),
        Err(Error::Corrupt(_))
    ));

    fs::remove_file(r.object_path(&b.hash)).unwrap();
    let quick = r.verify(false, &CancelToken::new()).unwrap();
    assert_eq!(quick.missing_objects, vec![b.hash.to_hex()]);
}

#[test]
fn truncated_object_is_repaired_by_next_store() {
    let d = tmp();
    let r = Repository::init(d.path()).unwrap();
    let w = r.writer().unwrap();
    let a = store(&w, b"full content");
    fs::write(r.object_path(&a.hash), b"full").unwrap();
    let again = store(&w, b"full content");
    assert!(again.is_new);
    assert!(r.check_object(&a.hash).unwrap());
}

#[test]
fn foreign_files_in_snapshots_dir_are_ignored() {
    let d = tmp();
    let r = Repository::init(d.path()).unwrap();
    fs::write(r.root().join("snapshots/../../evil.json"), b"{}").ok();
    fs::write(r.root().join("snapshots/notes.txt"), b"hi").unwrap();
    fs::write(r.root().join("snapshots/garbage.json"), b"{}").unwrap();
    assert!(r.snapshot_ids().unwrap().is_empty());
}
