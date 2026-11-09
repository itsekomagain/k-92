# K-92: Backup & Recovery for Windows

*Your PC. Your history. Your way back.*

K-92 is a **Windows 10/11 (x64) desktop application** that backs up your folders to an external
drive every hour, keeps a browsable history, and restores files or whole folders, with every
restored file verified against its stored BLAKE3 hash.

> Platform note: K-92 targets Windows only. The engine is portable Rust, so its tests also run
> on macOS/Linux developer machines. The shipped product, installers and Windows-specific
> behaviour (locked files, reparse points, OneDrive placeholders, service) are Windows-only and
> are built and tested by `.github/workflows/windows.yml` on `windows-latest`.

## Layout

```
apps/desktop/            Tauri 2 + React + TypeScript app (Windows installer: MSI/NSIS)
crates/k92-core          shared types, validated paths, errors, IPC DTOs (exported to TS)
crates/k92-repository    on-disk backup format, atomic commits, lock, verify, retention, GC
crates/k92-backup        incremental backup engine
crates/k92-restore       browse, search, versions, conflict-safe verified restore
crates/k92-windows       Windows platform layer (portable fallbacks for tests)
crates/k92-cli           `k92`: portable recovery tool, works without the app
tests/integration        end-to-end backup/restore tests
docs/                    architecture, risks, repository format, test plans
```

## Build on Windows

Prerequisites: Rust stable (MSVC), Node 22+, WebView2 (preinstalled on Windows 11),
Visual Studio Build Tools with the C++ workload.

```powershell
cargo test --workspace --exclude k92-desktop   # engine + integration tests
cd apps/desktop
npm ci
npx tauri dev                                  # run the app
npx tauri build                                # MSI + NSIS installers in target\release\bundle
```

`npm run bindings` regenerates the TypeScript IPC types from Rust.

## Developing on macOS/Linux

Engine work and tests run anywhere: `cargo test`. To type-check the Windows code paths:

```sh
rustup target add x86_64-pc-windows-msvc
cargo check --target x86_64-pc-windows-msvc -p k92-cli -p k92-windows --features k92-repository/pure-hash
```

The desktop crate cannot be fully built off Windows (its build embeds Windows resources with
`rc.exe`); use the Windows CI job.

## Portable recovery

```powershell
k92 snapshots --repo E:\
k92 restore --repo E:\ --snapshot latest --path "Reports/q3.xlsx" --to C:\Recovered
k92 verify --repo E:\ --deep
```

## Status

See `docs/architecture/milestones.md`. Phase 1 engine, CLI and desktop UI are implemented;
the Windows Service, VSS, USN journal, SQLite catalog and signed installer are not yet.

## License

K-92 is free software: you can redistribute it and/or modify it under the terms of the
GNU General Public License as published by the Free Software Foundation, either version 3
of the License, or (at your option) any later version. See [LICENSE](LICENSE).
