use serde::{Deserialize, Serialize};

use k92_core::{Error, Result};

pub const REPO_FORMAT: &str = "k92-repository";
pub const SUPPORTED_FORMAT_VERSION: u32 = 2;
/// v1: every object is stored raw.
pub const ENCODING_RAW: &str = "raw";
/// v2: each object is stored either raw (`<hash>`) or zstd-compressed (`<hash>.zst`).
pub const ENCODING_RAW_ZSTD: &str = "raw+zstd";

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
            object_encoding: ENCODING_RAW_ZSTD.into(),
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
        let encoding_ok = match self.format_version {
            1 => self.object_encoding == ENCODING_RAW,
            _ => [ENCODING_RAW, ENCODING_RAW_ZSTD].contains(&self.object_encoding.as_str()),
        };
        if self.hash_algorithm != "blake3" || !encoding_ok {
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

impl Descriptor {
    /// New objects may be written compressed.
    pub fn supports_compression(&self) -> bool {
        self.object_encoding == ENCODING_RAW_ZSTD
    }
}

impl Default for Descriptor {
    fn default() -> Self {
        Self::new()
    }
}
