# Manual Windows Test Plan (V1)

Run on clean Windows 10 22H2 and Windows 11 VMs, as a **standard (non-admin) user** unless
noted. Use a disposable USB drive or a VHDX attached as a removable disk. Record results with
date, build and OS in the release checklist. Never run these on a developer's main machine data.

| # | Scenario | Steps | Expected |
|---|----------|-------|----------|
| W1 | Install | Run MSI | App starts from Start menu; no admin needed to launch |
| W2 | Onboarding | Choose Documents + Pictures, choose USB drive | First backup starts; Overview shows progress then "Protected" |
| W3 | Same-disk warning | Pick a folder on C: as destination | Warning banner about disk failure is shown |
| W4 | Incremental | Edit one file, click Back up now | Activity: "1 changed"; timeline gains a point |
| W5 | Restore old version | Timeline → earlier point → file → Restore to folder | Old content restored; original untouched |
| W6 | Conflict | Restore to original location over an edited file | Dialog lists conflict; "Keep both" creates "(restored …)" copy |
| W7 | Deleted file | Delete a file, back up, Restore screen → search older backup | File found and restored |
| W8 | Locked file | Open a file with exclusive lock (e.g. Outlook PST), back up | Backup succeeds; file reported as skipped "locked" |
| W9 | OneDrive placeholder | Online-only file in a source | Skipped as cloud placeholder; no download triggered |
| W10 | Junction | `mklink /J` inside a source pointing to C:\Windows | Not followed; reported as skipped |
| W11 | Long path | Enable long paths, create 300-char path | Backed up and restored |
| W12 | Drive removal | Unplug USB mid-backup | Backup fails cleanly; no new snapshot; next run after replug succeeds; `k92 verify --deep` OK |
| W13 | Power loss | Hard power-off VM mid-backup | After reboot: earlier snapshots intact, `k92 verify --deep` OK |
| W14 | Reinstall/import | Uninstall, reinstall, point onboarding at same drive | "Existing backup found"; full history visible; restore works |
| W15 | Portable CLI | On a different PC, run `k92 restore` against the drive | Files restored and verified |
| W16 | Uninstall | Uninstall app | Backups on drive remain untouched |
| W17 | Dark mode / keyboard | Switch Windows to dark; navigate with Tab/arrows/Ctrl+1-6 | All screens readable and operable |
| W18 | Large backup, interrupted | Back up 50 GB+; during copying, cancel (or unplug the drive); back up again | Second run's progress starts near where the first stopped ("to copy" shows only the rest); final snapshot complete; `k92 verify --deep` OK |
| W19 | Destination too small | Choose a source larger than the free space on the backup drive | Backup refuses to start with "not enough space … needs about X, only Y is free"; nothing written |
| W20 | No sleep during backup | Set sleep to 1 minute; run a long backup without touching the PC | PC stays awake until the backup ends, then sleeps normally |
| W21 | Progress for large jobs | Back up 100,000+ files | "Looking for changes… N files found", then a bar with "X of Y to copy" and a time-left estimate |
| W22 | Exclusions | Settings → Leave out: exclude a subfolder, add `*.iso`, set "Larger than 1 GB"; back up | Those items are absent from the new backup (Timeline), activity says "N excluded", nothing reported as skipped; older backups still contain them |
| W23 | Compression | Back up a folder of documents/spreadsheets (and some photos) | Activity shows "… saved by compression"; restored documents open normally and are identical (compare with `fc /b`) |
| W24 | Many small files | Back up a folder with 50,000+ small files (e.g. a code project) to a USB drive; note the time; unplug mid-way on a second run | Completes far faster than one-file-at-a-time; after replug the next run resumes; `k92 verify --deep` OK |
