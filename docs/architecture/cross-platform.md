# Cross-Platform Plan: Windows and macOS

Status: Windows is the first release. A first macOS build exists (2026-11-09): the same engine and app,
built as `K-92.app` / `.dmg` (Apple silicon), with keep-awake via `caffeinate`, iCloud "optimised
storage" placeholders skipped, Mac system folders in the junk list, and folder-access prompts.
Still missing on Mac: signing/notarization, names containing `:` or `\\` (skipped), launchd
background backups, Finder tags/xattrs, Intel/universal build.

## What moves between machines

| From → To | Files and folders | File metadata | Installed apps |
|-----------|-------------------|---------------|----------------|
| Win → Win | Yes | Modified time, read-only | No (see below) |
| Mac → Mac | Yes | Modified time, permissions (planned) | No |
| Win → Mac | Yes, into a folder you choose | Modified time | No |
| Mac → Win | Yes, into a folder you choose; names Windows forbids are renamed | Modified time | No |

**K-92 does not move installed applications in any direction.** It backs up files. An installed app
is more than its folder:
* On Windows, apps also depend on registry entries, services, drivers, shared runtimes and license
  activation. Copying `Program Files` produces apps that do not start.
* On macOS, most apps are self-contained bundles, but they still rely on data in `~/Library`, on
  App Store licensing and on code signing.
* Across systems, a Windows `.exe` cannot run on macOS, nor a `.app` on Windows.

Same-machine recovery *with* apps is the job of the planned system image (Phases 3–4), which restores
the whole Windows disk and is not file-based.

## Why cross-OS restore already mostly works

The repository format is OS-neutral by design (see `docs/backup-format/repository-format.md`):
UTF-8 `/`-separated relative paths, BLAKE3 content addressing, timestamps in UTC milliseconds, and
`RelPath` rules that already reject names Windows cannot store. A Mac can open a backup made on
Windows and the reverse; the engine's tests already run on both.

## Work required for a macOS release

| Area | Work |
|------|------|
| Platform layer | `k92-macos` (or `k92-platform`): APFS clones, iCloud "optimised storage" placeholders (skip like OneDrive), skip `.DS_Store` / `.Spotlight-V100`, xattrs and Finder tags (optional) |
| Names | Mac allows `:`, `\`, `?`, trailing dots: today those files are skipped with `invalidName`. Add an escaped-name field to the manifest (format v2) so Mac-only names back up and restore on Mac, and get renamed safely on Windows |
| Case | APFS is usually case-insensitive like NTFS, but can be case-sensitive; restore conflict detection already prevents silent overwrites |
| Background job | `launchd` user agent instead of a Windows Service |
| Permissions | Full Disk Access prompt and guidance for protected folders (Desktop, Documents, Mail) |
| Restore UX | "Restore from another computer": pick a source machine in the timeline; default target is a chosen folder, with a suggested mapping (e.g. `C:\Users\Ada\Documents` → `~/Documents`) |
| Packaging | Signed and notarized `.dmg` (Apple Developer ID), universal binary (Apple silicon + Intel) |
| CI | `macos-latest` job alongside `windows-latest` |

## Future: app inventory ("migration assistant")

Not file backup, but a practical answer to "move my apps":
1. Record installed apps with each snapshot (Windows: `winget list` / uninstall registry keys;
   macOS: `/Applications` bundle IDs, Homebrew, Mac App Store).
2. On a new machine, show the list and reinstall what is available for that OS (winget, Homebrew,
   App Store links), matching cross-platform apps by name/vendor.
3. Restore known per-app settings where a safe mapping exists (opt-in, per app).
