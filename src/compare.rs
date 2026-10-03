//! Compatible snapshot comparison and diff output.
//!
//! Identity is `(pid, start_time_ticks)`: a PID seen on both sides with a
//! different start time is reported as one removal plus one addition, so PID
//! reuse can never silently match. When both sides report start time zero
//! (unknown), matching falls back to PID only and records an explicit
//! warning stating that limitation. Snapshots with different schema versions
//! are rejected before any comparison.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::collector::{ExportProcessMemory, ExportSnapshot};

/// Comparison failures are explicit: callers (and exit codes) distinguish
/// an unreadable input from an incompatible contract.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CompareError {
    #[error("incompatible snapshot schema versions {from} and {to}")]
    SchemaMismatch { from: u32, to: u32 },
}

/// Thresholds shaping a comparison.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompareOptions {
    /// Only report changes with at least this much absolute RSS movement.
    pub min_delta_bytes: u64,
}

/// Stable identity of a process present on exactly one side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessRef {
    pub pid: i32,
    pub start_time_ticks: u64,
    pub name: String,
}

/// Per-process movement between two snapshots. Deltas are signed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessDelta {
    pub pid: i32,
    pub name: String,
    pub rss_delta_bytes: i64,
    pub pss_delta_bytes: i64,
    pub private_delta_bytes: i64,
    pub swap_delta_bytes: i64,
}

/// System-wide movement between two snapshots. Deltas are signed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemDelta {
    pub total_delta_bytes: i64,
    pub available_delta_bytes: i64,
    pub swap_used_delta_bytes: i64,
}

/// The full comparison result. Added/removed list by PID ascending;
/// `changed` orders by absolute RSS movement descending.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotDiff {
    pub schema_version: u32,
    pub from_captured_at_unix_ms: u64,
    pub to_captured_at_unix_ms: u64,
    pub system: SystemDelta,
    pub added: Vec<ProcessRef>,
    pub removed: Vec<ProcessRef>,
    pub changed: Vec<ProcessDelta>,
    pub warnings: Vec<String>,
}

fn process_ref(process: &ExportProcessMemory) -> ProcessRef {
    ProcessRef {
        pid: process.pid,
        start_time_ticks: process.start_time_ticks,
        name: process.name.clone(),
    }
}

