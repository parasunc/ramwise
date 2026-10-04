//! Memory-type visualization and accounting education.
//!
//! Two rules keep every visualization honest:
//!
//! 1. **Unavailable is omitted, never zeroed.** A metric that was not
//!    collected (no smaps, no swap configured) produces no segment and no
//!    line — a zero would read as "measured nothing" instead of "unknown".
//! 2. **Cache is not pressure.** Reclaimable page cache counts as available
//!    memory; the notes say so explicitly wherever cache is shown.

use crate::collector::{ProcessMemory, SystemMemory};

/// One segment of a stacked composition bar.
#[derive(Debug, Clone, PartialEq)]
pub struct CompositionSegment {
    pub label: &'static str,
    pub bytes: u64,
    /// Fraction of the process RSS, for bar widths.
    pub fraction: f64,
}

/// Stacked composition of one process: private and shared memory as
/// fractions of RSS. Swap is deliberately excluded — VmSwap is not resident
/// and stacking it with RSS fractions sums past 100%. Segments with no data
/// are omitted (never zeroed); an empty RSS yields no segments at all.
///
/// Without smaps detail (`pss == 0 && uss == 0`) the split is unavailable:
/// a single `rss` segment says so instead of guessing a breakdown.
pub fn composition_segments(process: &ProcessMemory) -> Vec<CompositionSegment> {
    if process.rss == 0 {
        return Vec::new();
    }
    if process.pss == 0 && process.uss == 0 {
        return vec![CompositionSegment {
            label: "rss",
            bytes: process.rss,
            fraction: 1.0,
        }];
    }
    let mut segments = Vec::new();
    let mut accounted = 0u64;
    for (label, bytes) in [("private", process.private), ("shared", process.shared)] {
        if bytes == 0 {
            continue;
        }
        accounted = accounted.saturating_add(bytes);
        // Clamp: inconsistent inputs must not push the total past 1.
        let fraction = (bytes as f64 / process.rss as f64).min(1.0);
        segments.push(CompositionSegment {
            label,
            bytes,
            fraction,
        });
    }
    let remainder = process.rss.saturating_sub(accounted);
    if remainder > 0 {
        segments.push(CompositionSegment {
            label: "other",
            bytes: remainder,
            fraction: (remainder as f64 / process.rss as f64).min(1.0),
        });
    }
    segments
}

/// One accounting note: a labeled, caveated explanation of a system metric.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountingNote {
    pub label: &'static str,
    pub text: String,
}

/// System accounting notes with explicit caveats. Metrics that carry no
/// information on this machine (no swap configured, no cache reported)
/// produce explanatory notes or nothing — never a zero masquerading as data.
pub fn system_notes(system: &SystemMemory) -> Vec<AccountingNote> {
    use crate::utils::format_bytes;
    let mut notes = Vec::new();
    if system.total > 0 {
        notes.push(AccountingNote {
            label: "available",
            text: format!(
                "{} free for new applications (reclaimable cache included)",
                format_bytes(system.available)
            ),
        });
    }
    if system.cached > 0 {
        notes.push(AccountingNote {
            label: "page cache",
            text: format!(
                "{} of file data; reclaimed under pressure, not a leak",
                format_bytes(system.cached)
            ),
        });
    }
    if system.slab > 0 {
        notes.push(AccountingNote {
            label: "slab",
            text: format!(
                "{} in kernel caches; largely unreclaimable",
                format_bytes(system.slab)
            ),
        });
    }
    if system.shared > 0 {
        notes.push(AccountingNote {
            label: "shmem",
            text: format!(
                "{} in tmpfs/shared memory; counted in processes too",
                format_bytes(system.shared)
            ),
        });
    }
    if system.swap_total > 0 {
        notes.push(AccountingNote {
            label: "swap",
            text: format!(
                "{} of {} used; swapping slows the system before it saves it",
                format_bytes(system.swap_used),
                format_bytes(system.swap_total)
            ),
        });
    } else if system.total > 0 {
        notes.push(AccountingNote {
            label: "swap",
            text: "not configured on this machine (not zero usage)".to_string(),
        });
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;

    #[test]
    fn composition_covers_rss_without_zero_segments() {
        let process = test_support::process(1000);
        // Fixture: shared 250, private 750, swap 0 (swap never stacks).
        let segments = composition_segments(&process);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].label, "private");
        assert_eq!(segments[0].fraction, 0.75);
        assert_eq!(segments[1].label, "shared");
        let total: f64 = segments.iter().map(|segment| segment.fraction).sum();
        assert!((total - 1.0).abs() < 1e-9);
    }

    #[test]
    fn swap_is_not_stacked_into_rss() {
        let mut process = test_support::process(1000);
        process.swap = 500;
        let segments = composition_segments(&process);
        assert!(segments.iter().all(|segment| segment.label != "swap"));
        let total: f64 = segments.iter().map(|segment| segment.fraction).sum();
        assert!(total <= 1.0 + 1e-9);
    }

    #[test]
    fn missing_smaps_yields_a_single_rss_segment() {
        let mut process = test_support::process(1000);
        process.pss = 0;
        process.uss = 0;
        let segments = composition_segments(&process);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].label, "rss");
        assert_eq!(segments[0].fraction, 1.0);
    }

    #[test]
    fn empty_rss_has_no_composition() {
        let mut process = test_support::process(0);
        process.private = 0;
        process.shared = 0;
        assert!(composition_segments(&process).is_empty());
    }

    #[test]
    fn notes_omit_unavailable_metrics() {
        let notes = system_notes(&SystemMemory::default());
        assert!(notes.is_empty());
    }

    #[test]
    fn swap_less_machines_say_so_explicitly() {
        let mut system = test_support::system_memory();
        system.swap_total = 0;
        system.swap_used = 0;
        let notes = system_notes(&system);
        let swap = notes.iter().find(|note| note.label == "swap").unwrap();
        assert!(swap.text.contains("not configured"));
        assert!(!swap.text.contains("0B"));
    }

    #[test]
    fn notes_explain_cache_and_slab_caveats() {
        let mut system = test_support::system_memory();
        system.slab = 512 * 1024 * 1024;
        let notes = system_notes(&system);
        let labels: Vec<&str> = notes.iter().map(|note| note.label).collect();
        assert!(labels.contains(&"available"));
        assert!(labels.contains(&"page cache"));
        assert!(labels.contains(&"slab"));
        let cache = notes
            .iter()
            .find(|note| note.label == "page cache")
            .unwrap();
        assert!(cache.text.contains("reclaimed"));
    }
}
