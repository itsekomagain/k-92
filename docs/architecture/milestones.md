# Implementation Plan and Milestones

Each milestone is complete only once its tests pass. Status is updated as work lands.

| ID | Milestone | Scope | Exit criteria | Status |
|----|-----------|-------|---------------|--------|
| M0 | Foundation | Cargo workspace, crates, strict TS, fmt/clippy, Tauri shell, typed IPC | `cargo test`, `cargo clippy -D warnings`, `npm run typecheck`, app launches | done on dev host; Windows build via CI pending first run |
| M1 | Repository core | Descriptor, objects, manifests, lock, staging, verify | Format invariant tests (I1–I8) pass | done |
| M2 | Backup engine | Scan, incremental, deletions, skip reporting, cancel, resume | Backup/modify/backup/restore-old-version test passes | done (VSS/USN in M9) |
| M3 | Restore engine | Browse, search, versions, restore file/dir, conflicts, verify | Conflict, traversal, and corruption tests pass | done |
| M4 | Desktop MVP | Onboarding, overview, timeline, browser, restore wizard, activity | Manual Windows test script passes (docs/recovery/manual-windows-tests.md) | UI built; Windows manual run pending |
| M5 | Local catalog | SQLite cache of manifests for search/timeline at scale; rebuild from repo | Catalog rebuild == manifests | not started |
| M6 | Retention + GC | Hourly 24h / daily 30d / weekly forever policy, GC, storage report | Retention never removes the latest snapshot; GC respects I8 | not started (prune primitives exist) |
| M7 | Windows Service | Scheduler, named-pipe IPC, drive detection, backoff, notifications | Backups continue with UI closed | not started |
| M8 | Packaging | WiX/MSI via Tauri bundler, service registration, uninstall keeps backups | Clean install/uninstall on Windows 10 and 11 VMs | not started |
| M9 | Phase 2 | VSS, USN journal, encryption, network targets | Separate plan | not started |

## Order of work

M1 → M2 → M3 are built and tested first because they carry the data-integrity risk.
M0/M4 run alongside them so the UI is always backed by the real engine, never by mock data.
