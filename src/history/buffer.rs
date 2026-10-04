//! Ring buffer for memory snapshot history

#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::categories::{Category, classify};
use crate::collector::{MemorySnapshot, SystemMemory};

/// Data point for a process at a specific time
#[derive(Debug, Clone)]
pub struct ProcessDataPoint {
    pub timestamp: Instant,
    pub rss: u64,
    pub pss: u64,
    pub private: u64,
    pub swap: u64,
    /// Category at ingest time, so category series stay queryable even as
    /// binaries are reclassified or exit.
    pub category: Category,
}

/// Trend direction for memory usage
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trend {
    Increasing,
    Decreasing,
    Stable,
}

/// Growth statistics for a process
#[derive(Debug, Clone)]
pub struct GrowthStats {
    /// Bytes per second growth rate
    pub rate_per_sec: f64,
    /// Percentage change over the period
    pub percent_change: f64,
    /// Trend direction
    pub trend: Trend,
    /// Duration of the measurement period
    pub duration: Duration,
    /// Starting value
    pub start_value: u64,
    /// Ending value
    pub end_value: u64,
}

/// Ring buffer for memory snapshot history
pub struct HistoryBuffer {
    /// System-level snapshots
    system_history: VecDeque<(Instant, SystemMemory)>,
    /// Per-process history (indexed by PID)
    process_history: HashMap<i32, VecDeque<ProcessDataPoint>>,
    /// Maximum number of snapshots to keep
    capacity: usize,
    /// Maximum age of snapshots
    max_age: Duration,
}

impl HistoryBuffer {
    /// Create a new history buffer
    ///
    /// # Arguments
    /// * `capacity` - Maximum number of snapshots to keep
    /// * `max_age` - Maximum age of snapshots before pruning
    pub fn new(capacity: usize, max_age: Duration) -> Self {
        Self {
            system_history: VecDeque::with_capacity(capacity),
            process_history: HashMap::new(),
            capacity,
            max_age,
        }
    }

    /// Create with default settings (5 minutes of history at 1s intervals)
    pub fn default_5min() -> Self {
        Self::new(300, Duration::from_secs(300))
    }

    /// Add a new snapshot to the history
    pub fn push(&mut self, snapshot: &MemorySnapshot) {
        let timestamp = snapshot.timestamp;

        // Add system data
        self.system_history
            .push_back((timestamp, snapshot.system.clone()));

        // Add process data
        for proc in &snapshot.processes {
            let entry = self
                .process_history
                .entry(proc.pid)
                .or_insert_with(|| VecDeque::with_capacity(self.capacity));

            entry.push_back(ProcessDataPoint {
                timestamp,
                rss: proc.rss,
                pss: proc.pss,
                private: proc.private,
                swap: proc.swap,
                category: classify(proc),
            });

            // Trim old entries for this process
            while entry.len() > self.capacity {
                entry.pop_front();
            }
        }

        // Trim old system entries
        while self.system_history.len() > self.capacity {
            self.system_history.pop_front();
        }

        // Prune old data
        self.prune_old_data(timestamp);
    }

    /// Remove data older than max_age
    fn prune_old_data(&mut self, now: Instant) {
        let cutoff = now - self.max_age;

        // Prune system history
        while let Some((ts, _)) = self.system_history.front() {
            if *ts < cutoff {
                self.system_history.pop_front();
            } else {
                break;
            }
        }

        // Prune process history and remove empty entries
        let mut to_remove = Vec::new();
        for (pid, history) in self.process_history.iter_mut() {
            while let Some(point) = history.front() {
                if point.timestamp < cutoff {
                    history.pop_front();
                } else {
                    break;
                }
            }
            if history.is_empty() {
                to_remove.push(*pid);
            }
        }
        for pid in to_remove {
            self.process_history.remove(&pid);
        }
    }

    /// Get system memory trend data for graphing
    pub fn system_trend(&self) -> Vec<(Instant, u64)> {
        self.system_history
            .iter()
            .map(|(ts, mem)| (*ts, mem.used()))
            .collect()
    }

    /// Get system memory trend as percentage
    pub fn system_trend_percent(&self) -> Vec<(Instant, f64)> {
        self.system_history
            .iter()
            .map(|(ts, mem)| (*ts, mem.usage_percent()))
            .collect()
    }

