//! Calibrated 0–100 leak score.
//!
//! The binary leak detector answers "leaking or not". This score answers
//! "how convinced are we": 0 means no leak evidence, 100 means a large,
//! steady, low-noise climb observed long enough to trust.
//!
//! ```text
//! score = 100 × growth × (0.5 × monotonicity + 0.3 × stability + 0.2 × coverage)
//! ```
//!
//! - **growth**: relative climb `(end − start) / start`, clamped to 0–1.
//!   No climb, no score — stable and shrinking series always score 0.
//! - **monotonicity**: fraction of ascending adjacent pairs. Steady leaks
//!   approach 1; sawtooth noise lands near 0.5.
//! - **stability**: `1 − stdev/mean`, clamped to 0–1. Noise destroys
//!   confidence even when the endpoints moved.
//! - **coverage**: sample trust (`n/10`) times duration trust
//!   (`duration/60s`), each capped at 1. Ten points over a minute earn full
//!   coverage; a ten-second burst does not, no matter how steep.
//!
//! Guards: fewer than 3 samples, a non-positive duration, or a peak below
//! `min_size_bytes` all score 0 with zeroed components, so tiny or
//! barely-observed processes can never look leaky.
//!
//! Bands: 0–24 low, 25–49 watch, 50–74 likely, 75–100 severe.
//!
//! Complement to [`crate::analyzer::rules::MemoryLeakDetector`]: the binary
//! detector trips on a growth threshold (conservative, 50 MB floor); this
//! score grades the evidence (sensitive, 10 MB floor). Both may fire on a
//! strong leak — the score explains *why* it is convincing — while weak or
//! brief climbs surface only as a low score, never as an insight.

use std::time::Duration;

/// Score bands with stable labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeakBand {
    Low,
    Watch,
    Likely,
    Severe,
}

impl LeakBand {
    pub fn label(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Watch => "watch",
            Self::Likely => "likely",
            Self::Severe => "severe",
        }
    }
}

impl From<u8> for LeakBand {
    fn from(score: u8) -> Self {
        match score {
            0..=24 => Self::Low,
            25..=49 => Self::Watch,
            50..=74 => Self::Likely,
            _ => Self::Severe,
        }
    }
}

/// Explainable leak assessment for one RSS series.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LeakScore {
    /// Calibrated 0–100 score.
    pub score: u8,
    pub band: LeakBand,
    /// Fraction of ascending adjacent pairs.
    pub monotonicity: f64,
    /// Relative climb, clamped to 0–1.
    pub growth: f64,
    /// One minus coefficient of variation, clamped to 0–1.
    pub stability: f64,
    /// Observation trust from sample count.
    pub coverage: f64,
}

/// Inputs for scoring. `rss` must be time-ascending.
pub struct LeakScoreInput<'a> {
    pub rss: &'a [u64],
    pub duration: Duration,
    pub min_size_bytes: u64,
}

