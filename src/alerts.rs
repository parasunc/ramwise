//! Deterministic anomaly-alert foundations and calm mode.
//!
//! Rules still decide *what* is wrong; this module decides *what happens
//! next*. [`AlertDispatcher`] turns fresh Warning/Critical insights into
//! notifications with per-insight cooldown and deduplication, so a flapping
//! rule pages once, not every tick. Calm mode is the opposite direction:
//! under Critical pressure the UI sheds expensive work (trend rendering)
//! while critical alert dispatch keeps running — monitoring degrades
//! visibly, never silently.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::analyzer::{Insight, Severity};

/// Where alert notifications go.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AlertSink {
    /// `tracing::warn`, for journald/syslog pipelines.
    #[default]
    Log,
    /// One line per alert on stderr; stdout stays pure data.
    Stderr,
}

/// Tunable alert behavior. Cooldown is documented in seconds wherever it
/// surfaces so operators can reason about paging frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlertConfig {
    /// Minimum severity to notify on; Info insights never page.
    pub min_severity: Severity,
    /// Quiet period per insight ID before it may notify again.
    pub cooldown: Duration,
    /// When true, notifications are formatted and returned but never sent.
    pub dry_run: bool,
    pub sink: AlertSink,
}

impl Default for AlertConfig {
    fn default() -> Self {
        Self {
            min_severity: Severity::Warning,
            cooldown: Duration::from_secs(60),
            dry_run: false,
            sink: AlertSink::Log,
        }
    }
}

/// Dispatches insight notifications with cooldown and dedup. `dispatch`
/// returns the messages it emitted (dry-run included) so behavior is
/// observable without capturing global sinks.
pub struct AlertDispatcher {
    config: AlertConfig,
    notified_at: HashMap<String, Instant>,
}

impl AlertDispatcher {
    pub fn new(config: AlertConfig) -> Self {
        Self {
            config,
            notified_at: HashMap::new(),
        }
    }

    /// Notify for fresh, severe-enough insights. Calm mode never reaches
    /// this function — dispatch runs regardless of UI load shedding, so
    /// critical updates are preserved by construction.
    pub fn dispatch(&mut self, insights: &[&Insight], now: Instant) -> Vec<String> {
        // Prune expired cooldowns so the map stays bounded.
        self.notified_at
            .retain(|_, notified| now.saturating_duration_since(*notified) < self.config.cooldown);
        let mut emitted = Vec::new();
        for insight in insights {
            if insight.severity < self.config.min_severity || insight.acknowledged {
                continue;
            }
            // Escalations re-notify: a Warning that becomes Critical is a
            // new fact, not a duplicate.
            let key = format!("{}:{}", insight.id, insight.severity.as_str());
            if let Some(notified) = self.notified_at.get(&key)
                && now.saturating_duration_since(*notified) < self.config.cooldown
            {
                continue;
            }
            let message = format!(
                "[{}] {}: {}",
                insight.severity.as_str(),
                insight.title,
                insight.suggestion
            );
            if self.config.dry_run {
                // Dry runs preview without side effects: no sink, and no
                // bookkeeping that could suppress a later real notification.
                emitted.push(format!("[dry-run] {message}"));
                continue;
            }
            match self.config.sink {
                AlertSink::Log => tracing::warn!("{message}"),
                AlertSink::Stderr => eprintln!("{message}"),
            }
            self.notified_at.insert(key, now);
            emitted.push(message);
        }
        emitted
    }

    /// Whether dispatch only previews (used to surface dry-run output).
    pub fn is_dry_run(&self) -> bool {
        self.config.dry_run
    }
}

/// Calm mode: shed expensive UI work under load. Rendering the trend chart
/// is the first thing paused; collection, analysis and alert dispatch are
/// untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalmMode {
    pub active: bool,
    /// Whether automatic engagement may fire. A manual release disarms
    /// until pressure clears, so sustained Critical cannot re-latch over
    /// the user's explicit choice.
    auto_armed: bool,
}

impl Default for CalmMode {
    fn default() -> Self {
        Self {
            active: false,
            auto_armed: true,
        }
    }
}

