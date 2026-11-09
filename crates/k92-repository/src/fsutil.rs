//! Durable, atomic filesystem helpers.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use k92_core::{IoContext, Result};

/// Renames `from` to `to`, retrying briefly on transient Windows errors
/// (antivirus or indexer holding a handle: ERROR_ACCESS_DENIED / ERROR_SHARING_VIOLATION).
pub fn rename_with_retry(from: &Path, to: &Path) -> io::Result<()> {
    let mut delay = Duration::from_millis(20);
    for attempt in 0.. {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if attempt < 5 && is_transient(&e) => {
                thread::sleep(delay);
                delay *= 2;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
}

fn is_transient(e: &io::Error) -> bool {
    cfg!(windows)
        && (e.kind() == io::ErrorKind::PermissionDenied || matches!(e.raw_os_error(), Some(32)))
}

/// Flushes directory metadata so a rename survives power loss. No-op where unsupported (Windows).
pub fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(f) = File::open(dir) {
        let _ = f.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

/// Writes `bytes` to a fresh file in `staging`, fsyncs it, and renames it to `dest`.
/// Readers observe either the old state (no file) or the complete new file.
pub fn write_atomic(staging: &Path, dest: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = staging_file_path(staging);
    let res = (|| {
        let mut f = File::create_new(&tmp).ctx("create", &tmp)?;
        f.write_all(bytes).ctx("write", &tmp)?;
        f.sync_all().ctx("flush", &tmp)?;
        drop(f);
        rename_with_retry(&tmp, dest).ctx("commit", dest)?;
        if let Some(parent) = dest.parent() {
            sync_dir(parent);
        }
        Ok(())
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

pub fn staging_file_path(staging: &Path) -> PathBuf {
    staging.join(format!("{}.tmp", uuid::Uuid::new_v4().simple()))
}
