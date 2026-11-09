use serde::{Deserialize, Serialize};

use k92_core::{Error, Result};

pub const REPO_FORMAT: &str = "k92-repository";
pub const SUPPORTED_FORMAT_VERSION: u32 = 1;

/// Contents of `k92-repository.json`. Written once at creation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
    pub format: String,
    pub format_version: u32,
    pub repository_id: String,
    pub created_at: String,
    pub hash_algorithm: String,
    pub object_encoding: String,
    /// Reserved for authenticated encryption. `null` in v1.
    pub encryption: Option<serde_json::Value>,
}

impl Descriptor {
    pub fn new() -> Self {
        Descriptor {
            format: REPO_FORMAT.into(),
            format_version: SUPPORTED_FORMAT_VERSION,
            repository_id: uuid::Uuid::new_v4().to_string(),
            created_at: crate::now_rfc3339(),
            hash_algorithm: "blake3".into(),
            object_encoding: "raw".into(),
            encryption: None,
        }
    }

    /// Refuses anything this version cannot read correctly.
    pub fn validate(&self) -> Result<()> {
        if self.format != REPO_FORMAT {
            return Err(Error::NotARepository(format!(
                "unknown format {:?}",
                self.format
            )));
        }
        if self.format_version > SUPPORTED_FORMAT_VERSION {
            return Err(Error::Unsupported(format!(
                "repository format version {} is newer than this K-92 supports ({}); update K-92",
                self.format_version, SUPPORTED_FORMAT_VERSION
            )));
        }
        if self.hash_algorithm != "blake3" || self.object_encoding != "raw" {
            return Err(Error::Unsupported(format!(
                "hash {:?} / encoding {:?}",
                self.hash_algorithm, self.object_encoding
            )));
        }
        if self.encryption.is_some() {
            return Err(Error::Unsupported("encrypted repositories".into()));
        }
        if uuid::Uuid::parse_str(&self.repository_id).is_err() {
            return Err(Error::Corrupt("invalid repositoryId".into()));
        }
        Ok(())
    }
}

impl Default for Descriptor {
    fn default() -> Self {
        Self::new()
    }
}
