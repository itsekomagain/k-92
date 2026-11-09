//! Retention planning. Pure logic: decides which snapshots to delete, never deletes itself.

use chrono::{DateTime, Datelike, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::SnapshotId;

/// Time Machine-style thinning: everything for `keep_all_hours`, then one per day for
/// `daily_days`, then one per ISO week. The newest snapshot is always kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionPolicy {
    pub keep_all_hours: i64,
    pub daily_days: i64,
    /// `None` keeps weekly snapshots forever.
    pub weekly_weeks: Option<i64>,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        RetentionPolicy {
            keep_all_hours: 24,
            daily_days: 30,
            weekly_weeks: None,
        }
    }
}

/// Returns the ids to delete. Snapshots with unparseable timestamps are always kept.
pub fn plan(ids: &[SnapshotId], now: DateTime<Utc>, policy: &RetentionPolicy) -> Vec<SnapshotId> {
    let mut sorted: Vec<&SnapshotId> = ids.iter().collect();
    sorted.sort();
    sorted.reverse(); // newest first: the first one seen in each bucket is kept

    let mut seen_days = std::collections::HashSet::new();
    let mut seen_weeks = std::collections::HashSet::new();
    let mut delete = Vec::new();
    for (i, id) in sorted.into_iter().enumerate() {
        let Some(t) = id.timestamp() else { continue };
        if i == 0 {
            continue; // newest is sacred
        }
        let age = now - t;
        let keep = if age < Duration::hours(policy.keep_all_hours) {
            true
        } else if age < Duration::days(policy.daily_days) {
            seen_days.insert(t.date_naive())
        } else if policy.weekly_weeks.is_none_or(|w| age < Duration::weeks(w)) {
            let w = t.iso_week();
            seen_weeks.insert((w.year(), w.week()))
        } else {
            false
        };
        if !keep {
            delete.push(id.clone());
        }
    }
    delete
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id_at(t: DateTime<Utc>) -> SnapshotId {
        SnapshotId::generate(t, None)
    }

    #[test]
    fn keeps_newest_and_recent_and_thins_old() {
        let now = Utc::now();
        let mut ids = Vec::new();
        // hourly for 10 days
        for h in 0..240 {
            ids.push(id_at(now - Duration::hours(h)));
        }
        let del = plan(&ids, now, &RetentionPolicy::default());
        let kept: Vec<_> = ids.iter().filter(|i| !del.contains(i)).collect();
        // all of last 24h kept
        for h in 0..24 {
            let t = now - Duration::hours(h);
            assert!(
                kept.iter()
                    .any(|k| k.timestamp().unwrap() == id_at(t).timestamp().unwrap())
            );
        }
        // older than 24h: at most one per calendar day
        let mut days = std::collections::HashMap::new();
        for k in &kept {
            let t = k.timestamp().unwrap();
            if now - t >= Duration::hours(24) {
                *days.entry(t.date_naive()).or_insert(0) += 1;
            }
        }
        assert!(days.values().all(|&c| c == 1), "{days:?}");
        assert!(!del.is_empty());
    }

    #[test]
    fn never_deletes_the_only_or_newest_snapshot() {
        let now = Utc::now();
        let old = id_at(now - Duration::days(4000));
        assert!(plan(std::slice::from_ref(&old), now, &RetentionPolicy::default()).is_empty());
        let policy = RetentionPolicy {
            keep_all_hours: 0,
            daily_days: 0,
            weekly_weeks: Some(0),
        };
        let newer = id_at(now - Duration::days(3000));
        let del = plan(&[old.clone(), newer.clone()], now, &policy);
        assert_eq!(del, vec![old]);
    }
}
