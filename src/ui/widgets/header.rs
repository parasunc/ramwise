//! Modern header widget with sleek design

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};

use crate::analyzer::{PressureLevel, PressureThresholds, classify};
use crate::collector::SystemMemory;
use crate::ui::Theme;
use crate::utils::format_bytes;

/// Modern header bar widget
pub struct HeaderWidget<'a> {
    system: &'a SystemMemory,
    theme: &'a Theme,
    version: &'static str,
}

impl<'a> HeaderWidget<'a> {
    pub fn new(system: &'a SystemMemory, theme: &'a Theme) -> Self {
        Self {
            system,
            theme,
            version: env!("CARGO_PKG_VERSION"),
        }
    }
}

impl<'a> Widget for HeaderWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let sys = self.system;

        // Build the header line with modern styling

        // App title with icon
        let title = Span::styled(
            format!(" ramwise v{}", self.version),
            Style::default()
                .fg(self.theme.primary)
                .add_modifier(Modifier::BOLD),
        );

        let dot = Span::styled(" · ", self.theme.muted_style());

        // RAM usage with smooth gradient bar
        let ram_percent = sys.usage_percent();
        let ram_color = self.theme.mem_color_interpolated(ram_percent);
        let ram_bar = create_sleek_bar(ram_percent, 12);

        let ram = vec![
            Span::styled("RAM ", Style::default().fg(self.theme.fg_dim)),
            Span::styled(ram_bar, Style::default().fg(ram_color)),
            Span::styled(
                format!(" {}/{} ", format_bytes(sys.used()), format_bytes(sys.total),),
                Style::default().fg(self.theme.fg),
            ),
            Span::styled(
                format!("{:.0}%", ram_percent),
                Style::default().fg(ram_color).add_modifier(Modifier::BOLD),
            ),
        ];

        // Swap usage with status indicator
        let swap_percent = sys.swap_percent();
        let swap = if sys.swap_total > 0 {
            let swap_color = if swap_percent > 80.0 {
                self.theme.error
            } else if swap_percent > 50.0 {
                self.theme.warning
            } else {
                self.theme.fg_dim
            };

            let status_icon = if swap_percent > 80.0 {
                "▲"
            } else if swap_percent > 50.0 {
                "●"
            } else {
                "○"
            };

            vec![
                dot.clone(),
                Span::styled("Swap ", Style::default().fg(self.theme.fg_dim)),
                Span::styled(status_icon, Style::default().fg(swap_color)),
                Span::styled(
                    format!(
                        " {}/{} ",
                        format_bytes(sys.swap_used),
                        format_bytes(sys.swap_total),
                    ),
                    Style::default().fg(self.theme.fg),
                ),
                Span::styled(
                    format!("{:.0}%", swap_percent),
                    Style::default().fg(swap_color),
                ),
            ]
        } else {
            vec![]
        };

        // Help hint with modern styling (right-aligned)
        let help = vec![
            Span::styled(
                "?",
                Style::default()
                    .fg(self.theme.secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Help  ", self.theme.muted_style()),
            Span::styled(
                "q",
                Style::default()
                    .fg(self.theme.tertiary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" Quit ", self.theme.muted_style()),
        ];

        // Combine in priority order — title, RAM, pressure, swap, help —
        // so narrow terminals degrade gracefully: each segment joins only
        // when it fits, and the pressure level always survives (full label
        // when room allows, single glyph otherwise).
        let level = classify(sys, &PressureThresholds::default());
        let width = area.width as usize;
        let mut spans = vec![title, dot.clone()];
        let mut pressure_shown = false;
        let pressure_full = pressure_spans(level, self.theme);
        let candidates = [&ram, &pressure_full, &swap, &help];
        for (index, candidate) in candidates.iter().enumerate() {
            let used: usize = spans.iter().map(|span| span.width()).sum();
            let extra: usize = candidate.iter().map(|span| span.width()).sum();
            if used + extra <= width {
                spans.extend(candidate.iter().cloned());
                pressure_shown = pressure_shown || index == 1;
            }
        }
        if !pressure_shown {
            let used: usize = spans.iter().map(|span| span.width()).sum();
            if used < width {
                spans.push(pressure_glyph(level, self.theme));
            }
        }

        let line = Line::from(spans);
        let paragraph = Paragraph::new(line).style(Style::default().bg(self.theme.bg_elevated));

        paragraph.render(area, buf);
    }
}

/// Create a sleek progress bar with partial block characters
fn create_sleek_bar(percent: f64, width: usize) -> String {
    let chars = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];
    let total_eighths = ((percent / 100.0) * (width * 8) as f64).round() as usize;
    let full_blocks = total_eighths / 8;
    let partial = total_eighths % 8;

    let mut bar = "█".repeat(full_blocks);

    if partial > 0 && full_blocks < width {
        bar.push(chars[partial]);
    }

    let remaining = width.saturating_sub(bar.chars().count());
    bar.push_str(&"░".repeat(remaining));

    bar
}