    /// Get process RSS trend data for graphing
    pub fn process_trend(&self, pid: i32) -> Vec<(Instant, u64)> {
        self.process_history
            .get(&pid)
            .map(|history| history.iter().map(|p| (p.timestamp, p.rss)).collect())
            .unwrap_or_default()
    }

    /// Get process RSS trend as normalized values (0.0 - 1.0)
    pub fn process_trend_normalized(&self, pid: i32) -> Vec<f64> {
        let trend = self.process_trend(pid);
        if trend.is_empty() {
            return Vec::new();
        }

        let max = trend.iter().map(|(_, v)| *v).max().unwrap_or(1) as f64;
        let min = trend.iter().map(|(_, v)| *v).min().unwrap_or(0) as f64;
        let range = max - min;

        if range < 1.0 {
            // No significant change, return flat line at 0.5
            return vec![0.5; trend.len()];
        }

        trend
            .iter()
            .map(|(_, v)| (*v as f64 - min) / range)
            .collect()
    }

    /// Downsampled raw process trend for responsive charts: at most
    /// `max_points` averaged buckets over `window` ending at the latest
    /// point. The full five-minute history stays in the buffer; only the
    /// view is bounded, so narrow terminals and huge histories stay cheap.
    pub fn process_trend_downsampled(
        &self,
        pid: i32,
        window: Duration,
        max_points: usize,
    ) -> Vec<(Instant, u64)> {
        let trend = self.process_trend(pid);
        downsample(&trend, window, max_points)
    }

    /// Downsampled raw system trend, same contract as
    /// [`Self::process_trend_downsampled`].
    pub fn system_trend_downsampled(
        &self,
        window: Duration,
        max_points: usize,
    ) -> Vec<(Instant, u64)> {
        downsample(&self.system_trend(), window, max_points)
    }

    /// Normalized 0.0–1.0 process sparkline over `window`, capped at
    /// `max_points`. Empty means unknown (no data); a constant 0.5 line
    /// means flat. Backed by the same windowed buckets as the downsampled
    /// trend, so the sparkline and the chart never disagree.
    pub fn process_sparkline(&self, pid: i32, window: Duration, max_points: usize) -> Vec<f64> {
        normalize(&downsample(&self.process_trend(pid), window, max_points))
    }

    /// Normalized system sparkline, same contract as
    /// [`Self::process_sparkline`].
    pub fn system_sparkline(&self, window: Duration, max_points: usize) -> Vec<f64> {
        normalize(&downsample(&self.system_trend(), window, max_points))
    }

    /// Normalized sparkline of total RSS for one category: points sharing a
    /// snapshot tick are summed first, so each tick contributes its category
    /// total before windowing and bucketing. Windowed against the buffer's
    /// global latest tick: a category with no recent points reports unknown
    /// (empty) instead of a stale-looking series.
    pub fn category_sparkline(
        &self,
        category: Category,
        window: Duration,
        max_points: usize,
    ) -> Vec<f64> {
        use std::collections::BTreeMap;
        let Some(latest) = self.latest_timestamp() else {
            return Vec::new();
        };
        let cutoff = latest.checked_sub(window).unwrap_or_else(|| {
            self.system_history
                .front()
                .map(|(timestamp, _)| *timestamp)
                .unwrap_or(latest)
        });
        let mut per_tick: BTreeMap<Instant, u64> = BTreeMap::new();
        for history in self.process_history.values() {
            for point in history {
                if point.category == category && point.timestamp >= cutoff {
                    let entry = per_tick.entry(point.timestamp).or_insert(0);
                    *entry = entry.saturating_add(point.rss);
                }
            }
        }
        let series: Vec<(Instant, u64)> = per_tick.into_iter().collect();
        normalize(&downsample(&series, window, max_points))
    }

    /// Newest timestamp across system and process histories, if any.
    fn latest_timestamp(&self) -> Option<Instant> {
        let system_latest = self.system_history.back().map(|(timestamp, _)| *timestamp);
        let process_latest = self
            .process_history
            .values()
            .filter_map(|history| history.back().map(|point| point.timestamp))
            .max();
        system_latest.into_iter().chain(process_latest).max()
    }

