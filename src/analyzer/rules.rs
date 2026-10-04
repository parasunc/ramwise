//! Rule definitions for memory analysis

#![allow(dead_code)]

use std::time::{Duration, Instant};

use crate::collector::MemorySnapshot;
use crate::history::HistoryBuffer;

use super::insights::{Insight, Severity};
use super::leak_score::{LeakBand, LeakScoreInput, leak_score};

/// Trait for analysis rules
pub trait Rule: Send + Sync {
    /// Unique name for this rule
    fn name(&self) -> &'static str;

    /// Evaluate the rule and return an insight if triggered
    fn evaluate(&self, snapshot: &MemorySnapshot, history: &HistoryBuffer) -> Option<Insight>;
}

/// Detect potential memory leaks based on consistent growth
pub struct MemoryLeakDetector {
    /// Minimum growth percentage to trigger
    pub threshold_percent: f64,
    /// Duration to analyze
    pub duration: Duration,
    /// Minimum RSS to consider (ignore small processes)
    pub min_rss: u64,
}

impl Default for MemoryLeakDetector {
    fn default() -> Self {
        Self {
            threshold_percent: 20.0,
            duration: Duration::from_secs(180), // 3 minutes
            min_rss: 50 * 1024 * 1024,          // 50 MB
        }
    }
}

impl Rule for MemoryLeakDetector {
    fn name(&self) -> &'static str {
        "memory_leak_detector"
    }

    fn evaluate(&self, snapshot: &MemorySnapshot, history: &HistoryBuffer) -> Option<Insight> {
        for proc in &snapshot.processes {
            if proc.rss < self.min_rss {
                continue;
            }

            if let Some(stats) = history.growth_stats(proc.pid, self.duration)
                && stats.percent_change >= self.threshold_percent
                && history.is_consistently_growing(proc.pid, self.threshold_percent / 2.0)
            {
                let rate_mb_per_min = (stats.rate_per_sec * 60.0) / (1024.0 * 1024.0);

                return Some(
                    Insight::new(
                        format!("leak_{}_{}", proc.pid, proc.name),
                        if stats.percent_change > 50.0 {
                            Severity::Critical
                        } else {
                            Severity::Warning
                        },
                        format!(
                            "RSS grew {:.1}% in {:.0}s",
                            stats.percent_change,
                            stats.duration.as_secs_f64()
                        ),
                        format!(
                            "Memory increased from {} to {} ({:+.1} MB/min)",
                            format_bytes(stats.start_value),
                            format_bytes(stats.end_value),
                            rate_mb_per_min
                        ),
                        "Possible memory leak. Consider restarting or investigating allocations."
                            .to_string(),
                    )
                    .with_process(proc.pid, proc.insight_name()),
                );
            }
        }
        None
    }
}

/// Detect processes using excessive memory
pub struct MemoryHogDetector {
    /// Percentage of total RAM to trigger
    pub threshold_percent: f64,
}

impl Default for MemoryHogDetector {
    fn default() -> Self {
        Self {
            threshold_percent: 30.0,
        }
    }
}

impl Rule for MemoryHogDetector {
    fn name(&self) -> &'static str {
        "memory_hog_detector"
    }

    fn evaluate(&self, snapshot: &MemorySnapshot, _history: &HistoryBuffer) -> Option<Insight> {
        let total = snapshot.system.total;
        if total == 0 {
            return None;
        }

        for proc in &snapshot.processes {
            let percent = (proc.rss as f64 / total as f64) * 100.0;
            if percent >= self.threshold_percent {
                return Some(
                    Insight::new(
                        format!("hog_{}_{}", proc.pid, proc.name),
                        Severity::Warning,
                        format!("Using {:.1}% of system RAM", percent),
                        format!(
                            "This process is using {} of {} total RAM",
                            format_bytes(proc.rss),
                            format_bytes(total)
                        ),
                        "Consider memory limits or alternative applications.".to_string(),
                    )
                    .with_process(proc.pid, proc.insight_name()),
                );
            }
        }
        None
    }
}

/// Detect sudden memory spikes
pub struct SuddenSpikeDetector {
    /// Minimum spike size in bytes
    pub min_spike_bytes: u64,
    /// Time window to check
    pub window: Duration,
}

impl Default for SuddenSpikeDetector {
    fn default() -> Self {
        Self {
            min_spike_bytes: 100 * 1024 * 1024, // 100 MB
            window: Duration::from_secs(10),
        }
    }
}

