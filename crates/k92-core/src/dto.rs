//! Data transfer types shared by the engine, the desktop IPC layer and (later) the service IPC.
//! `cargo test -p k92-core` exports them to `apps/desktop/src/ipc/bindings/*.ts` via ts-rs.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::ErrorDto;

/// Why an item was not included in a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum SkipReason {
    /// File is open exclusively by another program (Windows sharing violation).
    Locked,
    AccessDenied,
    /// Symlink, junction or other reparse point. Not followed, by design.
    ReparsePoint,
    /// OneDrive-style online-only file; reading it would trigger a download.
    CloudPlaceholder,
    /// Name cannot be represented safely (invalid Unicode or reserved characters).
    InvalidName,
    /// File changed while it was being read; it will be retried next backup.
    ChangedDuringRead,
    /// Device, socket, pipe or other non-regular file.
    UnsupportedType,
    ReadError,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SkippedItemDto {
    pub source_path: String,
    pub path: String,
    pub reason: SkipReason,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum JobPhase {
    Preparing,
    Scanning,
    Copying,
    Committing,
    Restoring,
    Verifying,
}

/// Progress notification. Totals are unknown (`None`) while scanning.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ProgressEvent {
    pub phase: JobPhase,
    #[ts(type = "number")]
    pub files_done: u64,
    #[ts(type = "number | null")]
    pub files_total: Option<u64>,
    #[ts(type = "number")]
    pub bytes_done: u64,
    /// Bytes this job expects to read (backup: new and changed files). `None` while unknown.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub bytes_total: Option<u64>,
    pub current_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SourceDto {
    pub id: u32,
    pub path: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BackupReport {
    pub snapshot_id: String,
    #[ts(type = "number")]
    pub files: u64,
    #[ts(type = "number")]
    pub dirs: u64,
    #[ts(type = "number")]
    pub bytes: u64,
    /// Files whose content was read and stored (new or modified since the parent snapshot).
    #[ts(type = "number")]
    pub files_copied: u64,
    /// Files unchanged since the parent snapshot (content reused without reading).
    #[ts(type = "number")]
    pub files_unchanged: u64,
    /// Files present in the parent snapshot but gone now. They remain in older snapshots.
    #[ts(type = "number")]
    pub files_removed: u64,
    #[ts(type = "number")]
    pub new_objects: u64,
    #[ts(type = "number")]
    pub new_bytes: u64,
    pub skipped: Vec<SkippedItemDto>,
    #[ts(type = "number")]
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SnapshotSummary {
    pub id: String,
    /// RFC 3339, UTC.
    pub started_at: String,
    pub completed_at: String,
    pub sources: Vec<SourceDto>,
    #[ts(type = "number")]
    pub files: u64,
    #[ts(type = "number")]
    pub dirs: u64,
    #[ts(type = "number")]
    pub bytes: u64,
    #[ts(type = "number")]
    pub new_bytes: u64,
    #[ts(type = "number")]
    pub skipped: u64,
}

/// A snapshot that exists on disk but failed validation. Shown to the user, never restored from.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DamagedSnapshot {
    pub id: String,
    pub error: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum EntryKind {
    File,
    Dir,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BrowseEntry {
    pub source: u32,
    pub name: String,
    /// Path relative to the source root, `/`-separated.
    pub path: String,
    pub kind: EntryKind,
    #[ts(type = "number")]
    pub size: u64,
    #[ts(type = "number | null")]
    pub modified_ms: Option<i64>,
    pub hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct FileVersion {
    /// First snapshot that contains this exact content.
    pub snapshot_id: String,
    pub snapshot_time: String,
    /// Number of consecutive snapshots that contain this same version.
    #[ts(type = "number")]
    pub snapshot_count: u64,
    #[ts(type = "number")]
    pub size: u64,
    #[ts(type = "number | null")]
    pub modified_ms: Option<i64>,
    pub hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ConflictPolicy {
    /// Refuse to touch existing files. The restore reports each conflict and writes nothing there.
    #[default]
    Fail,
    Skip,
    /// Keep the existing file and restore next to it as `name (restored YYYY-MM-DD HHMM).ext`.
    Rename,
    /// Replace the existing file. Only when explicitly chosen.
    Overwrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ConflictKind {
    /// A different file already exists at the target path.
    Exists,
    /// The existing file has exactly the backed-up content; nothing needs to be written.
    Identical,
    /// A directory exists where a file would go, or the other way round.
    TypeMismatch,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RestoreConflict {
    pub target_path: String,
    pub kind: ConflictKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RestorePlan {
    #[ts(type = "number")]
    pub files: u64,
    #[ts(type = "number")]
    pub dirs: u64,
    #[ts(type = "number")]
    pub bytes: u64,
    pub target_root: String,
    pub conflicts: Vec<RestoreConflict>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RestoreFailure {
    pub path: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RestoreReport {
    /// Files written and verified against their stored hash.
    #[ts(type = "number")]
    pub restored: u64,
    #[ts(type = "number")]
    pub dirs_created: u64,
    #[ts(type = "number")]
    pub already_present: u64,
    #[ts(type = "number")]
    pub skipped_conflicts: u64,
    #[ts(type = "number")]
    pub renamed: u64,
    #[ts(type = "number")]
    pub overwritten: u64,
    #[ts(type = "number")]
    pub bytes: u64,
    pub conflicts: Vec<RestoreConflict>,
    pub failed: Vec<RestoreFailure>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct VerifyReport {
    #[ts(type = "number")]
    pub snapshots_checked: u64,
    #[ts(type = "number")]
    pub objects_checked: u64,
    pub damaged_snapshots: Vec<DamagedSnapshot>,
    /// Objects referenced by a snapshot but absent from the repository.
    pub missing_objects: Vec<String>,
    /// Objects whose content no longer matches their hash (deep verify) or size.
    pub corrupt_objects: Vec<String>,
    /// Snapshot ids affected by missing or corrupt objects.
    pub affected_snapshots: Vec<String>,
    pub deep: bool,
}

impl VerifyReport {
    pub fn is_healthy(&self) -> bool {
        self.damaged_snapshots.is_empty()
            && self.missing_objects.is_empty()
            && self.corrupt_objects.is_empty()
    }
}

// ---------------------------------------------------------------------------------------------
// Desktop application types

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AppSettings {
    /// Minutes between automatic backups. 0 disables the schedule.
    pub interval_minutes: u32,
    pub paused: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            interval_minutes: 60,
            paused: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum JobKind {
    Backup,
    Restore,
    Verify,
    Prune,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RunningJob {
    pub kind: JobKind,
    pub started_at: String,
    pub progress: Option<ProgressEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct AppStatus {
    /// False until onboarding has chosen sources and a destination.
    pub configured: bool,
    pub sources: Vec<String>,
    pub destination: Option<String>,
    /// The backup drive is connected and the repository opened successfully.
    pub destination_available: bool,
    pub destination_error: Option<ErrorDto>,
    pub last_snapshot: Option<SnapshotSummary>,
    #[ts(type = "number")]
    pub snapshot_count: u64,
    pub last_error: Option<ErrorDto>,
    pub running: Option<RunningJob>,
    /// RFC 3339; `None` when paused, unscheduled or not configured.
    pub next_backup_at: Option<String>,
    pub settings: AppSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct DestinationInfo {
    pub path: String,
    pub exists: bool,
    pub writable: bool,
    pub has_repository: bool,
    #[ts(type = "number | null")]
    pub snapshot_count: Option<u64>,
    pub last_backup_at: Option<String>,
    /// Destination is on the same volume as at least one source: no protection from disk failure.
    pub same_volume_as_source: bool,
    pub problem: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct SnapshotList {
    pub snapshots: Vec<SnapshotSummary>,
    pub damaged: Vec<DamagedSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct StorageInfo {
    pub path: String,
    #[ts(type = "number")]
    pub repository_bytes: u64,
    #[ts(type = "number")]
    pub snapshot_count: u64,
    /// Sum of file sizes in the latest snapshot.
    #[ts(type = "number")]
    pub latest_logical_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum ActivityOutcome {
    Success,
    Warning,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct ActivityEntry {
    pub at: String,
    pub kind: JobKind,
    pub outcome: ActivityOutcome,
    pub summary: String,
    #[ts(type = "number")]
    pub duration_ms: u64,
}

/// Restore request as sent by the UI. Validated on the Rust side before use.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RestoreRequestDto {
    pub snapshot_id: String,
    pub items: Vec<RestoreItemDto>,
    /// `None` = original location.
    pub target_folder: Option<String>,
    pub policy: ConflictPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct RestoreItemDto {
    pub source: u32,
    pub path: String,
}

/// Emitted on the `k92://job` event channel.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", tag = "type")]
#[ts(export)]
pub enum JobEvent {
    Started {
        kind: JobKind,
    },
    Progress {
        kind: JobKind,
        progress: ProgressEvent,
    },
    Finished {
        kind: JobKind,
        outcome: ActivityOutcome,
        summary: String,
        error: Option<ErrorDto>,
    },
}
