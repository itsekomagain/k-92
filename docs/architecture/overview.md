# K-92 Technical Architecture

## Goals, in priority order

1. **Never lose or corrupt backed-up data.** Committed snapshots are immutable and verified.
2. **Restore must work**, including when the original PC, its database and its K-92 install are gone.
3. Never modify source files. Never silently overwrite files during restore.
4. Simple for non-technical users; the timeline is the main way to use the app.

## Components

```
apps/desktop (Tauri 2 + React + TS)
   │  typed IPC (Tauri commands; Rust DTOs exported to TS via ts-rs)
   ▼
desktop backend (src-tauri)  ── today: calls engine crates in-process
   │                          ── Phase 4: authenticated named-pipe client to k92-service
   ▼
k92-backup ──┐        k92-restore ──┐
             ▼                      ▼
          k92-repository (format, objects, manifests, lock, verify, retention, GC)
             │
          k92-core (shared types, errors, path validation, cancellation, progress)
          k92-windows (Windows-only: reparse points, sharing violations, volume info; VSS later)
k92-cli  (`k92`) portable recovery and admin tool that uses the same crates
k92-service (Phase 4) Windows Service: scheduler, drive detection, job state
```

### Crate responsibilities

| Crate | Responsibility | Depends on |
|-------|----------------|------------|
| `k92-core` | IDs, `RelPath` (validated relative path), error types, `CancelToken`, progress events, IPC DTOs | none |
| `k92-repository` | On-disk format (see `docs/backup-format`), atomic writes, writer lock, object store, manifest load/validate, verify, retention, GC | core |
| `k92-backup` | Source scanning, change detection against the parent snapshot, streaming hash+store, snapshot commit, cancellation | core, repository, windows |
| `k92-restore` | Browse/search snapshots, file versions, restore with conflict policy, hash verification of output | core, repository |
| `k92-windows` | Thin platform layer. Windows code is behind `cfg(windows)`; portable fallbacks elsewhere so tests run on any OS | core |
| `k92-cli` | `k92 init/backup/snapshots/ls/versions/restore/verify/prune` | all engine crates |
| `k92-service` | Phase 4 | all engine crates |

## Key design decisions

* **The repository is the source of truth.** SQLite (Phase 2 milestone M3) is a *local cache*
  of manifest contents for fast search and timeline queries, rebuilt from the repository at any
  time. Losing it never loses backups.
* **File-level content addressing with BLAKE3.** Unchanged files are detected via
  (size, mtime) against the parent snapshot and reuse the existing hash without re-reading.
  Changed files are streamed once: hashed and written to staging at the same time.
* **Commit point = manifest rename.** Everything before it is invisible; see invariants I1–I9.
* **Single writer, many readers.** Backup/prune/GC take an OS-level exclusive lock on
  `<repo>/lock`. Browsing and restoring need no lock and no admin rights.
* **Untrusted repositories.** Imported repositories are parsed defensively. `RelPath` is the
  only way a manifest path becomes a filesystem path, and it rejects traversal, roots,
  drive letters, ADS colons and reserved characters.
* **Engine in-process for now.** Until the Windows Service exists, the desktop app
  runs the engine on a worker thread. The command surface is the same one the service IPC
  will expose, so the switch is local to `src-tauri`.

## Data flow: backup

1. Acquire writer lock; clear `staging/`.
2. Load latest snapshot (parent) and index its file entries by `(source path, rel path)`.
3. Walk each source root without following reparse points/symlinks. For each file:
   * same size + mtime as parent and object exists → reuse hash;
   * else stream file → hash + staging → rename into `objects/`;
   * verify metadata unchanged after read, else record `changedDuringRead` and skip;
   * lock/permission errors → recorded in `skipped`, backup continues.
4. Check cancellation between files; on cancel nothing is committed.
5. Write manifest to staging, fsync, rename into `snapshots/`. Release lock.

## Data flow: restore

1. Load manifest (checksum + path validation).
2. Select entries (single file, a directory subtree, or a whole source).
3. Compute target path = destination root + validated `RelPath`.
4. Apply conflict policy: `skip`, `rename` (`name (restored 2026-11-09 1030).ext`), `overwrite`
   (explicit only), or `fail` (default; the UI asks first by running a conflict preview).
5. Stream object → temp file next to target while hashing → compare hash → rename → set mtime/readonly.

## Windows Service and IPC (Phase 4 design)

* Service runs as `LocalSystem` only if VSS requires it; otherwise as a virtual service account.
  Repository ACLs grant the interactive user read access for restore without elevation.
* IPC: named pipe `\\.\pipe\k92-service` with a DACL limited to the installing user and
  SYSTEM, plus `GetNamedPipeClientProcessId`/token check. Messages are length-prefixed JSON
  using the same DTOs as Tauri commands.
* Scheduling: hourly default, no overlapping jobs (writer lock + in-memory job state),
  bounded exponential backoff (1, 2, 4 … max 60 min), destination presence checks via
  `WM_DEVICECHANGE`/volume GUID polling.