impl Rule for SuddenSpikeDetector {
    fn name(&self) -> &'static str {
        "sudden_spike_detector"
    }

    fn evaluate(&self, snapshot: &MemorySnapshot, history: &HistoryBuffer) -> Option<Insight> {
        for proc in &snapshot.processes {
            if let Some(stats) = history.growth_stats(proc.pid, self.window) {
                let growth = stats.end_value.saturating_sub(stats.start_value);
                if growth >= self.min_spike_bytes {
                    return Some(
                        Insight::new(
                            format!("spike_{}_{}", proc.pid, proc.name),
                            Severity::Warning,
                            format!(
                                "Sudden +{} in {}s",
                                format_bytes(growth),
                                self.window.as_secs()
                            ),
                            format!(
                                "Memory jumped from {} to {} very quickly",
                                format_bytes(stats.start_value),
                                format_bytes(stats.end_value)
                            ),
                            "Check recent activity in this process.".to_string(),
                        )
                        .with_process(proc.pid, proc.insight_name()),
                    );
                }
            }
        }
        None
    }
}

/// Detect OOM risk
pub struct OomRiskDetector {
    /// Available memory threshold percentage
    pub available_threshold: f64,
    /// Swap usage threshold percentage
    pub swap_threshold: f64,
}

impl Default for OomRiskDetector {
    fn default() -> Self {
        Self {
            available_threshold: 5.0,
            swap_threshold: 80.0,
        }
    }
}

impl Rule for OomRiskDetector {
    fn name(&self) -> &'static str {
        "oom_risk_detector"
    }

    fn evaluate(&self, snapshot: &MemorySnapshot, _history: &HistoryBuffer) -> Option<Insight> {
        let sys = &snapshot.system;
        let available_percent = if sys.total > 0 {
            (sys.available as f64 / sys.total as f64) * 100.0
        } else {
            100.0
        };

        let swap_percent = sys.swap_percent();

        if available_percent < self.available_threshold && swap_percent > self.swap_threshold {
            return Some(Insight::new(
                "oom_risk",
                Severity::Critical,
                format!(
                    "Low memory: {:.1}% available, {:.1}% swap used",
                    available_percent, swap_percent
                ),
                format!(
                    "Only {} available, {} swap used",
                    format_bytes(sys.available),
                    format_bytes(sys.swap_used)
                ),
                "System at risk of OOM. Close some applications immediately.".to_string(),
            ));
        }
        None
    }
}

/// Detect swap pressure
pub struct SwapPressureDetector {
    /// Swap usage threshold to trigger
    pub threshold_percent: f64,
}

impl Default for SwapPressureDetector {
    fn default() -> Self {
        Self {
            threshold_percent: 25.0,
        }
    }
}

impl Rule for SwapPressureDetector {
    fn name(&self) -> &'static str {
        "swap_pressure_detector"
    }

    fn evaluate(&self, snapshot: &MemorySnapshot, _history: &HistoryBuffer) -> Option<Insight> {
        let sys = &snapshot.system;
        let swap_percent = sys.swap_percent();

        if swap_percent >= self.threshold_percent {
            return Some(Insight::new(
                "swap_pressure",
                Severity::Warning,
                format!("Swap usage at {:.1}%", swap_percent),
                format!(
                    "Using {} of {} swap space",
                    format_bytes(sys.swap_used),
                    format_bytes(sys.swap_total)
                ),
                "System is swapping, which may cause slowdowns.".to_string(),
            ));
        }
        None
    }
}

/// Detect high fragmentation
pub struct FragmentationDetector {
    /// VSS/RSS ratio threshold
    pub ratio_threshold: f64,
    /// Minimum RSS to consider
    pub min_rss: u64,
}

impl Default for FragmentationDetector {
    fn default() -> Self {
        Self {
            ratio_threshold: 10.0,
            min_rss: 100 * 1024 * 1024, // 100 MB
        }
    }
}

impl Rule for FragmentationDetector {
    fn name(&self) -> &'static str {
        "fragmentation_detector"
    }

    fn evaluate(&self, snapshot: &MemorySnapshot, _history: &HistoryBuffer) -> Option<Insight> {
        for proc in &snapshot.processes {
            if proc.rss < self.min_rss {
                continue;
            }

            let ratio = proc.fragmentation_ratio();
            if ratio >= self.ratio_threshold {
                return Some(
                    Insight::new(
                        format!("frag_{}_{}", proc.pid, proc.name),
                        Severity::Info,
                        format!("High VSS/RSS ratio ({:.1}:1)", ratio),
                        format!(
                            "Virtual size {} vs actual {} ({:.1}x)",
                            format_bytes(proc.vss),
                            format_bytes(proc.rss),
                            ratio
                        ),
                        "Process has fragmented virtual address space.".to_string(),
                    )
                    .with_process(proc.pid, proc.insight_name()),
                );
            }
        }
        None
    }
}

