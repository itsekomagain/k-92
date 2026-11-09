//! Tauri commands: the typed IPC surface. Every input from the webview is validated here.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tauri::{AppHandle, State};

use k92_backup::{BackupOptions, SourceSpec, run_backup};
use k92_core::dto::*;
use k92_core::{Error, ErrorDto, RelPath, Result};
use k92_repository::{Repository, SnapshotId};
use k92_restore::{RestoreRequest, RestoreTarget, SnapshotView};

use crate::state::{AppState, JobResult, spawn_job};

type Cmd<T> = std::result::Result<T, ErrorDto>;
type St<'a> = State<'a, Arc<AppState>>;

fn dto<T>(r: Result<T>) -> Cmd<T> {
    r.map_err(|e| {
        tracing::warn!(error = %e, "command failed");
        e.to_dto()
    })
}

/// Paths from the UI must be absolute and free of `..`.
fn checked_path(p: &str) -> Result<PathBuf> {
    let path = PathBuf::from(p);
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(Error::InvalidPath(format!(
            "{p:?} must be an absolute folder path"
        )));
    }
    Ok(path)
}

fn snapshot_id(s: &str) -> Result<SnapshotId> {
    SnapshotId::parse(s).map_err(|_| Error::InvalidRequest(format!("invalid snapshot id {s:?}")))
}

#[tauri::command]
pub fn get_status(state: St<'_>) -> Cmd<AppStatus> {
    let c = state.config();
    let mut status = AppStatus {
        configured: c.is_configured(),
        sources: c.sources.clone(),
        destination: c.destination.clone(),
        destination_available: false,
        destination_error: None,
        last_snapshot: None,
        snapshot_count: 0,
        last_error: c.last_error.clone(),
        running: state.running(),
        next_backup_at: state
            .next_backup_at()
            .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        settings: c.settings.clone(),
    };
    if c.destination.is_some() {
        match state.open_repo() {
            Ok(repo) => {
                status.destination_available = true;
                status.snapshot_count = repo.snapshot_ids().map(|v| v.len() as u64).unwrap_or(0);
                status.last_snapshot = state.latest_summary(&repo).unwrap_or(None);
            }
            Err(e) => status.destination_error = Some(e.to_dto()),
        }
    }
    Ok(status)
}

#[tauri::command]
pub fn inspect_destination(path: String, sources: Vec<String>) -> Cmd<DestinationInfo> {
    dto((|| {
        let p = checked_path(&path)?;
        let exists = p.is_dir();
        let repo = Repository::open(&p).ok();
        let mut info = DestinationInfo {
            path: path.clone(),
            exists,
            writable: false,
            has_repository: repo.is_some(),
            snapshot_count: None,
            last_backup_at: None,
            same_volume_as_source: false,
            problem: None,
        };
        if let Some(r) = &repo {
            info.snapshot_count = r.snapshot_ids().ok().map(|v| v.len() as u64);
            info.last_backup_at = r
                .latest_valid_snapshot()
                .ok()
                .flatten()
                .map(|s| s.completed_at);
        }
        if exists {
            let probe = p.join(format!(".k92-write-test-{}", std::process::id()));
            info.writable = std::fs::write(&probe, b"").is_ok();
            let _ = std::fs::remove_file(&probe);
            for s in &sources {
                let sp = checked_path(s)?;
                if k92_windows::same_volume(&sp, &p) == Some(true) {
                    info.same_volume_as_source = true;
                }
                let canon = |x: &Path| std::fs::canonicalize(x).unwrap_or_else(|_| x.to_path_buf());
                if canon(&p).starts_with(canon(&sp)) {
                    info.problem =
                        Some("The destination is inside a folder you are backing up.".into());
                }
            }
        } else {
            info.problem = Some("Folder not found. Is the drive connected?".into());
        }
        if exists && !info.writable && info.problem.is_none() {
            info.problem = Some("K-92 cannot write to this folder.".into());
        }
        Ok(info)
    })())
}

/// Completes onboarding: opens the existing repository at `destination` or creates one.
#[tauri::command]
pub fn configure(state: St<'_>, sources: Vec<String>, destination: String) -> Cmd<AppStatus> {
    dto((|| {
        if sources.is_empty() {
            return Err(Error::InvalidRequest(
                "choose at least one folder to back up".into(),
            ));
        }
        let mut clean = Vec::new();
        for s in &sources {
            let p = checked_path(s)?;
            if !p.is_dir() {
                return Err(Error::NotFound(format!("folder {s}")));
            }
            clean.push(p.display().to_string());
        }
        let dest = checked_path(&destination)?;
        let repo = match Repository::open(&dest) {
            Ok(r) => r,
            Err(Error::NotARepository(_)) => Repository::init(&dest)?,
            Err(e) => return Err(e),
        };
        state.update_config(|c| {
            c.sources = clean;
            c.destination = Some(repo.root().display().to_string());
            c.consecutive_failures = 0;
            c.last_error = None;
        })?;
        Ok(())
    })())?;
    get_status(state)
}

