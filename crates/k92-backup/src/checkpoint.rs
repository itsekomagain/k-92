//! Resume journal for long backups.
//!
//! While a backup runs, every file it stores is appended to `<repo>/checkpoint.jsonl`. If the
//! backup is interrupted (cancel, sleep, unplugged drive, crash) the next run reads the journal
//! and treats those files like files from the previous snapshot: if their size and modified time
//! still match and the object is present, they are not read again. The journal is deleted when a
//! snapshot commits.
//!
//! The journal is advisory and untrusted. Each line is `<16 hex checksum> <json>`; lines that are
//! torn, damaged or fail validation are ignored. A record is only used when its object exists with
//! the recorded size, exactly like a parent-snapshot entry, so a bad journal can at worst cause
//! files to be read again. Records are appended only after the object has been fsynced.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use k92_core::{IoContext, ObjectHash, RelPath, Result};
use serde::{Deserialize, Serialize};

/// Journals larger than this are ignored on load (about 4 million records).
const MAX_LOAD_BYTES: u64 = 1 << 30;
const SYNC_EVERY_BYTES: u64 = 256 * 1024 * 1024;
const SYNC_EVERY: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct Record {
    /// `source_key` of the source folder.
    pub s: String,
    pub p: RelPath,
    pub size: u64,
    pub mtime: i64,
    pub hash: ObjectHash,
    /// The object was newly added to the repository by the interrupted run.
    pub new: bool,
}

pub(crate) type Index = HashMap<(String, RelPath), Record>;

fn line_checksum(json: &[u8]) -> String {
    blake3::hash(json).to_hex()[..16].to_string()
}

/// Reads a journal. Missing, oversized or unreadable journals yield an empty index.
pub(crate) fn load(path: &Path) -> Index {
    let mut out = Index::new();
    let Ok(f) = File::open(path) else {
        return out;
    };
    if f.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > MAX_LOAD_BYTES {
        tracing::warn!("checkpoint too large; ignoring it");
        return out;
    }
    let mut bad = 0u64;
    for line in BufReader::new(f).split(b'\n') {
        let Ok(line) = line else { break };
        if line.is_empty() {
            continue;
        }
        let parsed = (|| {
            let (sum, json) = line.split_at_checked(17)?;
            if sum[16] != b' ' || sum[..16] != *line_checksum(json).as_bytes() {
                return None;
            }
            serde_json::from_slice::<Record>(json).ok()
        })();
        match parsed {
            Some(r) => {
                out.insert((r.s.clone(), r.p.clone()), r);
            }
            None => bad += 1,
        }
    }
    if bad > 0 {
        tracing::warn!(bad, "ignored damaged checkpoint lines");
    }
    out
}

/// Appends records; syncs to disk periodically and on drop.
pub(crate) struct Journal {
    path: PathBuf,
    out: BufWriter<File>,
    unsynced_bytes: u64,
    last_sync: Instant,
}

impl Journal {
    pub fn open(path: &Path) -> Result<Self> {
        let f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ctx("open", path)?;
        let mut out = BufWriter::new(f);
        // Start on a fresh line if a previous run was cut off mid-record.
        if ends_mid_line(path) {
            out.write_all(b"\n").ctx("write", path)?;
        }
        Ok(Journal {
            path: path.to_owned(),
            out,
            unsynced_bytes: 0,
            last_sync: Instant::now(),
        })
    }

    /// Records a stored file. The file's size paces syncing.
    pub fn append(&mut self, rec: &Record) -> Result<()> {
        let json = serde_json::to_vec(rec).expect("record serializes");
        let mut line = Vec::with_capacity(json.len() + 18);
        line.extend_from_slice(line_checksum(&json).as_bytes());
        line.push(b' ');
        line.extend_from_slice(&json);
        line.push(b'\n');
        self.out.write_all(&line).ctx("write", &self.path)?;
        self.unsynced_bytes += rec.size;
        if self.unsynced_bytes >= SYNC_EVERY_BYTES || self.last_sync.elapsed() >= SYNC_EVERY {
            self.sync()?;
        }
        Ok(())
    }

    pub fn sync(&mut self) -> Result<()> {
        self.out.flush().ctx("write", &self.path)?;
        self.out.get_ref().sync_data().ctx("flush", &self.path)?;
        self.unsynced_bytes = 0;
        self.last_sync = Instant::now();
        Ok(())
    }

    /// The backup committed: the journal is no longer needed.
    pub fn finish(self) {
        let path = self.path.clone();
        drop(self);
        if let Err(e) = fs::remove_file(&path) {
            tracing::warn!(error = %e, "could not remove checkpoint");
        }
    }
}

fn ends_mid_line(path: &Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    let mut last = [0u8; 1];
    f.seek(SeekFrom::End(-1)).is_ok() && f.read_exact(&mut last).is_ok() && last[0] != b'\n'
}

impl Drop for Journal {
    fn drop(&mut self) {
        let _ = self.out.flush();
        let _ = self.out.get_ref().sync_data();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(p: &str) -> Record {
        Record {
            s: "c:/users/ada/documents".into(),
            p: RelPath::parse(p).unwrap(),
            size: 42,
            mtime: 1_700_000_000_000,
            hash: ObjectHash([7; 32]),
            new: true,
        }
    }

    #[test]
    fn roundtrip_and_damaged_lines_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("checkpoint.jsonl");
        {
            let mut j = Journal::open(&path).unwrap();
            j.append(&rec("a.txt")).unwrap();
            j.append(&rec("b/c.txt")).unwrap();
        }
        // A torn final line, a line with a bad checksum, and an invalid path.
        let mut raw = fs::read(&path).unwrap();
        let good = String::from_utf8(raw.clone()).unwrap();
        let tampered = good.lines().next().unwrap().replace("a.txt", "x.txt");
        raw.extend_from_slice(tampered.as_bytes());
        raw.push(b'\n');
        let evil = br#"{"s":"k","p":"../../etc","size":1,"mtime":1,"hash":"0707070707070707070707070707070707070707070707070707070707070707","new":false}"#;
        raw.extend_from_slice(line_checksum(evil).as_bytes());
        raw.push(b' ');
        raw.extend_from_slice(evil);
        raw.push(b'\n');
        raw.extend_from_slice(b"0123456789abcdef {\"s\":");
        fs::write(&path, raw).unwrap();

        let idx = load(&path);
        assert_eq!(idx.len(), 2, "{idx:?}");
        let key = (
            "c:/users/ada/documents".to_string(),
            RelPath::parse("b/c.txt").unwrap(),
        );
        assert_eq!(idx[&key], rec("b/c.txt"));

        // Appending after a torn line still yields readable records.
        {
            let mut j = Journal::open(&path).unwrap();
            j.append(&rec("d.txt")).unwrap();
        }
        assert_eq!(load(&path).len(), 3, "record after a torn line is kept");
        Journal::open(&path).unwrap().finish();
        assert!(!path.exists());
        assert!(load(&path).is_empty());
    }
}
