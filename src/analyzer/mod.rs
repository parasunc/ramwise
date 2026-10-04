//! Memory analysis and insight generation
//!
//! This module contains the rule engine that analyzes memory patterns
//! and generates actionable insights.

mod engine;
mod insights;
mod leak_score;
mod rules;

pub use engine::Analyzer;
pub use insights::{Insight, Severity};
// Score components exported for UI and downstream consumers
#[allow(unused_imports)]
pub use leak_score::{LeakBand, LeakScore, LeakScoreInput, leak_score};
// Rule trait exported for extensibility
#[allow(unused_imports)]
pub use rules::Rule;