/// Compare two snapshots. Fails on schema mismatch before touching data.
pub fn compare_snapshots(
    old: &ExportSnapshot,
    new: &ExportSnapshot,
    options: CompareOptions,
) -> std::result::Result<SnapshotDiff, CompareError> {
    if old.schema_version != new.schema_version {
        return Err(CompareError::SchemaMismatch {
            from: old.schema_version,
            to: new.schema_version,
        });
    }

    let mut warnings = Vec::new();
    let (old_index, old_duplicates) = index_processes(&old.processes);
    let (new_index, new_duplicates) = index_processes(&new.processes);
    for duplicate in old_duplicates.iter().chain(&new_duplicates) {
        warnings.push(format!(
            "duplicate identity (pid {}, start {}) kept first occurrence",
            duplicate.0, duplicate.1
        ));
    }
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();

    let old_keys: BTreeSet<(i32, u64)> = old_index.keys().copied().collect();
    let new_keys: BTreeSet<(i32, u64)> = new_index.keys().copied().collect();

    for key in old_keys.difference(&new_keys) {
        let old_process = &old_index[key];
        // Same PID with a different KNOWN start time on the new side means
        // the PID was reused: removal plus addition, never a silent match.
        // When either side is zero (unknown, e.g. pre-start-time files),
        // identity falls back to PID-only with an explicit warning instead.
        let counterpart = new.processes.iter().find(|p| p.pid == key.0);
        match counterpart {
            Some(other)
                if key.1 != 0 && other.start_time_ticks != 0 && other.start_time_ticks != key.1 =>
            {
                warnings.push(format!(
                    "pid {} reused (start time changed); reported as removed plus added",
                    key.0
                ));
                removed.push(process_ref(old_process));
            }
            Some(_) if key.1 == 0 || counterpart.map(|p| p.start_time_ticks) == Some(0) => {
                warnings.push(format!(
                    "pid {} matched by PID only (start time unknown on a side)",
                    key.0
                ));
                let other = counterpart.expect("PID-only fallback found its counterpart");
                let delta = ProcessDelta {
                    pid: key.0,
                    name: other.name.clone(),
                    rss_delta_bytes: delta_bytes(other.rss_bytes, old_process.rss_bytes),
                    pss_delta_bytes: delta_bytes(other.pss_bytes, old_process.pss_bytes),
                    private_delta_bytes: delta_bytes(
                        other.private_bytes,
                        old_process.private_bytes,
                    ),
                    swap_delta_bytes: delta_bytes(other.swap_bytes, old_process.swap_bytes),
                };
                if delta.rss_delta_bytes != 0
                    && delta.rss_delta_bytes.unsigned_abs() >= options.min_delta_bytes
                {
                    changed.push(delta);
                }
            }
            _ => {
                removed.push(process_ref(old_process));
            }
        }
    }
    for key in new_keys.difference(&old_keys) {
        // The removal side above already records reuse pairs, but the
        // addition side is still needed for a complete picture. Pairs
        // already emitted as PID-only fallback matches are skipped here.
        let already_matched = old.processes.iter().any(|p| {
            p.pid == key.0 && (p.start_time_ticks == 0 || key.1 == 0) && !old_keys.contains(key)
        });
        if !already_matched {
            added.push(process_ref(new_index[key]));
        }
    }
    for key in old_keys.intersection(&new_keys) {
        let old_process = &old_index[key];
        let new_process = &new_index[key];
        if key.1 == 0 {
            warnings.push(format!(
                "pid {} matched by PID only (start time unknown on both sides)",
                key.0
            ));
        }
        let delta = ProcessDelta {
            pid: key.0,
            name: new_process.name.clone(),
            rss_delta_bytes: delta_bytes(new_process.rss_bytes, old_process.rss_bytes),
            pss_delta_bytes: delta_bytes(new_process.pss_bytes, old_process.pss_bytes),
            private_delta_bytes: delta_bytes(new_process.private_bytes, old_process.private_bytes),
            swap_delta_bytes: delta_bytes(new_process.swap_bytes, old_process.swap_bytes),
        };
        if delta.rss_delta_bytes != 0
            && delta.rss_delta_bytes.unsigned_abs() >= options.min_delta_bytes
        {
            changed.push(delta);
        }
    }

    added.sort_by_key(|entry: &ProcessRef| entry.pid);
    removed.sort_by_key(|entry: &ProcessRef| entry.pid);
    changed.sort_by_key(|delta: &ProcessDelta| {
        std::cmp::Reverse(delta.rss_delta_bytes.unsigned_abs())
    });
    warnings.sort();
    warnings.dedup();

    Ok(SnapshotDiff {
        schema_version: new.schema_version,
        from_captured_at_unix_ms: old.captured_at_unix_ms,
        to_captured_at_unix_ms: new.captured_at_unix_ms,
        system: SystemDelta {
            total_delta_bytes: delta_bytes(new.system.total_bytes, old.system.total_bytes),
            available_delta_bytes: delta_bytes(
                new.system.available_bytes,
                old.system.available_bytes,
            ),
            swap_used_delta_bytes: delta_bytes(
                new.system.swap_used_bytes,
                old.system.swap_used_bytes,
            ),
        },
        added,
        removed,
        changed,
        warnings,
    })
}

/// Signed byte delta computed via i128 so large counters can never wrap.
fn delta_bytes(new: u64, old: u64) -> i64 {
    (new as i128 - old as i128).clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

type ProcessIndex<'a> = BTreeMap<(i32, u64), &'a ExportProcessMemory>;

/// Index by identity, reporting duplicate keys. First occurrence wins;
/// callers surface the duplicates as warnings.
fn index_processes(processes: &[ExportProcessMemory]) -> (ProcessIndex<'_>, Vec<(i32, u64)>) {
    use std::collections::btree_map::Entry;
    let mut index = ProcessIndex::new();
    let mut duplicates = Vec::new();
    for process in processes {
        let key = (process.pid, process.start_time_ticks);
        match index.entry(key) {
            Entry::Occupied(_) => duplicates.push(key),
            Entry::Vacant(slot) => {
                slot.insert(process);
            }
        }
    }
    (index, duplicates)
}

/// Load one exported snapshot file, validating its schema version.
pub fn load_snapshot(path: &Path) -> Result<ExportSnapshot> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let snapshot: ExportSnapshot = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {} as a snapshot", path.display()))?;
    snapshot
        .validate()
        .map_err(|message| anyhow::anyhow!("{}: {message}", path.display()))?;
    Ok(snapshot)
}

