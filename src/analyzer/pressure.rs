//! Documented memory-pressure classification.
//!
//! The classifier turns available RAM plus swap usage/activity into one of
//! four levels:
//!
//! - **Unknown** — no usable input (`total == 0`); rendered dim, never as zero.
//! - **Stable (green)** — comfortably below every threshold.
//! - **Elevated (yellow)** — worth watching; sustained pressure building.
//! - **Critical (red)** — act now; OOM risk or heavy swapping.
//!
//! Thresholds are data, not hard-coded branches: [`PressureThresholds`]
//! carries the documented defaults and any caller (tests, future config
//! plumbing, the TUI) can supply its own. Comparisons are `>=` so exact
//! boundary values classify deterministically upward.

use crate::collector::SystemMemory;

/// Classified memory-pressure level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressureLevel {
    Unknown,
    Stable,
    Elevated,
    Critical,
}

impl PressureLevel {
    /// Short stable label for status lines and tests.
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Stable => "stable",
            Self::Elevated => "elevated",
            Self::Critical => "critical",
        }
    }
}

/// Tunable classification thresholds.
///
/// Defaults are starting points chosen for desktop/server interchangeability,
/// not absolutes: swap-rate thresholds in particular depend on device speed,
/// so they are deliberately conservative (any sustained swap-out activity
/// elevates; triple-digit pages/s is critical).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PressureThresholds {
    /// `used/total` percent at or above which pressure is elevated.
    pub yellow_used_pct: f64,
    /// `used/total` percent at or above which pressure is critical.
    pub red_used_pct: f64,
    /// Swap-fill percent at or above which pressure is elevated.
    pub yellow_swap_pct: f64,
    /// Swap-fill percent at or above which pressure is critical.
    pub red_swap_pct: f64,
    /// Sustained swap-out pages/s at or above which pressure is elevated.
    pub yellow_swap_rate: f64,
    /// Sustained swap-out pages/s at or above which pressure is critical.
    pub red_swap_rate: f64,
}

impl Default for PressureThresholds {
    fn default() -> Self {
        Self {
            yellow_used_pct: 75.0,
            red_used_pct: 90.0,
            yellow_swap_pct: 50.0,
            red_swap_pct: 80.0,
            yellow_swap_rate: 100.0,
            red_swap_rate: 1000.0,
        }
    }
}

/// Classify system memory pressure.
///
/// Swap conditions only apply when the machine actually has swap
/// (`swap_total > 0`); swap-less machines classify purely on RAM so a zero
/// swap total can never inflate or deflate the level — including through
/// swap-activity rates, which require real swap to be meaningful. Unknown
/// swap rates (`None`, e.g. the first sample) contribute nothing.
pub fn classify(system: &SystemMemory, thresholds: &PressureThresholds) -> PressureLevel {
    if system.total == 0 {
        return PressureLevel::Unknown;
    }
    let used_pct = system.usage_percent();
    let swap_pct = system.swap_percent();
    let swap_out_rate = if system.swap_total > 0 {
        system.swap_out_rate.unwrap_or(0.0)
    } else {
        0.0
    };

    let critical = used_pct >= thresholds.red_used_pct
        || (system.swap_total > 0 && swap_pct >= thresholds.red_swap_pct)
        || swap_out_rate >= thresholds.red_swap_rate;
    if critical {
        return PressureLevel::Critical;
    }

    let elevated = used_pct >= thresholds.yellow_used_pct
        || (system.swap_total > 0 && swap_pct >= thresholds.yellow_swap_pct)
        || swap_out_rate >= thresholds.yellow_swap_rate;
    if elevated {
        return PressureLevel::Elevated;
    }

    PressureLevel::Stable
}

