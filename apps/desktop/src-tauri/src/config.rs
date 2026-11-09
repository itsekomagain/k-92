//! Per-user app configuration. Losing it never loses backups: the repository is self-describing,
//! and onboarding can re-attach an existing drive.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use k92_core::dto::AppSettings;
use k92_core::{ErrorDto, IoContext, Result};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    pub sources: Vec<String>,
    /// Repository root (`…/K-92 Backup`).
    pub destination: Option<String>,
    #[serde(default)]
    pub settings: AppSettings,
    #[serde(default)]
    pub last_success_at: Option<String>,
    #[serde(default)]
    pub last_attempt_at: Option<String>,
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default)]
    pub last_error: Option<ErrorDto>,
}

impl Config {
    pub fn is_configured(&self) -> bool {
        !self.sources.is_empty() && self.destination.is_some()
    }

    pub fn load(path: &Path) -> Config {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "config unreadable; starting fresh (backups are unaffected)");
                Config::default()
            }),
            Err(_) => Config::default(),
        }
    }

    /// Atomic save: temp file + rename.
    pub fn save(&self, path: &Path) -> Result<()> {
        let dir = path.parent().expect("config path has a parent");
        fs::create_dir_all(dir).ctx("create", dir)?;
        let tmp: PathBuf = path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(self).expect("config serializes");
        fs::write(&tmp, json).ctx("write", &tmp)?;
        fs::rename(&tmp, path).ctx("save", path)?;
        Ok(())
    }
}
