//! Platform layer. Windows behaviour lives behind `cfg(windows)`; every function has a
//! portable fallback so the engine and its tests run on any OS.
//!
//! Planned (Phase 2, service only): VSS snapshots, NTFS USN journal, volume GUID tracking.

use std::fs::Metadata;
use std::io;
use std::path::Path;

/// What the backup scanner should do with a directory entry, decided from its metadata
/// (obtained with `symlink_metadata`, so links are never followed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileClass {
    Regular,
    Directory,
    /// Symlink, junction, mount point or other reparse point.
    Link,
    /// Online-only cloud file (OneDrive etc.). Reading it would trigger a download.
    CloudPlaceholder,
    /// Device, pipe, socket, …
    Other,
}

#[cfg(windows)]
mod attrs {
    pub const REPARSE_POINT: u32 = 0x0000_0400;
    pub const OFFLINE: u32 = 0x0000_1000;
    pub const RECALL_ON_OPEN: u32 = 0x0004_0000;
    pub const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
}

pub fn classify(meta: &Metadata) -> FileClass {
    let ft = meta.file_type();
    if ft.is_symlink() {
        return FileClass::Link;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        let a = meta.file_attributes();
        if a & (attrs::RECALL_ON_DATA_ACCESS | attrs::RECALL_ON_OPEN | attrs::OFFLINE) != 0 {
            return FileClass::CloudPlaceholder;
        }
        // Junctions and other reparse points are not always reported as symlinks.
        if a & attrs::REPARSE_POINT != 0 {
            return FileClass::Link;
        }
    }
    if ft.is_dir() {
        FileClass::Directory
    } else if ft.is_file() {
        FileClass::Regular
    } else {
        FileClass::Other
    }
}

/// True for "file is open by another process" errors (ERROR_SHARING_VIOLATION = 32,
/// ERROR_LOCK_VIOLATION = 33). Always false off Windows.
pub fn is_locked_error(e: &io::Error) -> bool {
    cfg!(windows) && matches!(e.raw_os_error(), Some(32 | 33))
}

/// Best-effort check whether two paths live on the same volume.
/// Used to warn when the backup destination would not survive a disk failure.
/// Windows: compares volume prefixes (drive letter / UNC share); Phase 2 will compare
/// physical disk numbers via `IOCTL_STORAGE_GET_DEVICE_NUMBER`.
pub fn same_volume(a: &Path, b: &Path) -> Option<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(std::fs::metadata(a).ok()?.dev() == std::fs::metadata(b).ok()?.dev())
    }
    #[cfg(windows)]
    {
        use std::path::Component;
        let prefix = |p: &Path| -> Option<String> {
            let abs = std::path::absolute(p).ok()?;
            match abs.components().next()? {
                Component::Prefix(pre) => {
                    Some(pre.as_os_str().to_string_lossy().to_ascii_lowercase())
                }
                _ => None,
            }
        };
        Some(prefix(a)? == prefix(b)?)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (a, b);
        None
    }
}

/// Clears or sets the read-only attribute. Used before replacing a file during restore,
/// because Windows refuses to replace read-only files.
pub fn set_readonly(path: &Path, readonly: bool) -> io::Result<()> {
    let mut perms = std::fs::metadata(path)?.permissions();
    if perms.readonly() != readonly {
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(readonly);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

/// Bytes the current user can still write on the volume that holds `path`.
pub fn available_space(path: &Path) -> io::Result<u64> {
    fs4::available_space(path)
}

/// Keeps the PC from going to sleep while held (the display may still turn off).
/// Must be dropped on the thread that created it; it is deliberately `!Send`.
pub struct KeepAwake {
    _not_send: std::marker::PhantomData<*const ()>,
}

impl KeepAwake {
    pub fn new() -> Self {
        #[cfg(windows)]
        set_execution_state(true);
        KeepAwake {
            _not_send: std::marker::PhantomData,
        }
    }
}

impl Default for KeepAwake {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        #[cfg(windows)]
        set_execution_state(false);
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn set_execution_state(awake: bool) {
    use windows_sys::Win32::System::Power::{
        ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
    };
    let flags = if awake {
        ES_CONTINUOUS | ES_SYSTEM_REQUIRED
    } else {
        ES_CONTINUOUS
    };
    // SAFETY: plain Win32 call with no pointers; it only changes this thread's power request.
    // Failure (return 0) is harmless: the PC may then sleep as usual.
    let _ = unsafe { SetThreadExecutionState(flags) };
}

#[cfg(test)]
mod tests {
    use super::KeepAwake;

    #[test]
    fn keep_awake_guard_sets_and_clears() {
        let g = KeepAwake::new();
        drop(g);
        let _again = KeepAwake::default();
    }

    use super::*;

    #[test]
    fn classifies_files_dirs_and_links() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("f");
        std::fs::write(&f, b"x").unwrap();
        assert_eq!(
            classify(&std::fs::symlink_metadata(&f).unwrap()),
            FileClass::Regular
        );
        assert_eq!(
            classify(&std::fs::symlink_metadata(d.path()).unwrap()),
            FileClass::Directory
        );
        #[cfg(unix)]
        {
            let l = d.path().join("l");
            std::os::unix::fs::symlink(&f, &l).unwrap();
            assert_eq!(
                classify(&std::fs::symlink_metadata(&l).unwrap()),
                FileClass::Link
            );
        }
    }

    #[test]
    fn same_volume_for_siblings() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("a")).unwrap();
        std::fs::create_dir(d.path().join("b")).unwrap();
        assert_eq!(
            same_volume(&d.path().join("a"), &d.path().join("b")),
            Some(true)
        );
    }
}