/// Used-versus-available explanation for the header and insights panel.
/// States what is used, what remains for new applications, and why, so
/// `used` is never confused with `total - free` (reclaimable cache counts
/// as available, not as pressure).
pub fn explain(system: &SystemMemory, level: PressureLevel) -> String {
    use crate::utils::format_bytes;
    format!(
        "Memory pressure {}: using {} of {} ({} available for new applications)",
        level.label(),
        format_bytes(system.used()),
        format_bytes(system.total),
        format_bytes(system.available),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn system_with(used_pct: f64) -> SystemMemory {
        let total = 100 * GIB;
        let mut system = test_support::system_memory();
        system.total = total;
        system.available = ((100.0 - used_pct) * total as f64 / 100.0) as u64;
        system.swap_total = 0;
        system.swap_used = 0;
        system.swap_out_rate = None;
        system
    }

    #[test]
    fn ram_boundaries_classify_upward() {
        let thresholds = PressureThresholds::default();
        assert_eq!(
            classify(&system_with(0.0), &thresholds),
            PressureLevel::Stable
        );
        assert_eq!(
            classify(&system_with(74.9), &thresholds),
            PressureLevel::Stable
        );
        assert_eq!(
            classify(&system_with(75.0), &thresholds),
            PressureLevel::Elevated
        );
        assert_eq!(
            classify(&system_with(89.9), &thresholds),
            PressureLevel::Elevated
        );
        assert_eq!(
            classify(&system_with(90.0), &thresholds),
            PressureLevel::Critical
        );
    }

    #[test]
    fn zero_swap_machines_classify_on_ram_only() {
        let thresholds = PressureThresholds::default();
        let mut system = system_with(95.0);
        system.swap_total = 0;
        system.swap_used = 0;
        assert_eq!(classify(&system, &thresholds), PressureLevel::Critical);

        let calm = system_with(10.0);
        assert_eq!(classify(&calm, &thresholds), PressureLevel::Stable);
    }

    #[test]
    fn swap_fill_drives_the_level() {
        let thresholds = PressureThresholds::default();
        let mut system = system_with(10.0);
        system.swap_total = 4 * GIB;
        system.swap_used = 2 * GIB;
        assert_eq!(classify(&system, &thresholds), PressureLevel::Elevated);
        system.swap_used = 7 * GIB / 2;
        assert_eq!(classify(&system, &thresholds), PressureLevel::Critical);
    }

    #[test]
    fn swap_fill_boundaries_are_exact() {
        let thresholds = PressureThresholds::default();
        let mut system = system_with(10.0);
        system.swap_total = 100;
        system.swap_used = 49;
        assert_eq!(classify(&system, &thresholds), PressureLevel::Stable);
        system.swap_used = 50;
        assert_eq!(classify(&system, &thresholds), PressureLevel::Elevated);
        system.swap_used = 80;
        assert_eq!(classify(&system, &thresholds), PressureLevel::Critical);
    }

    #[test]
    fn swap_less_machines_ignore_swap_activity() {
        let thresholds = PressureThresholds::default();
        let mut system = system_with(10.0);
        system.swap_total = 0;
        system.swap_used = 0;
        system.swap_out_rate = Some(2500.0);
        assert_eq!(classify(&system, &thresholds), PressureLevel::Stable);
    }

    #[test]
    fn swap_activity_drives_the_level_without_swap_fill() {
        let thresholds = PressureThresholds::default();
        let mut system = system_with(10.0);
        system.swap_total = 4 * GIB;
        system.swap_used = 0;
        system.swap_out_rate = Some(250.0);
        assert_eq!(classify(&system, &thresholds), PressureLevel::Elevated);
        system.swap_out_rate = Some(2500.0);
        assert_eq!(classify(&system, &thresholds), PressureLevel::Critical);
        system.swap_out_rate = None;
        assert_eq!(classify(&system, &thresholds), PressureLevel::Stable);
    }

    #[test]
    fn missing_inputs_classify_unknown() {
        let system = SystemMemory::default();
        assert_eq!(
            classify(&system, &PressureThresholds::default()),
            PressureLevel::Unknown
        );
    }

    #[test]
    fn thresholds_are_configurable() {
        let strict = PressureThresholds {
            yellow_used_pct: 50.0,
            red_used_pct: 60.0,
            ..PressureThresholds::default()
        };
        assert_eq!(
            classify(&system_with(55.0), &strict),
            PressureLevel::Elevated
        );
        assert_eq!(
            classify(&system_with(55.0), &PressureThresholds::default()),
            PressureLevel::Stable
        );
    }

    #[test]
    fn explanation_states_used_versus_available() {
        let system = test_support::system_memory();
        let text = explain(&system, PressureLevel::Stable);
        assert!(text.contains("stable"));
        assert!(text.contains("available for new applications"));
    }
}