/// Score one RSS series. Pure and deterministic: the same inputs always
/// produce the same score and components.
pub fn leak_score(input: LeakScoreInput<'_>) -> LeakScore {
    let zero = LeakScore {
        score: 0,
        band: LeakBand::Low,
        monotonicity: 0.0,
        growth: 0.0,
        stability: 0.0,
        coverage: 0.0,
    };
    let rss = input.rss;
    if rss.len() < 3 || input.duration.is_zero() {
        return zero;
    }
    let peak = rss.iter().copied().max().unwrap_or(0);
    if peak < input.min_size_bytes {
        return zero;
    }
    let start = rss[0] as f64;
    let end = rss[rss.len() - 1] as f64;
    if end <= start || start <= 0.0 {
        return zero;
    }

    let growth = ((end - start) / start).min(1.0);
    let ascending = rss.windows(2).filter(|pair| pair[1] > pair[0]).count();
    let monotonicity = ascending as f64 / (rss.len() - 1) as f64;
    // f64 accumulation: u64 byte-count sums would wrap long before they
    // trouble a float.
    let mean = rss.iter().map(|value| *value as f64).sum::<f64>() / rss.len() as f64;
    let variance = rss
        .iter()
        .map(|value| (*value as f64 - mean).powi(2))
        .sum::<f64>()
        / rss.len() as f64;
    let stability = (1.0 - variance.sqrt() / mean).clamp(0.0, 1.0);
    let coverage =
        (rss.len() as f64 / 10.0).min(1.0) * (input.duration.as_secs_f64() / 60.0).min(1.0);

    let score = (100.0 * growth * (0.5 * monotonicity + 0.3 * stability + 0.2 * coverage))
        .round()
        .clamp(0.0, 100.0) as u8;
    LeakScore {
        score,
        band: LeakBand::from(score),
        monotonicity,
        growth,
        stability,
        coverage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score_of(rss: &[u64]) -> LeakScore {
        leak_score(LeakScoreInput {
            rss,
            duration: Duration::from_secs(60),
            min_size_bytes: 0,
        })
    }

    #[test]
    fn steady_growth_scores_severe() {
        let rss: Vec<u64> = (0..10).map(|i| 100 + i * 10).collect();
        let score = score_of(&rss);
        assert!(score.score >= 75, "score {}", score.score);
        assert_eq!(score.band, LeakBand::Severe);
        assert_eq!(score.monotonicity, 1.0);
        assert!((score.growth - 0.9).abs() < 1e-9);
    }

    #[test]
    fn stable_series_scores_zero() {
        let score = score_of(&[100; 10]);
        assert_eq!(score.score, 0);
        assert_eq!(score.band, LeakBand::Low);
    }

    #[test]
    fn shrinking_series_scores_zero() {
        let rss: Vec<u64> = (0..10).map(|i| 200 - i * 10).collect();
        assert_eq!(score_of(&rss).score, 0);
    }

    #[test]
    fn noise_without_trend_scores_low() {
        // Oscillates ±20 around a flat mean: zero net climb.
        let rss = [100, 120, 100, 120, 100, 120, 100, 120, 100, 100];
        assert_eq!(score_of(&rss).score, 0);
    }

    #[test]
    fn noisy_growth_scores_below_steady_growth() {
        let steady: Vec<u64> = (0..10).map(|i| 100 + i * 10).collect();
        let noisy = [100, 150, 110, 160, 120, 170, 130, 180, 140, 190];
        let steady_score = score_of(&steady).score;
        let noisy_score = score_of(&noisy).score;
        assert!(noisy_score > 0, "noisy growth is still evidence");
        assert!(
            noisy_score < steady_score,
            "noisy {noisy_score} vs steady {steady_score}"
        );
    }

    #[test]
    fn short_bursts_cannot_score_severe() {
        // Same steep climb as the severe fixture, but over 9 seconds:
        // duration trust caps coverage, so this lands in Likely at best.
        let rss: Vec<u64> = (0..10).map(|i| 100 + i * 10).collect();
        let score = leak_score(LeakScoreInput {
            rss: &rss,
            duration: Duration::from_secs(9),
            min_size_bytes: 0,
        });
        assert!(score.score < 75, "score {}", score.score);
        assert!(score.score >= 50, "score {}", score.score);
    }

    #[test]
    fn large_stable_processes_score_zero() {
        let rss = vec![100 * 1024 * 1024; 30];
        let score = leak_score(LeakScoreInput {
            rss: &rss,
            duration: Duration::from_secs(300),
            min_size_bytes: 10 * 1024 * 1024,
        });
        assert_eq!(score.score, 0);
    }

    #[test]
    fn guards_reject_thin_and_tiny_series() {
        assert_eq!(score_of(&[100, 200]).score, 0);
        assert_eq!(score_of(&[]).score, 0);
        let zero_duration = leak_score(LeakScoreInput {
            rss: &[100, 150, 200],
            duration: Duration::ZERO,
            min_size_bytes: 0,
        });
        assert_eq!(zero_duration.score, 0);
        let tiny = leak_score(LeakScoreInput {
            rss: &[100, 150, 200],
            duration: Duration::from_secs(60),
            min_size_bytes: 10_000,
        });
        assert_eq!(tiny.score, 0);
    }

    #[test]
    fn score_is_bounded_and_bands_cover_everything() {
        let cases = [
            (0u8, LeakBand::Low),
            (24, LeakBand::Low),
            (25, LeakBand::Watch),
            (49, LeakBand::Watch),
            (50, LeakBand::Likely),
            (74, LeakBand::Likely),
            (75, LeakBand::Severe),
            (100, LeakBand::Severe),
        ];
        for (score, band) in cases {
            assert_eq!(LeakBand::from(score), band);
        }
        assert_eq!(LeakBand::from(0).label(), "low");
        assert_eq!(LeakBand::from(100).label(), "severe");
    }

    #[test]
    fn scoring_is_deterministic() {
        let rss: Vec<u64> = (0..10).map(|i| 100 + i * 10).collect();
        assert_eq!(score_of(&rss), score_of(&rss));
    }
}
