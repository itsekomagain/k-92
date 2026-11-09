use std::collections::{BTreeSet, HashMap};

use k92_core::dto::VerifyReport;
use k92_core::{CancelToken, ObjectHash, Result};

use crate::Repository;

/// Checks every manifest and every referenced object.
/// Quick mode checks presence and size; deep mode re-hashes every object.
pub fn verify(repo: &Repository, deep: bool, cancel: &CancelToken) -> Result<VerifyReport> {
    let (snaps, damaged) = repo.load_all()?;
    let mut report = VerifyReport {
        snapshots_checked: (snaps.len() + damaged.len()) as u64,
        damaged_snapshots: damaged,
        deep,
        ..Default::default()
    };

    // hash -> (size, snapshots that reference it)
    let mut objects: HashMap<ObjectHash, (u64, Vec<usize>)> = HashMap::new();
    for (i, s) in snaps.iter().enumerate() {
        for e in &s.entries {
            if let Some(h) = e.hash {
                let slot = objects.entry(h).or_insert((e.size, Vec::new()));
                if slot.1.last() != Some(&i) {
                    slot.1.push(i);
                }
            }
        }
    }

    let mut affected = BTreeSet::new();
    let mut sorted: Vec<_> = objects.into_iter().collect();
    sorted.sort_by_key(|(h, _)| *h);
    for (hash, (size, users)) in sorted {
        cancel.check()?;
        report.objects_checked += 1;
        let path = repo.object_path(&hash);
        let problem = match std::fs::metadata(&path) {
            Err(_) => Some(&mut report.missing_objects),
            Ok(m) if m.len() != size => Some(&mut report.corrupt_objects),
            Ok(_) if deep && !repo.check_object(&hash)? => Some(&mut report.corrupt_objects),
            Ok(_) => None,
        };
        if let Some(list) = problem {
            list.push(hash.to_hex());
            for i in users {
                affected.insert(snaps[i].id.to_string());
            }
        }
    }
    report.affected_snapshots = affected.into_iter().collect();
    Ok(report)
}
