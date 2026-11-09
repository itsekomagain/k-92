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
        false,
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
        stored_size: 3,
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
    let res = w.store_object(&mut Cursor::new(vec![1u8; 10]), false, &c, &mut |_| {});
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

fn store_z(w: &RepoWriter, data: &[u8]) -> StoredObject {
    w.store_object(
        &mut Cursor::new(data.to_vec()),
        true,
        &CancelToken::new(),
        &mut |_| {},
    )
    .unwrap_or_else(|e| panic!("{e:?}"))
}

fn read_back(r: &Repository, h: &ObjectHash) -> Vec<u8> {
    let mut out = Vec::new();
    r.read_object_into(h, &mut out, &CancelToken::new())
        .unwrap();
    out
}

#[test]
fn compressible_data_is_stored_compressed_and_reads_back() {
    let d = tmp();
    let r = Repository::init(d.path()).unwrap();
    assert!(r.supports_compression());
    assert_eq!(r.descriptor().format_version, 2);
    let w = r.writer().unwrap();
    let text: Vec<u8> = b"The quick brown fox jumps over the lazy dog. "
        .iter()
        .cycle()
        .take(3 * 1024 * 1024 + 17)
        .copied()
        .collect();
    let o = store_z(&w, &text);
    assert!(o.is_new);
    assert!(o.stored_size < text.len() as u64 / 10, "{}", o.stored_size);
    assert!(r.compressed_object_path(&o.hash).is_file());
    assert!(!r.object_path(&o.hash).exists());
    assert!(r.has_object(&o.hash, text.len() as u64));
    assert_eq!(read_back(&r, &o.hash), text);
    assert!(r.check_object(&o.hash).unwrap());

    // Same content again deduplicates.
    assert!(!store_z(&w, &text).is_new);
}

