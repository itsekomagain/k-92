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
