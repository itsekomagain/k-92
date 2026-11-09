# K-92 Repository Format, version 1

Status: implemented (`crates/k92-repository`). This document is the source of truth.
If code and document disagree, that is a bug.

The repository is **self-describing**: the files below are sufficient to list, verify
and restore every snapshot. No SQLite database, registry key or app config is
needed. A fresh K-92 install (or the portable `k92` CLI) can open any repository.

## 1. Layout

```
<destination>/K-92 Backup/
  k92-repository.json          repository descriptor (written once, never modified)
  README.txt                   plain-text explanation for humans
  objects/<aa>/<64 hex>        content blobs, named by BLAKE3 of their bytes
  snapshots/<snapshot-id>.json committed snapshot manifests
  staging/                     scratch area for in-progress writes (never referenced)
  checkpoint.jsonl             progress journal of an unfinished backup (optional, advisory)
  lock                         advisory lock file for the single writer
```

`<aa>` is the first two hex characters of the hash (256 fan-out directories).

## 2. Repository descriptor (`k92-repository.json`)

```json
{
  "format": "k92-repository",
  "formatVersion": 1,
  "repositoryId": "6f1c0e0e-…",
  "createdAt": "2026-11-09T10:30:00.000Z",
  "hashAlgorithm": "blake3",
  "objectEncoding": "raw",
  "encryption": null
}
```

* Readers **must refuse** a repository whose `formatVersion` is greater than they support
  or whose `hashAlgorithm` / `objectEncoding` / `encryption` they do not understand.
* `objectEncoding` and `encryption` exist so a future version can add compression and
  AES-256-GCM authenticated encryption without changing the layout. Version 1 stores raw bytes.

## 3. Objects

* An object is the complete content of one file version (file-level, no chunking in v1).
* Path: `objects/<first 2 hex>/<64 hex BLAKE3>`. Identical content is stored once.
* Written as: stream into `staging/<random>.tmp` while hashing → `fsync` → rename to
  final path (skipped if an identical object already exists) → `fsync` parent directory
  where the platform supports it.
* Readers verify the BLAKE3 hash while streaming. A mismatch is reported as corruption
  and the restored output is discarded.

## 4. Snapshot manifests

File: `snapshots/<snapshot-id>.json`.

Snapshot id: `YYYYMMDDTHHMMSSmmmZ-<8 hex>` (UTC time + random suffix). Sorting ids
lexically sorts snapshots chronologically. Ids are validated against this exact pattern
before being used in a path.

The file is a JSON envelope whose checksum covers the **exact bytes** of the body:

```json
{
  "checksum": "blake3:<hex of BLAKE3(body bytes)>",
  "body": {
    "format": "k92-snapshot",
    "formatVersion": 1,
    "id": "20261109T103000123Z-1a2b3c4d",
    "repositoryId": "6f1c0e0e-…",
    "parentId": "20261109T093000456Z-9f8e7d6c",
    "startedAt": "…", "completedAt": "…",
    "host": { "hostname": "DESKTOP-1", "os": "windows" },
    "sources": [ { "id": 0, "path": "C:\\Users\\Ada\\Documents", "label": "Documents" } ],
    "entries": [
      { "source": 0, "path": "", "kind": "dir", "modifiedMs": 1762680000000 },
      { "source": 0, "path": "Reports/q3.xlsx", "kind": "file", "size": 1048576,
        "modifiedMs": 1762680000000, "hash": "<64 hex>", "readonly": false }
    ],
    "skipped": [ { "source": 0, "path": "locked.pst", "reason": "locked" } ],
    "stats": { "files": 1, "dirs": 1, "bytes": 1048576, "newObjects": 1, "newBytes": 1048576, "skipped": 1 }
  }
}
```

* `path` is relative to the source root, `/`-separated, normalized, may be `""` for
  the root directory itself. Each component must be non-empty, not `.` or `..`, and contain
  no `/`, `\`, `:`, NUL, or other characters invalid in Windows file names.
* A file is *deleted* in snapshot N if it appears in N-1 but not N; older snapshots still
  contain it, so deleted files remain restorable until those snapshots are pruned.
* `skipped` records what could not be backed up and why (locked, access denied,
  reparse point, invalid name, changed during read). Skipped items are not errors that
  abort the snapshot; they are surfaced to the user.

## 5. Invariants

| # | Invariant |
|---|-----------|
| I1 | The bytes of `objects/xx/H` always hash to `H`. Objects are only created by atomic rename of a fully written, fsynced staging file. |
| I2 | A snapshot is committed **iff** `snapshots/<id>.json` exists and its checksum is valid. Partial or corrupt manifests are never shown as successful snapshots. |
| I3 | Every object referenced by a committed manifest was durably written before that manifest was renamed into place. |
| I4 | Committed manifests and objects are never modified in place. |
| I5 | Nothing references `staging/`. A writer holding the lock may empty it at any time. |
| I6 | At most one writer (backup, prune, GC) runs at a time, enforced by an OS file lock on `lock`. Readers need no lock. |
| I7 | All manifest contents are untrusted: ids, paths and hashes are validated before use, and restore never writes outside the chosen destination. |
| I8 | Garbage collection deletes an object only if no committed manifest references it. If **any** manifest cannot be read, GC deletes nothing. |
| I9 | Retention deletes snapshot manifests only; space is reclaimed by a later GC pass. A failed operation never deletes a valid snapshot. |
| I10 | `checkpoint.jsonl` is never part of a snapshot and nothing depends on it. A record is appended only after its object is durable, and is used only if that object still exists with the recorded size and the source file's size and modified time still match. Damaged or foreign lines are ignored; deleting the file only costs re-reading. |

### Checkpoint journal (`checkpoint.jsonl`)

One line per file stored by a backup that has not committed yet:
`<first 16 hex chars of BLAKE3(json)> <json>`, where json is
`{"s": source key, "p": relative path, "size", "mtime": ms, "hash", "new": bool}`.
The next backup run uses matching records like entries of the parent snapshot, so files that were
already stored are not read again. The file is synced every 256 MiB or 20 s and on exit, and is
deleted when a snapshot commits. GC ignores it, so objects listed only there may be collected;
the backup then simply reads those files again.

## 6. Crash and interruption behaviour

| Interrupted during | Result after restart |
|--------------------|----------------------|
| object write | stray file in `staging/` (cleaned on next write session); no visible change |
| after some objects, before manifest | unreferenced but valid objects plus `checkpoint.jsonl`; the next backup skips files already stored (I10) and GC can reclaim the objects otherwise |
| destination full | backup stops with "not enough space" before or during copying; nothing committed; progress kept in the checkpoint |
| manifest write | stray staging file; snapshot not visible (I2) |
| manifest rename | atomic on NTFS/ReFS/APFS/ext4: either visible and complete or absent |

## 7. Versioning policy

* Additive, ignorable fields may be added without bumping `formatVersion`.
* Any change an older reader could misinterpret bumps `formatVersion`.
* Future versions (planned): chunked objects for large files, split tree manifests for very
  large file counts, compressed and encrypted object encodings.

## 8. Known v1 limitations

* Single JSON manifest per snapshot: fine for ≈10⁵–10⁶ files; very large trees need split
  manifests (planned).
* Whole-file objects: a 1-byte change to a 20 GB file stores 20 GB again (chunking planned).
* Not preserved: NTFS ACLs, alternate data streams, EFS encryption, sparse/compressed
  attributes, hard-link identity, reparse points (junctions/symlinks are skipped and reported).
* Preserved: content, relative path, last-modified time, read-only attribute, empty directories.
