# High-Risk Windows Implementation Decisions

Each item lists the risk, the decision for v1, and how it is tested.

| # | Area | Risk | v1 decision | Verification |
|---|------|------|-------------|--------------|
| R1 | **Locked files** (Outlook PST, open databases) | `ERROR_SHARING_VIOLATION` (32) / `ERROR_LOCK_VIOLATION` (33) without VSS | Record as `skipped: locked`, continue, retry next run. VSS snapshot in Phase 2 removes most cases. | Windows integration test opens a file with `share_mode(0)` and asserts skip. |
| R2 | **VSS** | Needs admin/backup privilege, COM and writer interaction, and shadow copy cleanup | Not used by the unprivileged desktop path. Implemented only in the service (Phase 2) through `windows` crate COM bindings, always with a fallback to a live read. | Manual and VM tests only. Never run on a developer's main system volume in CI. |
| R3 | **Reparse points** (junctions, symlinks, OneDrive placeholders, dedup) | Loops, escaping the backup root, triggering cloud downloads | Never follow reparse points. Skip and report them. Cloud placeholders (`FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`) are skipped as `cloudPlaceholder` so a backup does not trigger mass downloads. | Unit test on Unix symlinks; Windows test with `mklink /J`. |
| R4 | **Long paths** (>260 chars) | Win32 `MAX_PATH` failures | Rust `std` converts long absolute paths to `\\?\` form automatically. Manifests store relative paths without a length limit. | Integration test with a 300-char nested path (Windows CI). |
| R5 | **Filename encoding** | NTFS allows unpaired UTF-16 surrogates that are not valid Unicode | Such names are skipped with `invalidName`. Manifests are UTF-8 JSON. | Unit test on `RelPath`; Windows test creating such a name through `OsString::from_wide`. |
| R6 | **Case-insensitivity** | Two manifest paths differing only by case collide on restore | Restore conflict detection runs per target path, so the second becomes a conflict, never a silent overwrite. | Unit test. |
| R7 | **Reserved names / characters** | `CON`, `NUL`, `a:b` (ADS), trailing dots/spaces from untrusted manifests | `RelPath` rejects `\ / : * ? " < > |`, control chars, `.`/`..`; restore refuses reserved device names. | Unit tests. |
| R8 | **Change detection** | mtime can be preserved by some tools; size+mtime misses edits | v1 uses size + mtime (+ "deep verify" mode re-hashing everything). Phase 2 adds the NTFS USN journal; on journal wrap or ID change fall back to a full scan. | Tests for touch-only and content-only changes. |
| R9 | **Removable drives** | Drive letter changes, sleeping USB disks, surprise removal mid-backup | Repository identified by `repositoryId`, not drive letter. Mid-backup I/O errors abort without committing (I2). | Disconnect simulated by deleting the repo dir mid-run in a test; manual USB test plan. |
| R10 | **Atomic rename semantics** | `MoveFileEx` replace semantics, antivirus holding handles briefly | Objects/manifests are renamed to *new* names (no replace needed). Retry rename on `ERROR_ACCESS_DENIED` up to 5 times with backoff (AV scanners). | Failure-injection tests. |
| R11 | **Durability** | Write caching on USB drives; `fsync` on directory unsupported on Windows | `File::sync_all` (→ `FlushFileBuffers`) on every staged file before rename. Directory fsync is a no-op on Windows and documented as such. Recommend "Better performance" policy is acceptable because of explicit flushes. | Manual pull-the-plug test on a disposable drive. |
| R12 | **Privileges** | UI must not need admin; service needs some privileges | UI: browse/restore as user. Service: `SeBackupPrivilege` only when VSS is enabled. Installer is the only elevated UI. | Manual install test as standard user. |
| R13 | **Service IPC spoofing** | Another local process issuing commands to the privileged service | Named pipe DACL + client token check + `PIPE_REJECT_REMOTE_CLIENTS`. | Integration test connecting as a different user (VM). |
| R14 | **Antivirus / Controlled Folder Access** | Writes to protected folders blocked on restore | Report `access denied` per file with guidance; never retry indefinitely. | Manual test with CFA enabled. |
| R15 | **Same-disk destination** | No protection against disk failure | Onboarding warns when the destination is on the same physical disk as a source (`IOCTL_STORAGE_GET_DEVICE_NUMBER`, Phase 1 uses volume root comparison). | Unit test of the comparison logic. |

## Developer-machine safety

* Tests only touch directories created by `tempfile`.
* No test or script partitions, formats or images a disk. System-image work (Phase 3+) runs
  only in VMs, using disposable VHDX files.
