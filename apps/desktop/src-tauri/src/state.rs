//! Application state, job runner, activity log and the interim in-app scheduler.
//!
//! Until the Windows Service (milestone M7) exists, scheduled backups run only while the app
//! is open (it keeps running in the tray-less background window). The job API here is the same
//! shape the service IPC will expose.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, SecondsFormat, Utc};
use tauri::{AppHandle, Emitter};

use k92_core::dto::{
    ActivityEntry, ActivityOutcome, JobEvent, JobKind, ProgressEvent, RunningJob, SnapshotSummary,
};
use k92_core::{CancelToken, Error, ErrorDto, Result};
use k92_repository::{Repository, Snapshot, SnapshotId};

use crate::config::Config;

pub const JOB_EVENT: &str = "k92://job";

pub fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

struct JobSlot {
    cancel: CancelToken,
    info: RunningJob,
}

pub struct AppState {
    pub config_path: PathBuf,
    activity_path: PathBuf,
    pub config: Mutex<Config>,
    jobs: Mutex<HashMap<JobKind, JobSlot>>,
    /// Last loaded snapshot, for browsing without re-reading the manifest on every click.
    snapshot_cache: Mutex<Option<Arc<Snapshot>>>,
    latest_summary: Mutex<Option<(SnapshotId, SnapshotSummary)>>,
}

/// Outcome a job body reports back.
pub struct JobResult {
    pub outcome: ActivityOutcome,
    pub summary: String,
}

impl AppState {
    pub fn new(data_dir: PathBuf) -> Self {
        let config_path = data_dir.join("config.json");
        let config = Config::load(&config_path);
        AppState {
            activity_path: data_dir.join("activity.jsonl"),
            config_path,
            config: Mutex::new(config),
            jobs: Mutex::new(HashMap::new()),
            snapshot_cache: Mutex::new(None),
            latest_summary: Mutex::new(None),
        }
    }

    pub fn config(&self) -> Config {
        self.config.lock().unwrap().clone()
    }

    pub fn update_config(&self, f: impl FnOnce(&mut Config)) -> Result<Config> {
        let mut c = self.config.lock().unwrap();
        f(&mut c);
        c.save(&self.config_path)?;
        Ok(c.clone())
    }

    pub fn open_repo(&self) -> Result<Repository> {
        let dest = self
            .config()
            .destination
            .ok_or_else(|| Error::InvalidRequest("no backup destination configured".into()))?;
        Repository::open(&PathBuf::from(dest))
    }

    pub fn running(&self) -> Option<RunningJob> {
        let jobs = self.jobs.lock().unwrap();
        // Show backup-type jobs before restore if both run.
        [
            JobKind::Backup,
            JobKind::Verify,
            JobKind::Prune,
            JobKind::Restore,
        ]
        .iter()
        .find_map(|k| jobs.get(k).map(|s| s.info.clone()))
    }

    pub fn is_running(&self, kind: JobKind) -> bool {
        self.jobs.lock().unwrap().contains_key(&kind)
    }

    pub fn cancel(&self, kind: Option<JobKind>) {
        for (k, slot) in self.jobs.lock().unwrap().iter() {
            if kind.is_none_or(|want| want == *k) {
                slot.cancel.cancel();
            }
        }
    }

    pub fn load_snapshot(&self, repo: &Repository, id: &SnapshotId) -> Result<Arc<Snapshot>> {
        if let Some(s) = self.snapshot_cache.lock().unwrap().as_ref()
            && &s.id == id
            && s.repository_id == repo.id()
        {
            return Ok(s.clone());
        }
        let s = Arc::new(repo.load_snapshot(id)?);
        *self.snapshot_cache.lock().unwrap() = Some(s.clone());
        Ok(s)
    }

    /// Summary of the newest valid snapshot, cached by id.
    pub fn latest_summary(&self, repo: &Repository) -> Result<Option<SnapshotSummary>> {
        let ids = repo.snapshot_ids()?;
        let Some(newest) = ids.last() else {
            return Ok(None);
        };
        if let Some((id, sum)) = self.latest_summary.lock().unwrap().as_ref()
            && id == newest
        {
            return Ok(Some(sum.clone()));
        }
        let Some(s) = repo.latest_valid_snapshot()? else {
            return Ok(None);
        };
        let sum = s.summary();
        *self.latest_summary.lock().unwrap() = Some((s.id.clone(), sum.clone()));
        Ok(Some(sum))
    }