/// Color for a pressure level; unknown renders dim so absence of data is
/// visible rather than silently green.
pub(crate) fn pressure_color(level: PressureLevel, theme: &Theme) -> ratatui::style::Color {
    match level {
        PressureLevel::Unknown => theme.fg_dim,
        PressureLevel::Stable => theme.success,
        PressureLevel::Elevated => theme.warning,
        PressureLevel::Critical => theme.error,
    }
}

/// Full pressure segment: separator, label, colored glyph, level name.
fn pressure_spans(level: PressureLevel, theme: &Theme) -> Vec<Span<'static>> {
    let color = pressure_color(level, theme);
    vec![
        Span::styled(" · ", theme.muted_style()),
        Span::styled("Pressure ", Style::default().fg(theme.fg_dim)),
        pressure_glyph(level, theme),
        Span::styled(
            format!(" {}", level.label()),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ]
}

/// Single-cell glyph for very narrow terminals.
fn pressure_glyph(level: PressureLevel, theme: &Theme) -> Span<'static> {
    let color = pressure_color(level, theme);
    let glyph = match level {
        PressureLevel::Unknown => "?",
        _ => "●",
    };
    Span::styled(
        glyph,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;
    use crate::ui::Theme;

    fn buffer_text(buf: &Buffer) -> String {
        buf.content.iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn header_renders_pressure_level_at_full_width() {
        let theme = Theme::dark();
        let system = test_support::system_memory();
        let area = Rect::new(0, 0, 120, 1);
        let mut buf = Buffer::empty(area);
        HeaderWidget::new(&system, &theme).render(area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("RAM"));
        assert!(text.contains("stable"));
    }

    #[test]
    fn header_survives_narrow_terminals() {
        let theme = Theme::dark();
        let system = test_support::system_memory();
        for width in [20, 30, 40, 60, 69, 70, 80, 120] {
            let area = Rect::new(0, 0, width, 1);
            let mut buf = Buffer::empty(area);
            HeaderWidget::new(&system, &theme).render(area, &mut buf);
            let text = buffer_text(&buf);
            assert!(text.contains("ramwise"), "width {width}: {text}");
            // The pressure level always survives: full label or glyph.
            assert!(
                text.contains("stable") || text.contains('●'),
                "width {width}: {text}"
            );
        }
    }

    #[test]
    fn header_tiers_drop_help_swap_and_ram_in_order() {
        let theme = Theme::dark();
        let system = test_support::system_memory();
        let render = |width: u16| {
            let area = Rect::new(0, 0, width, 1);
            let mut buf = Buffer::empty(area);
            HeaderWidget::new(&system, &theme).render(area, &mut buf);
            buffer_text(&buf)
        };
        let full = render(130);
        assert!(full.contains("Quit"));
        assert!(full.contains("Swap"));
        let medium = render(80);
        assert!(medium.contains("stable"));
        assert!(!medium.contains("Quit"));
        let small = render(30);
        assert!(small.contains('●'));
        assert!(!small.contains("RAM "));
    }

    #[test]
    fn header_marks_unknown_pressure_explicitly() {
        let theme = Theme::dark();
        let system = SystemMemory::default();
        let area = Rect::new(0, 0, 120, 1);
        let mut buf = Buffer::empty(area);
        HeaderWidget::new(&system, &theme).render(area, &mut buf);
        assert!(buffer_text(&buf).contains("unknown"));
    }
}
