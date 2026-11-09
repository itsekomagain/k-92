//! Snapshot manifests: model, checksummed envelope, and validation of untrusted input.

use std::collections::HashSet;

use chrono::{DateTime, Duration, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use k92_core::dto::{EntryKind, SkipReason, SnapshotSummary, SourceDto};
use k92_core::{Error, ObjectHash, RelPath, Result};

pub const SNAPSHOT_FORMAT: &str = "k92-snapshot";
pub const SNAPSHOT_FORMAT_VERSION: u32 = 1;

/// `YYYYMMDDTHHMMSSmmmZ-xxxxxxxx`. Validated before it is ever used in a path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SnapshotId(String);

const TS_FMT: &str = "%Y%m%dT%H%M%S%3fZ";

impl SnapshotId {
    pub fn parse(s: &str) -> Result<Self> {
        let b = s.as_bytes();
        let ok = b.len() == 28
            && b[19] == b'-'
            && b[20..]
                .iter()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
            && NaiveDateTime::parse_from_str(&s[..18], "%Y%m%dT%H%M%S%3f").is_ok()
            && b[18] == b'Z';
        if ok {
            Ok(SnapshotId(s.to_owned()))
        } else {
            Err(Error::Corrupt(format!("invalid snapshot id {s:?}")))
        }
    }

    /// New id for `time`, guaranteed to sort after `after` even if the clock went backwards.
    pub fn generate(time: DateTime<Utc>, after: Option<&SnapshotId>) -> Self {
        let mut t = time;
        if let Some(prev) = after
            && let Some(prev_t) = prev.timestamp()
            && t <= prev_t
        {
            t = prev_t + Duration::milliseconds(1);
        }
        let suffix = &uuid::Uuid::new_v4().simple().to_string()[..8];
        SnapshotId(format!("{}-{}", t.format(TS_FMT), suffix))
    }

    pub fn timestamp(&self) -> Option<DateTime<Utc>> {
        NaiveDateTime::parse_from_str(&self.0[..18], "%Y%m%dT%H%M%S%3f")
            .ok()
            .map(|n| n.and_utc())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn file_name(&self) -> String {
        format!("{}.json", self.0)
    }
}

impl TryFrom<String> for SnapshotId {
    type Error = Error;
    fn try_from(s: String) -> Result<Self> {
        SnapshotId::parse(&s)
    }
}

impl From<SnapshotId> for String {
    fn from(s: SnapshotId) -> String {
        s.0
    }
}

impl std::fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    pub hostname: String,
    pub os: String,
}

impl HostInfo {
    pub fn current() -> Self {
        let hostname = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "unknown".into());
        HostInfo {
            hostname,
            os: std::env::consts::OS.into(),
        }
    }
}

