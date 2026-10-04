//! Modern insights panel with badge-style alerts

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

use crate::analyzer::{Insight, Severity};
use crate::collector::SystemMemory;
use crate::ui::Theme;

/// Modern insights panel widget
pub struct InsightsPanelWidget<'a> {
    insights: Vec<&'a Insight>,
    theme: &'a Theme,
    focused: bool,
    system: Option<&'a SystemMemory>,
}

impl<'a> InsightsPanelWidget<'a> {
    pub fn new(insights: Vec<&'a Insight>, theme: &'a Theme) -> Self {
        Self {
            insights,
            theme,
            focused: false,
            system: None,
        }
    }

    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Show the memory-pressure summary line above the insights.
    pub fn pressure(mut self, system: &'a SystemMemory) -> Self {
        self.system = Some(system);
        self
    }
}

impl<'a> Widget for InsightsPanelWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Count by severity for title badges
        let (crit, warn, info) = count_by_severity(&self.insights);

        // Modern title with count badges
        let title = if self.insights.is_empty() {
            Line::from(vec![
                Span::styled(" ", Style::default()),
                Span::styled(
                    "Insights",
                    Style::default()
                        .fg(self.theme.fg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" · ", self.theme.muted_style()),
                Span::styled("All clear", Style::default().fg(self.theme.success)),
                Span::styled(" ", Style::default()),
            ])
        } else {
            let mut spans = vec![
                Span::styled(" ", Style::default()),
                Span::styled(
                    "Insights",
                    Style::default()
                        .fg(self.theme.fg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" ", Style::default()),
            ];

            if crit > 0 {
                spans.push(Span::styled(
                    format!(" {} ", crit),
                    Style::default()
                        .fg(self.theme.bg)
                        .bg(self.theme.error)
                        .add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(" ", Style::default()));
            }
            if warn > 0 {
                spans.push(Span::styled(
                    format!(" {} ", warn),
                    Style::default()
                        .fg(self.theme.bg)
                        .bg(self.theme.warning)
                        .add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(" ", Style::default()));
            }
            if info > 0 {
                spans.push(Span::styled(
                    format!(" {} ", info),
                    Style::default()
                        .fg(self.theme.bg)
                        .bg(self.theme.info)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            spans.push(Span::styled(" ", Style::default()));

            Line::from(spans)
        };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(self.theme.border_style(self.focused))
            .style(Style::default().bg(self.theme.bg));

        let inner = block.inner(area);
        block.render(area, buf);

        // Optional pressure summary reserves the first row so the level and
        // the used-versus-available explanation are always visible.
        let mut lines: Vec<Line> = Vec::new();
        if let Some(system) = self.system {
            lines.push(pressure_line(system, self.theme));
        }
        let reserved = lines.len();

        if self.insights.is_empty() {
            // Show success state with icon
            lines.push(Line::from(vec![
                Span::styled("  ✓ ", Style::default().fg(self.theme.success)),
                Span::styled(
                    "System looks healthy",
                    Style::default().fg(self.theme.fg_dim),
                ),
            ]));
            let paragraph = Paragraph::new(lines);
            paragraph.render(inner, buf);
            return;
        }

        // Build lines for each insight with modern styling
        let insight_lines = self
            .insights
            .iter()
            .take((inner.height as usize).saturating_sub(reserved))
            .map(|insight| {
                // Severity icon with color
                let (icon, icon_style) = match insight.severity {
                    Severity::Critical => (
                        "▲",
                        Style::default()
                            .fg(self.theme.error)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Severity::Warning => ("●", Style::default().fg(self.theme.warning)),
                    Severity::Info => ("○", Style::default().fg(self.theme.info)),
                };

                // Process target with PID
                let target = match (&insight.process_name, insight.pid) {
                    (Some(name), Some(_pid)) => format!("{} ", name),
                    (Some(name), None) => format!("{} ", name),
                    (None, Some(pid)) => format!("PID {} ", pid),
                    (None, None) => String::new(),
                };

                let pid_str = insight.pid.map(|p| format!("[{}]", p)).unwrap_or_default();

                // Title
                let title = &insight.title;

                // Truncate suggestion to fit
                let used_width = 4 + target.len() + pid_str.len() + title.len() + 4;
                let remaining = (inner.width as usize).saturating_sub(used_width);

                let suggestion = if remaining > 10 {
                    let sug = &insight.suggestion;
                    if sug.len() > remaining {
                        format!("→ {}…", &sug[..remaining.saturating_sub(3)])
                    } else {
                        format!("→ {}", sug)
                    }
                } else {
                    String::new()
                };

                Line::from(vec![
                    Span::styled(format!(" {} ", icon), icon_style),
                    Span::styled(target, Style::default().fg(self.theme.primary)),
                    Span::styled(pid_str, Style::default().fg(self.theme.fg_muted)),
                    Span::styled(" ", Style::default()),
                    Span::styled(title.clone(), Style::default().fg(self.theme.fg)),
                    Span::styled(format!(" {}", suggestion), self.theme.muted_style()),
                ])
            })
            .collect::<Vec<Line>>();
        lines.extend(insight_lines);

        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: true });
        paragraph.render(inner, buf);
    }
}

/// First-row pressure summary: level plus used-versus-available explanation.
fn pressure_line(system: &SystemMemory, theme: &Theme) -> Line<'static> {
    use crate::analyzer::{PressureThresholds, classify, explain};
    let level = classify(system, &PressureThresholds::default());
    let color = super::header::pressure_color(level, theme);
    Line::from(vec![
        Span::styled(" ● ", Style::default().fg(color)),
        Span::styled(
            format!("Pressure {} — ", level.label()),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(explain(system, level), Style::default().fg(theme.fg_dim)),
    ])
}

fn count_by_severity(insights: &[&Insight]) -> (usize, usize, usize) {
    let mut crit = 0;
    let mut warn = 0;
    let mut info = 0;

    for i in insights.iter() {
        match i.severity {
            Severity::Critical => crit += 1,
            Severity::Warning => warn += 1,
            Severity::Info => info += 1,
        }
    }

    (crit, warn, info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::Severity;
    use crate::collector::SystemMemory;
    use crate::test_support;
    use crate::ui::Theme;

    fn buffer_text(buf: &Buffer) -> String {
        buf.content.iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn pressure_line_shows_with_empty_insights() {
        let theme = Theme::dark();
        let system = test_support::system_memory();
        let area = Rect::new(0, 0, 130, 6);
        let mut buf = Buffer::empty(area);
        InsightsPanelWidget::new(vec![], &theme)
            .pressure(&system)
            .render(area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("Pressure"));
        assert!(text.contains("stable"));
        assert!(text.contains("available for new applications"));
    }

    #[test]
    fn pressure_line_reserves_a_row_alongside_insights() {
        let theme = Theme::dark();
        let system = test_support::system_memory();
        let insight = Insight::new("hog_1", Severity::Warning, "hog", "detail", "free memory");
        let area = Rect::new(0, 0, 80, 8);
        let mut buf = Buffer::empty(area);
        InsightsPanelWidget::new(vec![&insight], &theme)
            .pressure(&system)
            .render(area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("Pressure"));
        assert!(text.contains("hog"));
    }

    #[test]
    fn tiny_panel_clips_insights_but_keeps_pressure() {
        let theme = Theme::dark();
        let system = SystemMemory::default();
        let insight = Insight::new("hog_1", Severity::Warning, "hog", "detail", "free memory");
        let area = Rect::new(0, 0, 80, 3);
        let mut buf = Buffer::empty(area);
        InsightsPanelWidget::new(vec![&insight], &theme)
            .pressure(&system)
            .render(area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("unknown"));
        assert!(!text.contains("hog"));
    }
}
