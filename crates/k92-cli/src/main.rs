//! `k92`: portable command-line tool for K-92 repositories.
//!
//! Works without the desktop app or any local database, so it doubles as the
//! recovery utility for a drive taken to another PC.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};

use k92_backup::{BackupOptions, SourceSpec, run_backup};
use k92_core::dto::{ConflictPolicy, EntryKind};
use k92_core::{CancelToken, RelPath, no_progress};
use k92_repository::Repository;
use k92_restore::{
    RestoreRequest, RestoreTarget, SnapshotView, file_versions, plan_restore, restore,
};

#[derive(Parser)]
#[command(
    name = "k92",
    version,
    about = "K-92 backup and recovery (portable CLI)",
    long_version = concat!(
        env!("CARGO_PKG_VERSION"),
        "\nCopyright © 2026 Ekomobong Otu · https://ekom.blog\nLicense GPL-3.0-or-later"
    )
)]
struct Cli {
    /// Print machine-readable JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a new backup repository inside DEST (as "DEST/K-92 Backup").
    Init { dest: PathBuf },
    /// Back up one or more folders.
    Backup {
        #[arg(long)]
        repo: PathBuf,
        #[arg(required = true)]
        sources: Vec<PathBuf>,
        /// Re-read every file instead of trusting size + modification time.
        #[arg(long)]
        deep: bool,
    },
    /// List snapshots, oldest first.
    Snapshots {
        #[arg(long)]
        repo: PathBuf,
    },
    /// List a folder inside a snapshot.
    Ls {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long, default_value = "latest")]
        snapshot: String,
        #[arg(long, default_value_t = 0)]
        source: u32,
        #[arg(default_value = "")]
        path: String,
    },
    /// Find files by name in a snapshot.
    Search {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long, default_value = "latest")]
        snapshot: String,
        query: String,
    },
    /// Show every stored version of a file.
    Versions {
        #[arg(long)]
        repo: PathBuf,
        /// Source folder as recorded in the snapshots (see `k92 snapshots`).
        #[arg(long)]
        source_path: String,
        path: String,
    },
    /// Restore a file or folder.
    Restore {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long, default_value = "latest")]
        snapshot: String,
        #[arg(long, default_value_t = 0)]
        source: u32,
        /// File or folder inside the source; empty restores the whole source.
        #[arg(long, default_value = "")]
        path: String,
        /// Destination folder. Omit together with --original to restore in place.
        #[arg(long, conflicts_with = "original")]
        to: Option<PathBuf>,
        #[arg(long)]
        original: bool,
        #[arg(long, value_enum, default_value_t = OnConflict::Fail)]
        on_conflict: OnConflict,
        /// Only show what would happen.
        #[arg(long)]
        dry_run: bool,
    },
    /// Check snapshots and stored data for damage.
    Verify {
        #[arg(long)]
        repo: PathBuf,
        /// Re-hash every stored object (slow, thorough).
        #[arg(long)]
        deep: bool,
    },
    /// Apply the default retention policy, then free unreferenced data.
    Prune {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum OnConflict {
    Fail,
    Skip,
    Rename,
    Overwrite,
}

impl From<OnConflict> for ConflictPolicy {
    fn from(o: OnConflict) -> Self {
        match o {
            OnConflict::Fail => ConflictPolicy::Fail,
            OnConflict::Skip => ConflictPolicy::Skip,
            OnConflict::Rename => ConflictPolicy::Rename,
            OnConflict::Overwrite => ConflictPolicy::Overwrite,
        }
    }
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("K92_LOG")
                .unwrap_or_else(|_| "warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn print<T: serde::Serialize>(json: bool, value: &T, text: impl FnOnce()) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(value).expect("serializable")
        );
    } else {
        text();
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let json = cli.json;
    let cancel = CancelToken::new();
    match cli.cmd {
        Cmd::Init { dest } => {
            let repo = Repository::init(&dest)?;
            println!("Created backup at {}", repo.root().display());
        }
        Cmd::Backup {
            repo,
            sources,
            deep,
        } => {
            let repo = Repository::open(&repo)?;
            let opts = BackupOptions {
                sources: sources.into_iter().map(SourceSpec::new).collect(),
                rehash_all: deep,
                ..Default::default()
            };
            let r = run_backup(&repo, &opts, &cancel, &no_progress)?;
            print(json, &r, || {
                println!("Snapshot {}", r.snapshot_id);
                println!(
                    "  {} files ({} copied, {} unchanged, {} removed since last), {} folders",
                    r.files, r.files_copied, r.files_unchanged, r.files_removed, r.dirs
                );
                println!("  {} new bytes stored in {} ms", r.new_bytes, r.duration_ms);
                for s in &r.skipped {
                    println!(
                        "  skipped {:?}: {} {}",
                        s.reason,
                        s.path,
                        s.detail.as_deref().unwrap_or("")
                    );
                }
            });
        }
        Cmd::Snapshots { repo } => {
            let repo = Repository::open(&repo)?;
            let (ok, bad) = repo.load_all()?;
            let summaries: Vec<_> = ok.iter().map(|s| s.summary()).collect();
            print(
                json,
                &serde_json::json!({ "snapshots": summaries, "damaged": bad }),
                || {
                    for s in &summaries {
                        let srcs: Vec<_> = s
                            .sources
                            .iter()
                            .map(|x| format!("[{}] {}", x.id, x.path))
                            .collect();
                        println!(
                            "{}  {:>7} files  {:>12} bytes  {}",
                            s.id,
                            s.files,
                            s.bytes,
                            srcs.join(", ")
                        );
                    }
                    for d in &bad {
                        println!("{}  DAMAGED: {}", d.id, d.error);
                    }
                },
            );
        }
        Cmd::Ls {
            repo,
            snapshot,
            source,
            path,
        } => {
            let repo = Repository::open(&repo)?;
            let snap = repo.load_snapshot(&repo.resolve_snapshot(&snapshot)?)?;
            let list = SnapshotView::new(&snap).list_dir(source, &RelPath::parse(&path)?)?;
            print(json, &list, || {
                for e in &list {
                    match e.kind {
                        EntryKind::Dir => println!("{:>14}  {}/", "", e.name),
                        EntryKind::File => println!("{:>14}  {}", e.size, e.name),
                    }
                }
            });
        }
        Cmd::Search {
            repo,
            snapshot,
            query,
        } => {
            let repo = Repository::open(&repo)?;
            let snap = repo.load_snapshot(&repo.resolve_snapshot(&snapshot)?)?;
            let hits = SnapshotView::new(&snap).search(&query, 500);
            print(json, &hits, || {
                hits.iter()
                    .for_each(|h| println!("[{}] {}", h.source, h.path))
            });
        }
        Cmd::Versions {
            repo,
            source_path,
            path,
        } => {
            let repo = Repository::open(&repo)?;
            let v = file_versions(&repo, &source_path, &RelPath::parse(&path)?)?;
            print(json, &v, || {
                for x in &v {
                    println!(
                        "{}  {:>12} bytes  in {} snapshot(s)  {}",
                        x.snapshot_id,
                        x.size,
                        x.snapshot_count,
                        &x.hash[..16]
                    );
                }
            });
        }
        Cmd::Restore {
            repo,
            snapshot,
            source,
            path,
            to,
            original,
            on_conflict,
            dry_run,
        } => {
            let repo = Repository::open(&repo)?;
            let snap = repo.load_snapshot(&repo.resolve_snapshot(&snapshot)?)?;
            let target = match (to, original) {
                (Some(dir), false) => RestoreTarget::Folder(dir),
                (None, true) => RestoreTarget::Original,
                _ => bail!("pass either --to <folder> or --original"),
            };
            let req = RestoreRequest {
                items: vec![(source, RelPath::parse(&path).context("invalid --path")?)],
                target,
                policy: on_conflict.into(),
            };
            if dry_run {
                let p = plan_restore(&snap, &req)?;
                print(json, &p, || {
                    println!(
                        "Would restore {} files, {} folders ({} bytes) into {}",
                        p.files, p.dirs, p.bytes, p.target_root
                    );
                    for c in &p.conflicts {
                        println!("  conflict {:?}: {}", c.kind, c.target_path);
                    }
                });
                return Ok(ExitCode::SUCCESS);
            }
            let r = restore(&repo, &snap, &req, &cancel, &no_progress)?;
            let blocked = matches!(req.policy, ConflictPolicy::Fail) && !r.conflicts.is_empty();
            print(json, &r, || {
                if blocked {
                    println!(
                        "Nothing restored: {} existing file(s) would be affected.",
                        r.conflicts.len()
                    );
                    for c in &r.conflicts {
                        println!("  {:?}: {}", c.kind, c.target_path);
                    }
                    println!("Re-run with --on-conflict skip|rename|overwrite.");
                } else {
                    println!(
                        "Restored {} files ({} bytes, verified), {} already present, {} skipped, {} renamed, {} overwritten",
                        r.restored,
                        r.bytes,
                        r.already_present,
                        r.skipped_conflicts,
                        r.renamed,
                        r.overwritten
                    );
                }
                for f in &r.failed {
                    println!("  FAILED {}: {}", f.path, f.message);
                }
            });
            if blocked || !r.failed.is_empty() {
                return Ok(ExitCode::from(1));
            }
        }
        Cmd::Verify { repo, deep } => {
            let repo = Repository::open(&repo)?;
            let r = repo.verify(deep, &cancel)?;
            print(json, &r, || {
                println!(
                    "{} snapshots, {} objects checked ({})",
                    r.snapshots_checked,
                    r.objects_checked,
                    if deep { "deep" } else { "quick" }
                );
                for d in &r.damaged_snapshots {
                    println!("  damaged snapshot {}: {}", d.id, d.error);
                }
                for o in &r.missing_objects {
                    println!("  missing object {o}");
                }
                for o in &r.corrupt_objects {
                    println!("  corrupt object {o}");
                }
                println!(
                    "{}",
                    if r.is_healthy() {
                        "OK"
                    } else {
                        "PROBLEMS FOUND"
                    }
                );
            });
            if !r.is_healthy() {
                return Ok(ExitCode::from(1));
            }
        }
        Cmd::Prune { repo, dry_run } => {
            let repo = Repository::open(&repo)?;
            let ids = repo.snapshot_ids()?;
            let doomed = k92_repository::retention::plan(&ids, chrono_now(), &Default::default());
            if dry_run {
                doomed.iter().for_each(|d| println!("would delete {d}"));
                return Ok(ExitCode::SUCCESS);
            }
            let w = repo.writer()?;
            for id in &doomed {
                w.delete_snapshot(id)?;
            }
            let gc = w.gc(&cancel)?;
            println!(
                "Deleted {} snapshots; freed {} bytes in {} objects",
                doomed.len(),
                gc.bytes_freed,
                gc.objects_deleted
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn chrono_now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}