    pub fn record_activity(&self, entry: &ActivityEntry) {
        let line = serde_json::to_string(entry).expect("activity serializes");
        let res = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.activity_path)
            .and_then(|mut f| writeln!(f, "{line}"));
        if let Err(e) = res {
            tracing::warn!(error = %e, "could not write activity log");
        }
    }

    /// Most recent entries first.
    pub fn activity(&self, limit: usize) -> Vec<ActivityEntry> {
        let Ok(f) = fs::File::open(&self.activity_path) else {
            return Vec::new();
        };
        let mut all: Vec<ActivityEntry> = BufReader::new(f)
            .lines()
            .map_while(|l| l.ok())
            .filter_map(|l| serde_json::from_str(&l).ok())
            .collect();
        all.reverse();
        all.truncate(limit);
        all
    }

    /// When the scheduler will next run a backup, if it will.
    pub fn next_backup_at(&self) -> Option<DateTime<Utc>> {
        let c = self.config();
        if !c.is_configured() || c.settings.paused || c.settings.interval_minutes == 0 {
            return None;
        }
        let parse = |s: &Option<String>| {
            s.as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc))
        };
        let interval = chrono::Duration::minutes(c.settings.interval_minutes as i64);
        if c.consecutive_failures > 0 {
            // Bounded exponential backoff: 1, 2, 4 … 60 minutes.
            let mins = 1i64 << c.consecutive_failures.min(6).saturating_sub(1);
            let backoff = chrono::Duration::minutes(mins.min(60)).min(interval);
            return Some(parse(&c.last_attempt_at).unwrap_or_else(Utc::now) + backoff);
        }
        Some(match parse(&c.last_success_at) {
            Some(t) => t + interval,
            None => Utc::now(),
        })
    }
}

/// Runs `body` on a worker thread as a job of `kind`, emitting `JobEvent`s.
/// Returns `Busy` if a job of the same kind (or any repository-writing job) is running.
pub fn spawn_job<F>(app: &AppHandle, state: &Arc<AppState>, kind: JobKind, body: F) -> Result<()>
where
    F: FnOnce(&CancelToken, &(dyn Fn(ProgressEvent) + Sync)) -> Result<JobResult> + Send + 'static,
{
    let cancel = CancelToken::new();
    {
        let mut jobs = state.jobs.lock().unwrap();
        let writes = |k: &JobKind| matches!(k, JobKind::Backup | JobKind::Prune | JobKind::Verify);
        if jobs.contains_key(&kind) || (writes(&kind) && jobs.keys().any(writes)) {
            return Err(Error::Busy);
        }
        jobs.insert(
            kind,
            JobSlot {
                cancel: cancel.clone(),
                info: RunningJob {
                    kind,
                    started_at: now(),
                    progress: None,
                },
            },
        );
    }
    let app = app.clone();
    let state = state.clone();
    std::thread::Builder::new()
        .name(format!("k92-{kind:?}"))
        .spawn(move || {
            let started = Instant::now();
            // Long jobs must not be cut off by the PC going to sleep.
            let _awake = matches!(kind, JobKind::Backup | JobKind::Restore | JobKind::Verify)
                .then(k92_windows::KeepAwake::new);
            let _ = app.emit(JOB_EVENT, JobEvent::Started { kind });
            let last_emit = Mutex::new(Instant::now() - Duration::from_secs(1));
            let progress = |p: ProgressEvent| {
                if let Some(slot) = state.jobs.lock().unwrap().get_mut(&kind) {
                    slot.info.progress = Some(p.clone());
                }
                let mut last = last_emit.lock().unwrap();
                if last.elapsed() >= Duration::from_millis(150) {
                    *last = Instant::now();
                    let _ = app.emit(JOB_EVENT, JobEvent::Progress { kind, progress: p });
                }
            };
            let result = body(&cancel, &progress);
            let (outcome, summary, error): (ActivityOutcome, String, Option<ErrorDto>) =
                match result {
                    Ok(r) => (r.outcome, r.summary, None),
                    Err(Error::Cancelled) => (ActivityOutcome::Cancelled, "Cancelled".into(), None),
                    Err(e) => (ActivityOutcome::Failed, e.to_string(), Some(e.to_dto())),
                };
            state.jobs.lock().unwrap().remove(&kind);
            state.record_activity(&ActivityEntry {
                at: now(),
                kind,
                outcome,
                summary: summary.clone(),
                duration_ms: started.elapsed().as_millis() as u64,
            });
            let _ = app.emit(
                JOB_EVENT,
                JobEvent::Finished {
                    kind,
                    outcome,
                    summary,
                    error,
                },
            );
        })
        .map_err(|e| Error::io("start worker thread", e))?;
    Ok(())
}

/// Interim scheduler (runs while the app is open). Checks every 30 s.
pub fn start_scheduler(app: AppHandle, state: Arc<AppState>) {
    std::thread::Builder::new()
        .name("k92-scheduler".into())
        .spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(30));
                let due = state.next_backup_at().is_some_and(|t| t <= Utc::now());
                if due && !state.is_running(JobKind::Backup) {
                    // Destination not connected: wait quietly, try again later.
                    if state.open_repo().is_err() {
                        continue;
                    }
                    if let Err(e) = crate::commands::start_backup_job(&app, &state) {
                        tracing::info!(error = %e, "scheduled backup not started");
                    }
                }
            }
        })
        .expect("spawn scheduler");
}
