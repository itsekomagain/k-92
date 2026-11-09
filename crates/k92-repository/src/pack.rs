//! Pack files (format v2): many small objects stored back to back in one file, so backing up
//! many small files needs one flush to disk per pack instead of one per file.
//!
//! Layout: `packs/<id>.pack` holds the object blobs; `packs/<id>.idx` lists them. A pack counts
//! only once its index exists (the index is written after the pack is flushed and renamed into
//! place). A `.pack` without an `.idx` is the leftover of an interrupted write and is removed by
//! cleanup. Indexes are untrusted input: checksummed, and every entry must lie inside the pack.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use k92_core::{Error, IoContext, ObjectHash, Result};

use crate::fsutil;

/// A pack is closed once it reaches this size.
pub const PACK_TARGET_SIZE: u64 = 32 * 1024 * 1024;
/// Files smaller than this go into packs; larger ones are stored as separate object files.
pub const SMALL_OBJECT: u64 = 1024 * 1024;
const INDEX_FORMAT: &str = "k92-pack-index";
const MAX_INDEX_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Encoding {
    Raw,
    Zstd,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackEntry {
    pub hash: ObjectHash,
    pub offset: u64,
    /// Bytes in the pack.
    pub len: u64,
    /// Original (uncompressed) size.
    pub size: u64,
    pub encoding: Encoding,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexBody {
    format: String,
    pack_id: String,
    pack_size: u64,
    entries: Vec<PackEntry>,
}

#[derive(Deserialize)]
struct EnvelopeIn<'a> {
    checksum: String,
    #[serde(borrow)]
    body: &'a RawValue,
}

/// Where a packed object lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackLoc {
    pub pack_id: Arc<str>,
    pub offset: u64,
    pub len: u64,
    pub size: u64,
    pub encoding: Encoding,
}

/// All committed packs, loaded from their indexes.
#[derive(Debug, Default)]
pub struct PackIndex {
    pub objects: HashMap<ObjectHash, PackLoc>,
    /// Pack id -> objects it holds.
    pub packs: HashMap<String, Vec<ObjectHash>>,
    /// Packs whose index is unreadable or inconsistent. Cleanup never deletes these.
    pub damaged: Vec<(String, String)>,
    /// `.pack` files without an index (interrupted writes).
    pub orphans: Vec<String>,
}

pub fn is_pack_id(s: &str) -> bool {
    s.len() == 32
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn pack_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.pack"))
}

pub fn index_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.idx"))
}

pub fn encode_index(pack_id: &str, pack_size: u64, entries: &[PackEntry]) -> Vec<u8> {
    let body = serde_json::to_string(&IndexBody {
        format: INDEX_FORMAT.into(),
        pack_id: pack_id.into(),
        pack_size,
        entries: entries.to_vec(),
    })
    .expect("index serializes");
    let checksum = blake3::hash(body.as_bytes()).to_hex();
    format!("{{\"checksum\":\"blake3:{checksum}\",\"body\":{body}}}\n").into_bytes()
}

/// Parses and validates an index against the pack it describes.
pub fn decode_index(bytes: &[u8], pack_id: &str, actual_pack_size: u64) -> Result<Vec<PackEntry>> {
    let bad = |m: String| Error::Corrupt(format!("pack {pack_id}: {m}"));
    let env: EnvelopeIn =
        serde_json::from_slice(bytes).map_err(|e| bad(format!("index unreadable: {e}")))?;
    let expected = format!(
        "blake3:{}",
        blake3::hash(env.body.get().as_bytes()).to_hex()
    );
    if env.checksum != expected {
        return Err(bad("index checksum mismatch".into()));
    }
    let body: IndexBody =
        serde_json::from_str(env.body.get()).map_err(|e| bad(format!("index unreadable: {e}")))?;
    if body.format != INDEX_FORMAT || body.pack_id != pack_id {
        return Err(bad("index does not belong to this pack".into()));
    }
    if body.pack_size != actual_pack_size {
        return Err(bad(format!(
            "pack is {actual_pack_size} bytes, index expects {}",
            body.pack_size
        )));
    }
    for e in &body.entries {
        let end = e
            .offset
            .checked_add(e.len)
            .ok_or_else(|| bad("bad offset".into()))?;
        if end > actual_pack_size {
            return Err(bad(format!("object {} lies outside the pack", e.hash)));
        }
        if e.encoding == Encoding::Raw && e.len != e.size {
            return Err(bad(format!("object {} has inconsistent size", e.hash)));
        }
    }
    Ok(body.entries)
}

impl PackIndex {
    /// Reads every index in `dir`. Problems are recorded, never fatal.
    pub fn load(dir: &Path) -> PackIndex {
        let mut idx = PackIndex::default();
        let Ok(rd) = fs::read_dir(dir) else {
            return idx;
        };
        let mut pack_files = HashSet::new();
        let mut index_files = Vec::new();
        for e in rd.flatten() {
            let name = e.file_name();
            let Some(name) = name.to_str() else { continue };
            if let Some(id) = name.strip_suffix(".pack").filter(|i| is_pack_id(i)) {
                pack_files.insert(id.to_string());
            } else if let Some(id) = name.strip_suffix(".idx").filter(|i| is_pack_id(i)) {
                index_files.push(id.to_string());
            }
        }
        index_files.sort();
        for id in &index_files {
            match load_one(dir, id) {
                Ok(entries) => idx.add(id, &entries),
                Err(e) => {
                    tracing::warn!(pack = %id, error = %e, "damaged pack");
                    idx.damaged.push((id.clone(), e.to_string()));
                }
            }
        }
        let indexed: HashSet<&String> = index_files.iter().collect();
        idx.orphans = pack_files
            .into_iter()
            .filter(|p| !indexed.contains(p))
            .collect();
        idx.orphans.sort();
        idx
    }