impl CalmMode {
    /// Engage automatically under Critical pressure. Disengaging is manual
    /// (or automatic when pressure clears, which re-arms).
    pub fn auto_engage(&mut self, critical: bool) {
        if critical {
            if self.auto_armed {
                self.active = true;
            }
        } else {
            self.auto_armed = true;
        }
    }

    pub fn toggle(&mut self) {
        self.active = !self.active;
        if !self.active {
            self.auto_armed = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insight(id: &str, severity: Severity) -> Insight {
        Insight::new(id, severity, "title", "detail", "suggestion")
    }

    #[test]
    fn dispatch_notifies_once_per_cooldown() {
        // Log sink without a subscriber is a silent no-op: cooldown
        // behavior stays deterministic without capturing global sinks.
        let mut dispatcher = AlertDispatcher::new(AlertConfig::default());
        let warning = insight("w1", Severity::Warning);
        let now = Instant::now();
        assert_eq!(dispatcher.dispatch(&[&warning], now).len(), 1);
        assert!(dispatcher.dispatch(&[&warning], now).is_empty());
        let later = now + Duration::from_secs(61);
        assert_eq!(dispatcher.dispatch(&[&warning], later).len(), 1);
    }

    #[test]
    fn dry_run_previews_without_bookkeeping() {
        let mut dispatcher = AlertDispatcher::new(AlertConfig {
            dry_run: true,
            ..AlertConfig::default()
        });
        let warning = insight("w1", Severity::Warning);
        let now = Instant::now();
        let first = dispatcher.dispatch(&[&warning], now);
        assert_eq!(first.len(), 1);
        assert!(first[0].contains("[dry-run]"));
        // No bookkeeping: a later real dispatcher for the same insight
        // still notifies (dry runs never suppress real alerts).
        assert!(dispatcher.notified_at.is_empty());
    }

    #[test]
    fn info_insights_and_acknowledged_never_page() {
        let mut dispatcher = AlertDispatcher::new(AlertConfig::default());
        let info = insight("i1", Severity::Info);
        assert!(dispatcher.dispatch(&[&info], Instant::now()).is_empty());
        let mut warning = insight("w1", Severity::Warning);
        warning.acknowledged = true;
        assert!(dispatcher.dispatch(&[&warning], Instant::now()).is_empty());
    }

    #[test]
    fn expired_cooldowns_are_pruned() {
        let mut dispatcher = AlertDispatcher::new(AlertConfig::default());
        let now = Instant::now();
        // Seed an entry whose cooldown already expired under a distinct id.
        dispatcher
            .notified_at
            .insert("old".into(), now - Duration::from_secs(120));
        let warning = insight("w1", Severity::Warning);
        dispatcher.dispatch(&[&warning], now);
        // The expired entry is gone; only the fresh notification remains.
        assert_eq!(
            dispatcher.notified_at.keys().collect::<Vec<_>>(),
            vec!["w1:WARN"]
        );
    }

    #[test]
    fn severity_escalation_re_notifies() {
        let mut dispatcher = AlertDispatcher::new(AlertConfig::default());
        let now = Instant::now();
        let warning = insight("p1", Severity::Warning);
        assert_eq!(dispatcher.dispatch(&[&warning], now).len(), 1);
        // Same id, worse severity: a new fact, not a duplicate.
        let critical = insight("p1", Severity::Critical);
        assert_eq!(dispatcher.dispatch(&[&critical], now).len(), 1);
        // Same severity again: still cooling down.
        assert!(dispatcher.dispatch(&[&critical], now).is_empty());
    }

    #[test]
    fn calm_engages_on_critical_and_never_auto_releases() {
        let mut calm = CalmMode::default();
        calm.auto_engage(false);
        assert!(!calm.active);
        calm.auto_engage(true);
        assert!(calm.active);
        // Manual release holds through sustained Critical...
        calm.toggle();
        assert!(!calm.active);
        calm.auto_engage(true);
        assert!(!calm.active);
        // ...until pressure clears, which re-arms automatic engagement.
        calm.auto_engage(false);
        calm.auto_engage(true);
        assert!(calm.active);
    }
}