/// Graded leak assessment complementing the binary [`MemoryLeakDetector`].
///
/// Where the binary detector fires on a growth threshold, this rule reports
/// the calibrated 0–100 score with its components, so borderline cases show
/// *why* they are (or are not) convincing. Only scores at or above
/// `min_score` produce insights. The 10 MB floor is deliberately more
/// sensitive than the binary detector's 50 MB: grading weak evidence is
/// safe, paging on it is not.
///
/// Both detectors may fire on a strong leak with distinct insight IDs
/// (`leak_` vs `leak_score_`); the score explains the binary trip.
pub struct LeakScoreRule {
    /// Minimum score to report.
    pub min_score: u8,
    /// History window to score over.
    pub window: Duration,
    /// Minimum peak RSS to consider (ignore small processes).
    pub min_size_bytes: u64,
}

impl Default for LeakScoreRule {
    fn default() -> Self {
        Self {
            min_score: 50,
            window: Duration::from_secs(300),
            min_size_bytes: 10 * 1024 * 1024,
        }
    }
}

impl Rule for LeakScoreRule {
    fn name(&self) -> &'static str {
        "leak_score_rule"
    }

    fn evaluate(&self, snapshot: &MemorySnapshot, history: &HistoryBuffer) -> Option<Insight> {
        for proc in &snapshot.processes {
            if proc.rss < self.min_size_bytes {
                continue;
            }
            let trend = history.process_trend(proc.pid);
            if trend.len() < 3 {
                continue;
            }
            // Score only the configured trailing window: ancient history
            // must not dilute (or rescue) recent behavior.
            let last = trend
                .last()
                .map(|(timestamp, _)| *timestamp)
                .unwrap_or_else(Instant::now);
            let cutoff = last.checked_sub(self.window).unwrap_or_else(|| trend[0].0);
            let windowed: Vec<(Instant, u64)> = trend
                .into_iter()
                .filter(|(timestamp, _)| *timestamp >= cutoff)
                .collect();
            if windowed.len() < 3 {
                continue;
            }
            let rss: Vec<u64> = windowed.iter().map(|(_, value)| *value).collect();
            let duration = windowed
                .last()
                .map(|(end, _)| end.saturating_duration_since(windowed[0].0))
                .unwrap_or(Duration::ZERO);
            let assessed = leak_score(LeakScoreInput {
                rss: &rss,
                duration,
                min_size_bytes: self.min_size_bytes,
            });
            if assessed.score >= self.min_score {
                return Some(
                    Insight::new(
                        format!("leak_score_{}_{}", proc.pid, proc.name),
                        if assessed.band == LeakBand::Severe {
                            Severity::Critical
                        } else {
                            Severity::Warning
                        },
                        format!(
                            "Leak score {}/100 ({})",
                            assessed.score,
                            assessed.band.label()
                        ),
                        format!(
                            "RSS {} → {} (growth {:.0}%, monotonicity {:.0}%, stability {:.0}%)",
                            format_bytes(rss[0]),
                            format_bytes(rss[rss.len() - 1]),
                            assessed.growth * 100.0,
                            assessed.monotonicity * 100.0,
                            assessed.stability * 100.0,
                        ),
                        "Sustained climb with low noise. Investigate allocations before it becomes critical."
                            .to_string(),
                    )
                    .with_process(proc.pid, proc.insight_name()),
                );
            }
        }
        None
    }
}

/// Informational insight about page cache
pub struct CacheInfoRule;

impl Rule for CacheInfoRule {
    fn name(&self) -> &'static str {
        "cache_info"
    }

    fn evaluate(&self, snapshot: &MemorySnapshot, _history: &HistoryBuffer) -> Option<Insight> {
        let sys = &snapshot.system;
        let cache_percent = if sys.total > 0 {
            (sys.cached as f64 / sys.total as f64) * 100.0
        } else {
            0.0
        };

        // Only show if cache is significant (>40%)
        if cache_percent > 40.0 {
            return Some(Insight::new(
                "cache_info",
                Severity::Info,
                format!("Page cache using {:.1}% of RAM", cache_percent),
                format!(
                    "Kernel is caching {} of file data",
                    format_bytes(sys.cached)
                ),
                "This is normal and will be reclaimed when needed.".to_string(),
            ));
        }
        None
    }
}