#[tauri::command]
pub fn update_settings(state: St<'_>, settings: AppSettings) -> Cmd<AppStatus> {
    dto((|| {
        if settings.interval_minutes > 7 * 24 * 60 {
            return Err(Error::InvalidRequest("interval too long".into()));
        }
        state.update_config(|c| c.settings = settings)?;
        Ok(())
    })())?;
    get_status(state)
}

#[tauri::command]
pub fn update_sources(state: St<'_>, sources: Vec<String>) -> Cmd<AppStatus> {
    dto((|| {
        if sources.is_empty() {
            return Err(Error::InvalidRequest("keep at least one folder".into()));
        }
        let clean = sources
            .iter()
            .map(|s| checked_path(s).map(|p| p.display().to_string()))
            .collect::<Result<Vec<_>>>()?;
        state.update_config(|c| c.sources = clean)?;
        Ok(())
    })())?;
    get_status(state)
}

pub fn start_backup_job(app: &AppHandle, state: &Arc<AppState>) -> Result<()> {
    let cfg = state.config();
    if !cfg.is_configured() {
        return Err(Error::InvalidRequest("finish setup first".into()));
    }
    let st = state.clone();
    spawn_job(app, state, JobKind::Backup, move |cancel, progress| {
        let attempt = crate::state::now();
        let result = st.open_repo().and_then(|repo| {
            let opts = BackupOptions {
                sources: cfg.sources.iter().map(SourceSpec::new).collect(),
                rehash_all: false,
            };
            run_backup(&repo, &opts, cancel, progress)
        });
        let _ = st.update_config(|c| {
            c.last_attempt_at = Some(attempt);
            match &result {
                Ok(_) => {
                    c.last_success_at = Some(crate::state::now());
                    c.consecutive_failures = 0;
                    c.last_error = None;
                }
                Err(Error::Cancelled) => {}
                Err(e) => {
                    c.consecutive_failures = c.consecutive_failures.saturating_add(1);
                    c.last_error = Some(e.to_dto());
                }
            }
        });
        let r = result?;
        let skipped = r.skipped.len();
        Ok(JobResult {
            outcome: if skipped > 0 {
                ActivityOutcome::Warning
            } else {
                ActivityOutcome::Success
            },
            summary: format!(
                "Backed up {} files ({} changed){}",
                r.files,
                r.files_copied,
                if skipped > 0 {
                    format!(", {skipped} skipped")
                } else {
                    String::new()
                }
            ),
        })
    })
}

#[tauri::command]
pub fn start_backup(app: AppHandle, state: St<'_>) -> Cmd<()> {
    dto(start_backup_job(&app, &state))
}

#[tauri::command]
pub fn cancel_job(state: St<'_>, kind: Option<JobKind>) {
    state.cancel(kind);
}

#[tauri::command]
pub fn list_snapshots(state: St<'_>) -> Cmd<SnapshotList> {
    dto((|| {
        let repo = state.open_repo()?;
        let (ok, damaged) = repo.load_all()?;
        Ok(SnapshotList {
            snapshots: ok.iter().map(|s| s.summary()).collect(),
            damaged,
        })
    })())
}

#[tauri::command]
pub fn list_dir(
    state: St<'_>,
    snapshot_id: String,
    source: u32,
    path: String,
) -> Cmd<Vec<BrowseEntry>> {
    dto((|| {
        let repo = state.open_repo()?;
        let snap = state.load_snapshot(&repo, &self::snapshot_id(&snapshot_id)?)?;
        SnapshotView::new(&snap).list_dir(source, &RelPath::parse(&path)?)
    })())
}

#[tauri::command]
pub fn search_snapshot(state: St<'_>, snapshot_id: String, query: String) -> Cmd<Vec<BrowseEntry>> {
    dto((|| {
        let repo = state.open_repo()?;
        let snap = state.load_snapshot(&repo, &self::snapshot_id(&snapshot_id)?)?;
        Ok(SnapshotView::new(&snap).search(&query, 200))
    })())
}

#[tauri::command]
pub async fn file_versions(
    state: St<'_>,
    source_path: String,
    path: String,
) -> Cmd<Vec<FileVersion>> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        dto((|| {
            let repo = state.open_repo()?;
            k92_restore::file_versions(&repo, &source_path, &RelPath::parse(&path)?)
        })())
    })
    .await
    .map_err(|e| Error::InvalidRequest(e.to_string()).to_dto())?
}