/// Key for matching the same source folder across snapshots (Windows paths are case-insensitive).
pub fn source_key(path: &str) -> String {
    if cfg!(windows) {
        path.to_lowercase()
    } else {
        path.to_owned()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SourceRoot {
    pub id: u32,
    /// Absolute path on the machine that made the backup. Informational and used for
    /// "restore to original location"; never trusted blindly.
    pub path: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub source: u32,
    pub path: RelPath,
    pub kind: EntryKind,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub modified_ms: Option<i64>,
    #[serde(default)]
    pub hash: Option<ObjectHash>,
    #[serde(default)]
    pub readonly: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SkippedItem {
    pub source: u32,
    /// Lossy display string; may be a name that was rejected as invalid.
    pub path: String,
    pub reason: SkipReason,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotStats {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub new_objects: u64,
    pub new_bytes: u64,
    pub skipped: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub format: String,
    pub format_version: u32,
    pub id: SnapshotId,
    pub repository_id: String,
    pub parent_id: Option<SnapshotId>,
    pub started_at: String,
    pub completed_at: String,
    pub host: HostInfo,
    pub sources: Vec<SourceRoot>,
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub skipped: Vec<SkippedItem>,
    pub stats: SnapshotStats,
}

impl Snapshot {
    pub fn source(&self, id: u32) -> Option<&SourceRoot> {
        self.sources.iter().find(|s| s.id == id)
    }

    pub fn summary(&self) -> SnapshotSummary {
        SnapshotSummary {
            id: self.id.to_string(),
            started_at: self.started_at.clone(),
            completed_at: self.completed_at.clone(),
            sources: self
                .sources
                .iter()
                .map(|s| SourceDto {
                    id: s.id,
                    path: s.path.clone(),
                    label: s.label.clone(),
                })
                .collect(),
            files: self.stats.files,
            dirs: self.stats.dirs,
            bytes: self.stats.bytes,
            new_bytes: self.stats.new_bytes,
            skipped: self.stats.skipped,
        }
    }

    /// Structural validation of an untrusted manifest. `RelPath` and `ObjectHash`
    /// were already validated during deserialization.
    pub fn validate(&self, expected_id: &SnapshotId, repository_id: &str) -> Result<()> {
        let corrupt = |m: String| Err(Error::Corrupt(format!("snapshot {expected_id}: {m}")));
        if self.format != SNAPSHOT_FORMAT {
            return corrupt(format!("unknown format {:?}", self.format));
        }
        if self.format_version > SNAPSHOT_FORMAT_VERSION {
            return Err(Error::Unsupported(format!(
                "snapshot {expected_id} uses format version {}; update K-92",
                self.format_version
            )));
        }
        if &self.id != expected_id {
            return corrupt(format!("id field {} does not match file name", self.id));
        }
        if self.repository_id != repository_id {
            return corrupt("belongs to a different repository".into());
        }
        let mut source_ids = HashSet::new();
        for s in &self.sources {
            if !source_ids.insert(s.id) {
                return corrupt(format!("duplicate source id {}", s.id));
            }
        }
        let mut seen = HashSet::with_capacity(self.entries.len());
        for e in &self.entries {
            if !source_ids.contains(&e.source) {
                return corrupt(format!(
                    "entry {} references unknown source {}",
                    e.path, e.source
                ));
            }
            if !seen.insert((e.source, &e.path)) {
                return corrupt(format!("duplicate entry {}", e.path));
            }
            match (e.kind, &e.hash) {
                (EntryKind::File, None) => return corrupt(format!("file {} has no hash", e.path)),
                (EntryKind::Dir, Some(_)) => return corrupt(format!("dir {} has a hash", e.path)),
                _ => {}
            }
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct EnvelopeIn<'a> {
    checksum: String,
    #[serde(borrow)]
    body: &'a RawValue,
}

/// Serializes `snap` into the checksummed envelope (see repository-format.md §4).
pub fn encode(snap: &Snapshot) -> Result<Vec<u8>> {
    let body = serde_json::to_string(snap)
        .map_err(|e| Error::InvalidRequest(format!("serialize snapshot: {e}")))?;
    let checksum = blake3::hash(body.as_bytes()).to_hex();
    Ok(format!("{{\"checksum\":\"blake3:{checksum}\",\"body\":{body}}}\n").into_bytes())
}

/// Parses an envelope, verifying the checksum over the exact body bytes.
pub fn decode(bytes: &[u8], expected_id: &SnapshotId, repository_id: &str) -> Result<Snapshot> {
    let corrupt = |m: String| Error::Corrupt(format!("snapshot {expected_id}: {m}"));
    let text = std::str::from_utf8(bytes).map_err(|_| corrupt("not UTF-8".into()))?;
    let env: EnvelopeIn<'_> =
        serde_json::from_str(text).map_err(|e| corrupt(format!("unreadable: {e}")))?;
    let expected = format!(
        "blake3:{}",
        blake3::hash(env.body.get().as_bytes()).to_hex()
    );
    if env.checksum != expected {
        return Err(corrupt("checksum mismatch".into()));
    }
    let snap: Snapshot =
        serde_json::from_str(env.body.get()).map_err(|e| corrupt(format!("invalid body: {e}")))?;
    snap.validate(expected_id, repository_id)?;
    Ok(snap)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: &SnapshotId) -> Snapshot {
        Snapshot {
            format: SNAPSHOT_FORMAT.into(),
            format_version: 1,
            id: id.clone(),
            repository_id: "r".into(),
            parent_id: None,
            started_at: "t".into(),
            completed_at: "t".into(),
            host: HostInfo {
                hostname: "h".into(),
                os: "windows".into(),
            },
            sources: vec![SourceRoot {
                id: 0,
                path: "C:\\Data".into(),
                label: "Data".into(),
            }],
            entries: vec![Entry {
                source: 0,
                path: RelPath::parse("a.txt").unwrap(),
                kind: EntryKind::File,
                size: 1,
                modified_ms: Some(0),
                hash: Some(ObjectHash([1; 32])),
                readonly: false,
            }],
            skipped: vec![],
            stats: SnapshotStats::default(),
        }
    }

    #[test]
    fn id_format_and_ordering() {
        let a = SnapshotId::generate(Utc::now(), None);
        assert!(SnapshotId::parse(a.as_str()).is_ok());
        // Clock moved backwards: still sorts after the previous id.
        let b = SnapshotId::generate(Utc::now() - Duration::hours(5), Some(&a));
        assert!(b > a, "{b} should sort after {a}");
        for bad in [
            "",
            "../../etc",
            "20261109T103000123Z-ABCDEF12",
            "20261399T103000123Z-abcdef12",
        ] {
            assert!(SnapshotId::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn envelope_roundtrip_and_tamper_detection() {
        let id = SnapshotId::generate(Utc::now(), None);
        let snap = sample(&id);
        let bytes = encode(&snap).unwrap();
        assert_eq!(decode(&bytes, &id, "r").unwrap(), snap);

        let tampered = String::from_utf8(bytes.clone())
            .unwrap()
            .replace("a.txt", "b.txt");
        assert!(matches!(
            decode(tampered.as_bytes(), &id, "r"),
            Err(Error::Corrupt(_))
        ));
        assert!(decode(&bytes[..bytes.len() / 2], &id, "r").is_err());
        assert!(decode(&bytes, &id, "other-repo").is_err());
    }

    #[test]
    fn rejects_traversal_even_with_valid_checksum() {
        let id = SnapshotId::generate(Utc::now(), None);
        let bytes = encode(&sample(&id)).unwrap();
        // Attacker rewrites the path AND recomputes the checksum.
        let text = String::from_utf8(bytes).unwrap();
        let body_start = text.find("\"body\":").unwrap() + 7;
        let body = text[body_start..text.len() - 2].replace("a.txt", "../../evil.txt");
        let forged = format!(
            "{{\"checksum\":\"blake3:{}\",\"body\":{body}}}",
            blake3::hash(body.as_bytes()).to_hex()
        );
        let err = decode(forged.as_bytes(), &id, "r").unwrap_err();
        assert!(err.to_string().contains("invalid"), "{err}");
    }
}