/// Helper to format bytes as human-readable
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1}G", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1}M", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1}K", bytes as f64 / KB as f64)
    } else {
        format!("{}B", bytes)
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::test_support;

    #[test]
    fn memory_hog_and_swap_rules_use_fixture_units() {
        let mut snapshot =
            test_support::snapshot_at(std::time::Instant::now(), 6 * 1024 * 1024 * 1024);
        snapshot.system.total = 16 * 1024 * 1024 * 1024;
        snapshot.system.swap_total = 4 * 1024 * 1024 * 1024;
        snapshot.system.swap_used = 2 * 1024 * 1024 * 1024;
        let history = HistoryBuffer::new(4, Duration::from_secs(60));

        let hog = MemoryHogDetector::default()
            .evaluate(&snapshot, &history)
            .unwrap();
        assert_eq!(hog.pid, Some(test_support::FIXTURE_PID));
        assert_eq!(hog.severity, Severity::Warning);
        assert_eq!(
            SwapPressureDetector::default()
                .evaluate(&snapshot, &history)
                .unwrap()
                .id,
            "swap_pressure"
        );
    }

    #[test]
    fn oom_rule_requires_both_available_memory_and_swap_pressure() {
        let mut snapshot = test_support::snapshot_at(std::time::Instant::now(), 1);
        snapshot.system.total = 100;
        snapshot.system.available = 4;
        snapshot.system.swap_total = 100;
        snapshot.system.swap_used = 90;
        let history = HistoryBuffer::new(1, Duration::from_secs(1));
        let insight = OomRiskDetector::default()
            .evaluate(&snapshot, &history)
            .unwrap();
        assert_eq!(insight.severity, Severity::Critical);

        snapshot.system.swap_used = 10;
        assert!(
            OomRiskDetector::default()
                .evaluate(&snapshot, &history)
                .is_none()
        );
    }

    #[test]
    fn leak_score_rule_grades_growth_and_ignores_stable() {
        use crate::collector::ProcessMemory;
        let base = std::time::Instant::now();
        let mut history = HistoryBuffer::new(20, Duration::from_secs(600));
        for index in 0..10 {
            let rss = 20 * 1024 * 1024 + index * 2 * 1024 * 1024;
            history.push(&crate::collector::MemorySnapshot {
                timestamp: base + Duration::from_secs(index),
                system: crate::collector::SystemMemory::default(),
                processes: vec![ProcessMemory {
                    pid: 777,
                    name: "leaky".into(),
                    rss,
                    vss: rss,
                    ..Default::default()
                }],
                total_processes: 1,
                running_processes: 1,
            });
        }
        let mut snapshot = test_support::snapshot_at(
            std::time::Instant::now(),
            20 * 1024 * 1024 + 9 * 2 * 1024 * 1024,
        );
        snapshot.processes[0].pid = 777;
        let insight = LeakScoreRule::default()
            .evaluate(&snapshot, &history)
            .unwrap();
        assert!(insight.id.starts_with("leak_score_777"));
        assert!(insight.title.contains("/100"));

        let calm_history = HistoryBuffer::new(4, Duration::from_secs(60));
        assert!(
            LeakScoreRule::default()
                .evaluate(&snapshot, &calm_history)
                .is_none()
        );
    }

    #[test]
    fn leak_score_rule_scores_only_the_trailing_window() {
        use crate::collector::ProcessMemory;
        let base = std::time::Instant::now();
        let mut history = HistoryBuffer::new(30, Duration::from_secs(600));
        // Ancient flat history must not rescue recent growth from scoring:
        // the rule sees only the last `window`.
        for index in 0..10u64 {
            history.push(&crate::collector::MemorySnapshot {
                timestamp: base + Duration::from_secs(index),
                system: crate::collector::SystemMemory::default(),
                processes: vec![ProcessMemory {
                    pid: 778,
                    name: "grower".into(),
                    rss: 20 * 1024 * 1024,
                    vss: 20 * 1024 * 1024,
                    ..Default::default()
                }],
                total_processes: 1,
                running_processes: 1,
            });
        }
        for index in 0..10u64 {
            let rss = 20 * 1024 * 1024 + index * 2 * 1024 * 1024;
            history.push(&crate::collector::MemorySnapshot {
                timestamp: base + Duration::from_secs(400 + index),
                system: crate::collector::SystemMemory::default(),
                processes: vec![ProcessMemory {
                    pid: 778,
                    name: "grower".into(),
                    rss,
                    vss: rss,
                    ..Default::default()
                }],
                total_processes: 1,
                running_processes: 1,
            });
        }
        let mut snapshot = test_support::snapshot_at(
            std::time::Instant::now(),
            20 * 1024 * 1024 + 9 * 2 * 1024 * 1024,
        );
        snapshot.processes[0].pid = 778;
        let rule = LeakScoreRule {
            window: Duration::from_secs(60),
            ..LeakScoreRule::default()
        };
        let insight = rule.evaluate(&snapshot, &history).unwrap();
        assert!(insight.id.starts_with("leak_score_778"));
    }
}