/// Human-readable diff: system movement, then added/removed/changed with
/// the largest movers first, then fallback warnings.
pub fn render_human(diff: &SnapshotDiff) -> String {
    let mut out = String::new();
    let system = &diff.system;
    writeln!(
        out,
        "system total {:+} available {:+} swap {:+}",
        format_bytes_signed(system.total_delta_bytes),
        format_bytes_signed(system.available_delta_bytes),
        format_bytes_signed(system.swap_used_delta_bytes),
    )
    .unwrap();
    if diff.added.is_empty() && diff.removed.is_empty() && diff.changed.is_empty() {
        out.push_str("no changes\n");
    }
    for entry in &diff.added {
        writeln!(out, "+ pid {} {} (new)", entry.pid, safe_name(&entry.name)).unwrap();
    }
    for entry in &diff.removed {
        writeln!(out, "- pid {} {} (gone)", entry.pid, safe_name(&entry.name)).unwrap();
    }
    const MAX_CHANGED_LINES: usize = 10;
    for delta in diff.changed.iter().take(MAX_CHANGED_LINES) {
        writeln!(
            out,
            "~ pid {} {} rss {:+} pss {:+}",
            delta.pid,
            safe_name(&delta.name),
            format_bytes_signed(delta.rss_delta_bytes),
            format_bytes_signed(delta.pss_delta_bytes),
        )
        .unwrap();
    }
    if diff.changed.len() > MAX_CHANGED_LINES {
        writeln!(out, "… and {} more", diff.changed.len() - MAX_CHANGED_LINES).unwrap();
    }
    for warning in &diff.warnings {
        writeln!(out, "! {warning}").unwrap();
    }
    out
}

fn format_bytes_signed(delta: i64) -> String {
    use crate::utils::format_bytes;
    if delta < 0 {
        format!("-{}", format_bytes(delta.unsigned_abs()))
    } else {
        format!("+{}", format_bytes(delta as u64))
    }
}

/// JSON diff: the [`SnapshotDiff`] contract verbatim.
pub fn render_json(diff: &SnapshotDiff) -> Result<String> {
    serde_json::to_string(diff).context("Failed to serialize diff to JSON")
}

/// CSV diff: the changed-process table only (stable header plus a metadata
/// comment). Added, removed and system movement live in the human and JSON
/// renders; CSV stays a machine-readable mover list by design.
pub fn render_csv(diff: &SnapshotDiff) -> String {
    let mut out = String::new();
    writeln!(out, "# ramwise-diff schema_version={}", diff.schema_version).unwrap();
    writeln!(
        out,
        "pid,name,rss_delta_bytes,pss_delta_bytes,private_delta_bytes,swap_delta_bytes"
    )
    .unwrap();
    for delta in &diff.changed {
        writeln!(
            out,
            "{},{},{},{},{},{}",
            delta.pid,
            csv_field(&delta.name),
            delta.rss_delta_bytes,
            delta.pss_delta_bytes,
            delta.private_delta_bytes,
            delta.swap_delta_bytes
        )
        .unwrap();
    }
    out
}