#[test]
fn incompressible_or_tiny_data_is_stored_raw() {
    let d = tmp();
    let r = Repository::init(d.path()).unwrap();
    let w = r.writer().unwrap();
    // Pseudo-random bytes (like a JPEG or ZIP) do not compress.
    let mut x = 0x1234_5678_u64;
    let noise: Vec<u8> = (0..200_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    let o = store_z(&w, &noise);
    assert!(r.object_path(&o.hash).is_file());
    assert_eq!(o.stored_size, noise.len() as u64);
    let tiny = store_z(&w, &[b'a'; 100]);
    assert!(r.object_path(&tiny.hash).is_file());
    assert_eq!(read_back(&r, &o.hash), noise);
    assert_eq!(read_back(&r, &tiny.hash), vec![b'a'; 100]);
}

#[test]
fn damaged_compressed_object_is_detected() {
    let d = tmp();
    let r = Repository::init(d.path()).unwrap();
    let w = r.writer().unwrap();
    let text = vec![b'z'; 100_000];
    let o = store_z(&w, &text);
    let p = r.compressed_object_path(&o.hash);
    let mut bytes = fs::read(&p).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xff;
    fs::write(&p, &bytes).unwrap();
    assert!(!r.check_object(&o.hash).unwrap());
    let v = r.verify(true, &CancelToken::new()).unwrap();
    assert_eq!(
        v.corrupt_objects.len(),
        0,
        "unreferenced objects are not verified"
    );

    // GC understands .zst names: unreferenced compressed objects are removed.
    let g = w.gc(&CancelToken::new()).unwrap();
    assert_eq!(g.objects_deleted, 1);
    assert!(!p.exists());
}

#[test]
fn version_1_repositories_stay_raw_and_readable() {
    let d = tmp();
    let r = Repository::init(d.path()).unwrap();
    // Rewrite the descriptor as a v1 repository.
    let desc = r.root().join("k92-repository.json");
    let mut v: serde_json::Value = serde_json::from_slice(&fs::read(&desc).unwrap()).unwrap();
    v["formatVersion"] = 1.into();
    v["objectEncoding"] = "raw".into();
    fs::write(&desc, serde_json::to_vec(&v).unwrap()).unwrap();
    let r = Repository::open(d.path()).unwrap();
    assert!(!r.supports_compression());
    let w = r.writer().unwrap();
    let o = store_z(&w, &vec![b'q'; 50_000]);
    assert!(r.object_path(&o.hash).is_file(), "v1 never writes .zst");

    // A v1 descriptor claiming compression is rejected.
    v["objectEncoding"] = "raw+zstd".into();
    fs::write(&desc, serde_json::to_vec(&v).unwrap()).unwrap();
    assert!(matches!(
        Repository::open(d.path()),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn small_objects_go_into_packs_and_survive_reopen() {
    let d = tmp();
    let r = Repository::init(d.path()).unwrap();
    let w = r.writer().unwrap();
    let mut stored = Vec::new();
    for i in 0..300u32 {
        let data = format!("small file number {i} ").repeat(1 + (i as usize % 400));
        let o = w.store_small(data.as_bytes(), true).unwrap();
        assert!(o.is_new);
        stored.push((o, data));
    }
    // Duplicate content inside the open pack is not stored twice.
    assert!(!w.store_small(stored[5].1.as_bytes(), true).unwrap().is_new);
    w.flush_pack().unwrap();
    drop(w);

    let r2 = Repository::open(d.path()).unwrap();
    let packs: Vec<_> = fs::read_dir(r2.root().join("packs")).unwrap().collect();
    assert_eq!(packs.len(), 2, "one .pack and one .idx");
    for (o, data) in &stored {
        assert!(r2.has_object(&o.hash, o.size));
        assert!(!r2.object_path(&o.hash).exists());
        assert_eq!(read_back(&r2, &o.hash), data.as_bytes());
    }
    assert!(
        stored.iter().any(|(o, _)| o.stored_size < o.size),
        "some compressed"
    );
}

#[test]
fn unflushed_pack_is_invisible_after_a_crash() {
    let d = tmp();
    let r = Repository::init(d.path()).unwrap();
    let w = r.writer().unwrap();
    let o = w.store_small(b"not yet durable", false).unwrap();
    // Simulate a crash: the writer goes away without flushing.
    drop(w);
    let r2 = Repository::open(d.path()).unwrap();
    assert!(!r2.has_object(&o.hash, o.size));
    // The next writer clears the half-written pack from staging.
    let w2 = r2.writer().unwrap();
    assert_eq!(fs::read_dir(r2.root().join("staging")).unwrap().count(), 0);
    let o2 = w2.store_small(b"not yet durable", false).unwrap();
    assert!(o2.is_new);
}

#[test]
fn full_packs_commit_automatically() {
    let d = tmp();
    let r = Repository::init(d.path()).unwrap();
    let w = r.writer().unwrap();
    let mut x = 7u64;
    let mut first = None;
    // ~40 MB of incompressible 512 KB objects: crosses the 32 MB pack size.
    for _ in 0..80 {
        let data: Vec<u8> = (0..512 * 1024)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        let o = w.store_small(&data, true).unwrap();
        first.get_or_insert(o);
    }
    let fresh = Repository::open(d.path()).unwrap();
    let f = first.unwrap();
    assert!(
        fresh.has_object(&f.hash, f.size),
        "first pack committed without flush"
    );
}

#[test]
fn damaged_pack_is_reported_and_never_cleaned_up() {
    let d = tmp();
    let r = Repository::init(d.path()).unwrap();
    let w = r.writer().unwrap();
    let a = w.store_small(b"aaaa", false).unwrap();
    let s = snapshot_with(&w, &[("a.txt", &a)]);
    w.commit_snapshot(&s).unwrap();
    drop(w);
    let idx = fs::read_dir(r.root().join("packs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "idx"))
        .unwrap();
    fs::write(&idx, b"{\"checksum\":\"blake3:00\",\"body\":{}}").unwrap();

    let r2 = Repository::open(d.path()).unwrap();
    assert_eq!(r2.damaged_packs().len(), 1);
    let v = r2.verify(false, &CancelToken::new()).unwrap();
    assert_eq!(v.missing_objects, vec![a.hash.to_hex()]);
    assert!(!v.is_healthy());
    let w2 = r2.writer().unwrap();
    w2.delete_snapshot(&s.id).unwrap();
    w2.gc(&CancelToken::new()).unwrap();
    assert!(idx.exists(), "damaged pack left alone");
}

#[test]
fn gc_removes_unused_and_orphan_packs_and_keeps_used_ones() {
    let d = tmp();
    let r = Repository::init(d.path()).unwrap();
    let w = r.writer().unwrap();
    let keep = w.store_small(b"keep me", false).unwrap();
    let s1 = snapshot_with(&w, &[("keep.txt", &keep)]);
    w.commit_snapshot(&s1).unwrap();
    let gone = w.store_small(b"delete me", false).unwrap();
    let s2 = snapshot_with(&w, &[("gone.txt", &gone)]);
    w.commit_snapshot(&s2).unwrap();
    // An interrupted pack write: pack file without index.
    fs::write(
        r.root()
            .join("packs")
            .join("0123456789abcdef0123456789abcdef.pack"),
        b"junk",
    )
    .unwrap();
    w.delete_snapshot(&s2.id).unwrap();
    let g = w.gc(&CancelToken::new()).unwrap();
    assert_eq!(g.objects_deleted, 1);
    assert!(g.bytes_freed >= 4 + "delete me".len() as u64);
    assert!(r.has_object(&keep.hash, keep.size));
    assert!(!r.has_object(&gone.hash, gone.size));
    assert_eq!(
        fs::read_dir(r.root().join("packs")).unwrap().count(),
        2,
        "only the used pack and its index remain"
    );
    assert!(r.verify(true, &CancelToken::new()).unwrap().is_healthy());
}

#[test]
fn corrupt_bytes_inside_a_pack_are_detected() {
    let d = tmp();
    let r = Repository::init(d.path()).unwrap();
    let w = r.writer().unwrap();
    let a = w.store_small(b"will rot", false).unwrap();
    let s = snapshot_with(&w, &[("a.txt", &a)]);
    w.commit_snapshot(&s).unwrap();
    let Some(ObjectFile::Packed { path, loc }) = r.locate_object(&a.hash, a.size) else {
        panic!("packed")
    };
    let mut bytes = fs::read(&path).unwrap();
    bytes[loc.offset as usize + 6] = b'0';
    fs::write(&path, bytes).unwrap();
    assert!(!r.check_object(&a.hash).unwrap());
    let v = r.verify(true, &CancelToken::new()).unwrap();
    assert_eq!(v.corrupt_objects, vec![a.hash.to_hex()]);
}