    /// Calculate growth rate for a process over a duration
    pub fn growth_stats(&self, pid: i32, duration: Duration) -> Option<GrowthStats> {
        let history = self.process_history.get(&pid)?;
        if history.len() < 2 {
            return None;
        }

        let now = history.back()?.timestamp;
        let cutoff = now - duration;

        // Find the oldest point within the duration
        let start_point = history.iter().find(|p| p.timestamp >= cutoff)?;
        let end_point = history.back()?;

        let actual_duration = end_point.timestamp.duration_since(start_point.timestamp);
        if actual_duration.as_secs_f64() < 1.0 {
            return None;
        }

        let start_value = start_point.rss;
        let end_value = end_point.rss;
        let diff = end_value as i64 - start_value as i64;

        let rate_per_sec = diff as f64 / actual_duration.as_secs_f64();
        let percent_change = if start_value > 0 {
            (diff as f64 / start_value as f64) * 100.0
        } else {
            0.0
        };

        // Determine trend (threshold: 1% change or 1MB)
        let trend = if percent_change > 1.0 || diff > 1_000_000 {
            Trend::Increasing
        } else if percent_change < -1.0 || diff < -1_000_000 {
            Trend::Decreasing
        } else {
            Trend::Stable
        };

        Some(GrowthStats {
            rate_per_sec,
            percent_change,
            trend,
            duration: actual_duration,
            start_value,
            end_value,
        })
    }

    /// Check if a process has consistent growth (potential leak)
    pub fn is_consistently_growing(&self, pid: i32, threshold_percent: f64) -> bool {
        let history = match self.process_history.get(&pid) {
            Some(h) if h.len() >= 10 => h,
            _ => return false,
        };

        // Check if most recent values are higher than earlier values
        let len = history.len();
        let first_quarter: Vec<_> = history.iter().take(len / 4).collect();
        let last_quarter: Vec<_> = history.iter().skip(3 * len / 4).collect();

        if first_quarter.is_empty() || last_quarter.is_empty() {
            return false;
        }

        let first_avg: f64 =
            first_quarter.iter().map(|p| p.rss as f64).sum::<f64>() / first_quarter.len() as f64;
        let last_avg: f64 =
            last_quarter.iter().map(|p| p.rss as f64).sum::<f64>() / last_quarter.len() as f64;

        if first_avg < 1.0 {
            return false;
        }

        let growth_percent = ((last_avg - first_avg) / first_avg) * 100.0;
        growth_percent > threshold_percent
    }

    /// Get the number of snapshots stored
    pub fn len(&self) -> usize {
        self.system_history.len()
    }

    /// Check if the buffer is empty
    pub fn is_empty(&self) -> bool {
        self.system_history.is_empty()
    }

    /// Get the number of tracked processes
    pub fn tracked_processes(&self) -> usize {
        self.process_history.len()
    }

    /// Get latest system memory
    pub fn latest_system(&self) -> Option<&SystemMemory> {
        self.system_history.back().map(|(_, mem)| mem)
    }

    /// Get latest RSS for a process
    pub fn latest_rss(&self, pid: i32) -> Option<u64> {
        self.process_history.get(&pid)?.back().map(|p| p.rss)
    }
}

/// Slice a series to `window` ending at its latest point, then average into
/// at most `max_points` buckets. Timestamps come out ascending; each
/// bucket carries its newest timestamp.
fn downsample(
    values: &[(Instant, u64)],
    window: Duration,
    max_points: usize,
) -> Vec<(Instant, u64)> {
    if values.is_empty() || max_points == 0 {
        return Vec::new();
    }
    let latest = values
        .last()
        .map(|(timestamp, _)| *timestamp)
        .unwrap_or_else(Instant::now);
    let cutoff = latest.checked_sub(window).unwrap_or_else(|| {
        // A window longer than uptime underflows the subtraction: keep
        // every point rather than collapsing to just the latest one.
        values
            .first()
            .map(|(timestamp, _)| *timestamp)
            .unwrap_or(latest)
    });
    let in_window: Vec<(Instant, u64)> = values
        .iter()
        .copied()
        .filter(|(timestamp, _)| *timestamp >= cutoff)
        .collect();
    if in_window.len() <= max_points {
        return in_window;
    }
    let buckets = max_points.max(1);
    let chunk = in_window.len().div_ceil(buckets);
    in_window
        .chunks(chunk)
        .map(|chunk| {
            // u128 accumulation: bucket sums of byte counts cannot wrap.
            let sum: u128 = chunk.iter().map(|(_, value)| *value as u128).sum();
            let timestamp = chunk
                .last()
                .map(|(timestamp, _)| *timestamp)
                .unwrap_or(latest);
            let average = (sum / chunk.len().max(1) as u128).min(u64::MAX as u128) as u64;
            (timestamp, average)
        })
        .collect()
}