    pub fn add(&mut self, id: &str, entries: &[PackEntry]) {
        let pid: Arc<str> = id.into();
        let mut list = Vec::with_capacity(entries.len());
        for e in entries {
            list.push(e.hash);
            self.objects.entry(e.hash).or_insert_with(|| PackLoc {
                pack_id: pid.clone(),
                offset: e.offset,
                len: e.len,
                size: e.size,
                encoding: e.encoding,
            });
        }
        self.packs.insert(id.to_string(), list);
    }
}

fn load_one(dir: &Path, id: &str) -> Result<Vec<PackEntry>> {
    let ip = index_path(dir, id);
    let pp = pack_path(dir, id);
    let pack_size = fs::metadata(&pp)
        .map_err(|_| Error::Corrupt(format!("pack {id}: pack file missing")))?
        .len();
    let len = fs::metadata(&ip).ctx("read", &ip)?.len();
    if len > MAX_INDEX_BYTES {
        return Err(Error::Corrupt(format!("pack {id}: index too large")));
    }
    let bytes = fs::read(&ip).ctx("read", &ip)?;
    decode_index(&bytes, id, pack_size)
}

/// A pack being written (lives in `staging/` until committed).
pub(crate) struct OpenPack {
    pub id: String,
    pub tmp: PathBuf,
    out: BufWriter<File>,
    pub offset: u64,
    pub entries: Vec<PackEntry>,
    pub hashes: HashSet<ObjectHash>,
}

impl OpenPack {
    pub fn create(staging: &Path) -> Result<Self> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let tmp = staging.join(format!("{id}.pack.tmp"));
        let f = File::create_new(&tmp).ctx("create", &tmp)?;
        Ok(OpenPack {
            id,
            tmp,
            out: BufWriter::with_capacity(1024 * 1024, f),
            offset: 0,
            entries: Vec::new(),
            hashes: HashSet::new(),
        })
    }

    pub fn append(
        &mut self,
        hash: ObjectHash,
        size: u64,
        encoding: Encoding,
        blob: &[u8],
    ) -> Result<()> {
        self.out.write_all(blob).ctx("write", &self.tmp)?;
        self.entries.push(PackEntry {
            hash,
            offset: self.offset,
            len: blob.len() as u64,
            size,
            encoding,
        });
        self.offset += blob.len() as u64;
        self.hashes.insert(hash);
        Ok(())
    }

    /// Flush + fsync the pack, move it into `packs/`, then write its index (the commit point).
    pub fn commit(self, staging: &Path, packs_dir: &Path) -> Result<Vec<PackEntry>> {
        let OpenPack {
            id,
            tmp,
            out,
            offset,
            entries,
            ..
        } = self;
        let f = out
            .into_inner()
            .map_err(|e| Error::io_path("write", &tmp, e.into_error()))?;
        f.sync_all().ctx("flush", &tmp)?;
        drop(f);
        fs::create_dir_all(packs_dir).ctx("create", packs_dir)?;
        let dest = pack_path(packs_dir, &id);
        fsutil::rename_with_retry(&tmp, &dest).ctx("store", &dest)?;
        fsutil::sync_dir(packs_dir);
        let bytes = encode_index(&id, offset, &entries);
        fsutil::write_atomic(staging, &index_path(packs_dir, &id), &bytes)?;
        tracing::debug!(pack = %id, objects = entries.len(), bytes = offset, "committed pack");
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(n: u8, offset: u64, len: u64) -> PackEntry {
        PackEntry {
            hash: ObjectHash([n; 32]),
            offset,
            len,
            size: len,
            encoding: Encoding::Raw,
        }
    }

    #[test]
    fn index_roundtrip_and_validation() {
        let id = "0123456789abcdef0123456789abcdef";
        let entries = vec![entry(1, 0, 10), entry(2, 10, 5)];
        let bytes = encode_index(id, 15, &entries);
        assert_eq!(decode_index(&bytes, id, 15).unwrap(), entries);
        // Truncated pack, wrong id, tampered body.
        assert!(decode_index(&bytes, id, 14).is_err());
        assert!(decode_index(&bytes, "ffffffffffffffffffffffffffffffff", 15).is_err());
        let tampered = String::from_utf8(bytes.clone())
            .unwrap()
            .replace("\"len\":5", "\"len\":6");
        assert!(decode_index(tampered.as_bytes(), id, 15).is_err());
        // An entry outside the pack is rejected even with a valid checksum.
        let evil = encode_index(id, 15, &[entry(3, 10, 50)]);
        assert!(decode_index(&evil, id, 15).is_err());
        let overflow = encode_index(id, 15, &[entry(3, u64::MAX, 2)]);
        assert!(decode_index(&overflow, id, 15).is_err());
    }

    #[test]
    fn pack_ids_are_strict() {
        assert!(is_pack_id("0123456789abcdef0123456789abcdef"));
        assert!(!is_pack_id("0123456789ABCDEF0123456789abcdef"));
        assert!(!is_pack_id("../../etc/passwd0123456789abcdef"));
        assert!(!is_pack_id("abc"));
    }
}