fn csv_field(field: &str) -> String {
    if field.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

/// Names render inside line-oriented output, so control characters that
/// could forge lines are replaced before printing.
fn safe_name(name: &str) -> String {
    name.chars()
        .map(|cell| if cell.is_control() { '?' } else { cell })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: i32, start: u64, rss: u64) -> ExportProcessMemory {
        ExportProcessMemory {
            pid,
            start_time_ticks: start,
            name: format!("p{pid}"),
            rss_bytes: rss,
            pss_bytes: rss / 2,
            ..Default::default()
        }
    }

    fn snapshots(
        old_processes: Vec<ExportProcessMemory>,
        new_processes: Vec<ExportProcessMemory>,
    ) -> (ExportSnapshot, ExportSnapshot) {
        let mut old = ExportSnapshot::fixture();
        old.processes = old_processes;
        let mut new = ExportSnapshot::fixture();
        new.processes = new_processes;
        (old, new)
    }

    #[test]
    fn identical_snapshots_have_no_changes() {
        let (old, _) = snapshots(vec![process(1, 10, 100)], vec![process(1, 10, 100)]);
        let diff = compare_snapshots(&old, &old.clone(), CompareOptions::default()).unwrap();
        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
        assert!(diff.changed.is_empty());
        assert!(diff.warnings.is_empty());
        assert_eq!(diff.system.total_delta_bytes, 0);
    }

    #[test]
    fn schema_mismatch_fails_before_comparison() {
        let (mut old, new) = snapshots(vec![], vec![]);
        old.schema_version += 1;
        assert_eq!(
            compare_snapshots(&old, &new, CompareOptions::default()),
            Err(CompareError::SchemaMismatch {
                from: old.schema_version,
                to: new.schema_version
            })
        );
    }

    #[test]
    fn added_removed_and_changed_are_reported() {
        let (old, new) = snapshots(
            vec![
                process(1, 10, 100),
                process(2, 20, 200),
                process(3, 30, 300),
            ],
            vec![process(1, 10, 500), process(3, 30, 300), process(4, 40, 50)],
        );
        let diff = compare_snapshots(&old, &new, CompareOptions::default()).unwrap();
        assert_eq!(
            diff.added.iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![4]
        );
        assert_eq!(
            diff.removed.iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![2]
        );
        assert_eq!(diff.changed.len(), 1);
        assert_eq!(diff.changed[0].pid, 1);
        assert_eq!(diff.changed[0].rss_delta_bytes, 400);
    }

    #[test]
    fn pid_reuse_is_removed_plus_added_never_a_match() {
        let (old, new) = snapshots(vec![process(7, 10, 100)], vec![process(7, 99, 100)]);
        let diff = compare_snapshots(&old, &new, CompareOptions::default()).unwrap();
        assert!(diff.changed.is_empty());
        assert_eq!(
            diff.removed.iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![7]
        );
        assert_eq!(
            diff.added.iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![7]
        );
        assert!(diff.warnings.iter().any(|w| w.contains("reused")));
    }

    #[test]
    fn unknown_start_on_one_side_falls_back_to_pid() {
        // Old file predates start times: same PID matches by PID with a
        // warning instead of reporting remove-plus-add.
        let (old, new) = snapshots(vec![process(7, 0, 100)], vec![process(7, 99, 150)]);
        let diff = compare_snapshots(&old, &new, CompareOptions::default()).unwrap();
        assert!(diff.removed.is_empty());
        assert!(diff.added.is_empty());
        assert_eq!(diff.changed.len(), 1);
        assert_eq!(diff.changed[0].rss_delta_bytes, 50);
        assert!(diff.warnings.iter().any(|w| w.contains("PID only")));
    }

    #[test]
    fn duplicate_identities_warn_and_keep_first() {
        let (old, new) = snapshots(
            vec![process(7, 10, 100), process(7, 10, 999)],
            vec![process(7, 10, 100)],
        );
        let diff = compare_snapshots(&old, &new, CompareOptions::default()).unwrap();
        assert!(diff.warnings.iter().any(|w| w.contains("duplicate")));
        assert!(diff.changed.is_empty());
    }

    #[test]
    fn negative_deltas_order_by_absolute_movement() {
        let (old, new) = snapshots(
            vec![process(1, 10, 1000), process(2, 20, 1000)],
            vec![process(1, 10, 900), process(2, 20, 500)],
        );
        let diff = compare_snapshots(&old, &new, CompareOptions::default()).unwrap();
        assert_eq!(diff.changed.len(), 2);
        assert_eq!(diff.changed[0].pid, 2);
        assert_eq!(diff.changed[0].rss_delta_bytes, -500);
        assert_eq!(diff.changed[1].rss_delta_bytes, -100);
    }

    #[test]
    fn csv_quotes_names_with_commas() {
        let (old, new) = snapshots(
            vec![process(1, 10, 100)],
            vec![ExportProcessMemory {
                name: "a,b".into(),
                rss_bytes: 500,
                ..process(1, 10, 100)
            }],
        );
        let diff = compare_snapshots(&old, &new, CompareOptions::default()).unwrap();
        assert!(render_csv(&diff).contains("\"a,b\""));
        let text = render_human(&diff);
        assert!(text.contains("a,b"));
    }

    #[test]
    fn load_snapshot_reports_paths_on_failure() {
        let missing = Path::new("/nonexistent-ramwise-fixture/snap.json");
        let error = load_snapshot(missing).unwrap_err().to_string();
        assert!(error.contains("snap.json"));
    }

    #[test]
    fn min_delta_threshold_filters_small_movers() {
        let (old, new) = snapshots(
            vec![process(1, 10, 100), process(2, 20, 1000)],
            vec![process(1, 10, 150), process(2, 20, 5000)],
        );
        let options = CompareOptions {
            min_delta_bytes: 1000,
        };
        let diff = compare_snapshots(&old, &new, options).unwrap();
        assert_eq!(
            diff.changed.iter().map(|d| d.pid).collect::<Vec<_>>(),
            vec![2]
        );
    }

    #[test]
    fn human_output_covers_every_section() {
        let (old, new) = snapshots(vec![process(7, 10, 100)], vec![process(7, 99, 100)]);
        let diff = compare_snapshots(&old, &new, CompareOptions::default()).unwrap();
        let text = render_human(&diff);
        assert!(text.contains("system total"));
        assert!(text.contains("(gone)"));
        assert!(text.contains("(new)"));
        assert!(text.contains("reused"));
    }

    #[test]
    fn json_diff_round_trips_and_csv_header_is_stable() {
        let (old, new) = snapshots(vec![process(1, 10, 100)], vec![process(1, 10, 500)]);
        let diff = compare_snapshots(&old, &new, CompareOptions::default()).unwrap();
        let decoded: SnapshotDiff = serde_json::from_str(&render_json(&diff).unwrap()).unwrap();
        assert_eq!(decoded, diff);
        let csv = render_csv(&diff);
        let mut lines = csv.lines();
        assert!(
            lines
                .next()
                .unwrap()
                .starts_with("# ramwise-diff schema_version=")
        );
        assert_eq!(
            lines.next().unwrap(),
            "pid,name,rss_delta_bytes,pss_delta_bytes,private_delta_bytes,swap_delta_bytes"
        );
        assert!(csv.contains(",400,"));
    }
}