/// Normalize values to 0.0–1.0. Empty in, empty out (unknown); a constant
/// series maps to 0.5 (flat).
fn normalize(values: &[(Instant, u64)]) -> Vec<f64> {
    if values.is_empty() {
        return Vec::new();
    }
    let max = values.iter().map(|(_, v)| *v).max().unwrap_or(1) as f64;
    let min = values.iter().map(|(_, v)| *v).min().unwrap_or(0) as f64;
    if max - min < 1.0 {
        return vec![0.5; values.len()];
    }
    values
        .iter()
        .map(|(_, v)| (*v as f64 - min) / (max - min))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collector::ProcessMemory;
    use crate::test_support;

    #[test]
    fn test_history_buffer() {
        let mut buffer = HistoryBuffer::new(10, Duration::from_secs(60));

        let snapshot = MemorySnapshot {
            timestamp: Instant::now(),
            system: SystemMemory {
                total: 16_000_000_000,
                available: 8_000_000_000,
                ..Default::default()
            },
            processes: vec![ProcessMemory {
                pid: 1234,
                name: "test".to_string(),
                rss: 100_000_000,
                ..Default::default()
            }],
            total_processes: 1,
            running_processes: 1,
        };

        buffer.push(&snapshot);

        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.tracked_processes(), 1);
        assert!(buffer.latest_rss(1234).is_some());
    }

    #[test]
    fn test_growth_detection() {
        let mut buffer = HistoryBuffer::new(100, Duration::from_secs(60));

        // Simulate growing process
        let base_time = Instant::now();
        for i in 0..20 {
            let snapshot = MemorySnapshot {
                timestamp: base_time + Duration::from_secs(i),
                system: SystemMemory::default(),
                processes: vec![ProcessMemory {
                    pid: 1234,
                    name: "leaky".to_string(),
                    rss: 100_000_000 + (i * 10_000_000), // Growing by 10MB/sec
                    ..Default::default()
                }],
                total_processes: 1,
                running_processes: 1,
            };
            buffer.push(&snapshot);
        }

        assert!(buffer.is_consistently_growing(1234, 10.0));
    }

    #[test]
    fn deterministic_fixture_exercises_trend_and_pruning() {
        let snapshots = test_support::history_snapshots(4, 100, 10);
        let mut buffer = HistoryBuffer::new(3, Duration::from_secs(60));
        for snapshot in &snapshots {
            buffer.push(snapshot);
        }
        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.process_trend(test_support::FIXTURE_PID).len(), 3);
        assert_eq!(buffer.latest_rss(test_support::FIXTURE_PID), Some(130));
        assert_eq!(
            buffer.process_trend_normalized(test_support::FIXTURE_PID),
            vec![0.0, 0.5, 1.0]
        );
        let stats = buffer
            .growth_stats(test_support::FIXTURE_PID, Duration::from_secs(10))
            .unwrap();
        assert_eq!(stats.start_value, 110);
        assert_eq!(stats.end_value, 130);
        assert_eq!(stats.trend, Trend::Increasing);
    }

    #[test]
    fn downsampling_bounds_points_and_preserves_endpoints() {
        let snapshots = test_support::history_snapshots(10, 100, 10);
        let mut buffer = HistoryBuffer::new(20, Duration::from_secs(600));
        for snapshot in &snapshots {
            buffer.push(snapshot);
        }
        let pid = test_support::FIXTURE_PID;
        let full = buffer.process_trend_downsampled(pid, Duration::from_secs(600), 100);
        assert_eq!(full.len(), 10);
        let capped = buffer.process_trend_downsampled(pid, Duration::from_secs(600), 4);
        assert_eq!(capped.len(), 4);
        // Buckets average ascending values, so the series still ascends and
        // the last bucket carries the latest timestamp.
        assert!(capped.windows(2).all(|pair| pair[0].1 <= pair[1].1));
        assert_eq!(capped.last().unwrap().1, 190);
        // A short window slices to recent points only.
        let recent = buffer.process_trend_downsampled(pid, Duration::from_secs(2), 100);
        assert_eq!(recent.len(), 3);
    }

    #[test]
    fn sparklines_distinguish_unknown_from_flat() {
        let buffer = HistoryBuffer::new(10, Duration::from_secs(60));
        assert_eq!(
            buffer.process_sparkline(4242, Duration::from_secs(60), 8),
            Vec::<f64>::new()
        );
        let snapshots = test_support::history_snapshots(4, 100, 0);
        let mut buffer = HistoryBuffer::new(10, Duration::from_secs(600));
        for snapshot in &snapshots {
            buffer.push(snapshot);
        }
        assert_eq!(
            buffer.process_sparkline(test_support::FIXTURE_PID, Duration::from_secs(600), 8),
            vec![0.5; 4]
        );
        let rising = test_support::history_snapshots(4, 100, 10);
        let mut buffer = HistoryBuffer::new(10, Duration::from_secs(600));
        for snapshot in &rising {
            buffer.push(snapshot);
        }
        assert_eq!(
            buffer.process_sparkline(test_support::FIXTURE_PID, Duration::from_secs(600), 8),
            vec![0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0]
        );
    }

    #[test]
    fn stale_categories_report_unknown() {
        let base = Instant::now();
        let mut buffer = HistoryBuffer::new(10, Duration::from_secs(600));
        buffer.push(&MemorySnapshot {
            timestamp: base,
            system: SystemMemory::default(),
            processes: vec![ProcessMemory {
                pid: 11,
                name: "firefox".into(),
                cmdline: "firefox".into(),
                rss: 100,
                vss: 100,
                ..Default::default()
            }],
            total_processes: 1,
            running_processes: 1,
        });
        buffer.push(&MemorySnapshot {
            timestamp: base + Duration::from_secs(500),
            system: SystemMemory::default(),
            processes: vec![ProcessMemory {
                pid: 13,
                name: "sshd".into(),
                cmdline: "sshd".into(),
                rss: 100,
                vss: 100,
                ..Default::default()
            }],
            total_processes: 1,
            running_processes: 1,
        });
        // The browser tick is within retention but outside the 60s window
        // anchored at the latest tick: unknown, not a stale series.
        assert_eq!(
            buffer.category_sparkline(Category::Browser, Duration::from_secs(60), 8),
            Vec::<f64>::new()
        );
        assert_eq!(
            buffer.category_sparkline(Category::Service, Duration::from_secs(600), 8),
            vec![0.5]
        );
    }

    #[test]
    fn category_sparkline_sums_members_per_bucket() {
        let mut buffer = HistoryBuffer::new(10, Duration::from_secs(600));
        // Two browsers growing together plus an unrelated service.
        for (index, rss) in [100, 200].iter().enumerate() {
            let snapshot = MemorySnapshot {
                timestamp: Instant::now() + Duration::from_secs(index as u64),
                system: SystemMemory::default(),
                processes: vec![
                    ProcessMemory {
                        pid: 11,
                        name: "firefox".into(),
                        cmdline: "firefox".into(),
                        rss: *rss,
                        vss: *rss,
                        ..Default::default()
                    },
                    ProcessMemory {
                        pid: 12,
                        name: "chrome".into(),
                        cmdline: "chrome".into(),
                        rss: *rss,
                        vss: *rss,
                        ..Default::default()
                    },
                    ProcessMemory {
                        pid: 13,
                        name: "sshd".into(),
                        cmdline: "sshd".into(),
                        rss: 1000,
                        vss: 1000,
                        ..Default::default()
                    },
                ],
                total_processes: 3,
                running_processes: 3,
            };
            buffer.push(&snapshot);
        }
        let spark = buffer.category_sparkline(Category::Browser, Duration::from_secs(600), 8);
        // Bucket sums 200 then 400, normalized to 0.0 then 1.0.
        assert_eq!(spark, vec![0.0, 1.0]);
        let service = buffer.category_sparkline(Category::Service, Duration::from_secs(600), 8);
        assert_eq!(service, vec![0.5, 0.5]);
    }
}