fn to_request(req: &RestoreRequestDto) -> Result<RestoreRequest> {
    let items = req
        .items
        .iter()
        .map(|i| Ok((i.source, RelPath::parse(&i.path)?)))
        .collect::<Result<Vec<_>>>()?;
    let target = match &req.target_folder {
        Some(f) => RestoreTarget::Folder(checked_path(f)?),
        None => RestoreTarget::Original,
    };
    Ok(RestoreRequest {
        items,
        target,
        policy: req.policy,
    })
}

#[tauri::command]
pub async fn plan_restore(state: St<'_>, request: RestoreRequestDto) -> Cmd<RestorePlan> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        dto((|| {
            let repo = state.open_repo()?;
            let snap = state.load_snapshot(&repo, &snapshot_id(&request.snapshot_id)?)?;
            k92_restore::plan_restore(&snap, &to_request(&request)?)
        })())
    })
    .await
    .map_err(|e| Error::InvalidRequest(e.to_string()).to_dto())?
}

/// Runs a restore and returns its report. Progress is emitted as job events.
#[tauri::command]
pub async fn run_restore(
    app: AppHandle,
    state: St<'_>,
    request: RestoreRequestDto,
) -> Cmd<RestoreReport> {
    let state = state.inner().clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let st = state.clone();
    dto(spawn_job(
        &app,
        &state,
        JobKind::Restore,
        move |cancel, progress| {
            let res = (|| {
                let repo = st.open_repo()?;
                let snap = st.load_snapshot(&repo, &snapshot_id(&request.snapshot_id)?)?;
                k92_restore::restore(&repo, &snap, &to_request(&request)?, cancel, progress)
            })();
            let summary = match &res {
                Ok(r) if !r.failed.is_empty() => (
                    ActivityOutcome::Warning,
                    format!("Restored {} files, {} failed", r.restored, r.failed.len()),
                ),
                Ok(r) if r.restored == 0 && !r.conflicts.is_empty() => (
                    ActivityOutcome::Warning,
                    format!("Restore paused: {} existing files", r.conflicts.len()),
                ),
                Ok(r) => (
                    ActivityOutcome::Success,
                    format!("Restored {} files (verified)", r.restored),
                ),
                Err(e) => (ActivityOutcome::Failed, e.to_string()),
            };
            let out = match &res {
                Ok(_) => Ok(JobResult {
                    outcome: summary.0,
                    summary: summary.1,
                }),
                Err(Error::Cancelled) => Err(Error::Cancelled),
                Err(e) => Err(Error::InvalidRequest(e.to_string())),
            };
            let _ = tx.send(res);
            out
        },
    ))?;
    tauri::async_runtime::spawn_blocking(move || rx.recv())
        .await
        .map_err(|e| Error::InvalidRequest(e.to_string()).to_dto())?
        .map_err(|_| Error::Cancelled.to_dto())?
        .map_err(|e| e.to_dto())
}

#[tauri::command]
pub fn start_verify(app: AppHandle, state: St<'_>, deep: bool) -> Cmd<()> {
    let st = state.inner().clone();
    dto(spawn_job(
        &app,
        &state,
        JobKind::Verify,
        move |cancel, _progress| {
            let r = st.open_repo()?.verify(deep, cancel)?;
            Ok(if r.is_healthy() {
                JobResult {
                    outcome: ActivityOutcome::Success,
                    summary: format!(
                        "Verified {} snapshots and {} stored files{}",
                        r.snapshots_checked,
                        r.objects_checked,
                        if deep { " (full check)" } else { "" }
                    ),
                }
            } else {
                JobResult {
                    outcome: ActivityOutcome::Failed,
                    summary: format!(
                        "Problems found: {} damaged snapshots, {} missing and {} corrupt files affecting {} snapshots",
                        r.damaged_snapshots.len(),
                        r.missing_objects.len(),
                        r.corrupt_objects.len(),
                        r.affected_snapshots.len()
                    ),
                }
            })
        },
    ))
}

#[tauri::command]
pub async fn get_storage(state: St<'_>) -> Cmd<StorageInfo> {
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        dto((|| {
            let repo = state.open_repo()?;
            let latest = state.latest_summary(&repo)?;
            Ok(StorageInfo {
                path: repo.root().display().to_string(),
                repository_bytes: repo.disk_usage()?,
                snapshot_count: repo.snapshot_ids()?.len() as u64,
                latest_logical_bytes: latest.map(|s| s.bytes).unwrap_or(0),
            })
        })())
    })
    .await
    .map_err(|e| Error::InvalidRequest(e.to_string()).to_dto())?
}

#[tauri::command]
pub fn get_activity(state: St<'_>) -> Vec<ActivityEntry> {
    state.activity(200)
}
