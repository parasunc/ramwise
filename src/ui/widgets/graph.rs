//! Modern memory trend graph widget

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{Axis, Block, Borders, Chart, Dataset, GraphType, Widget},
};

use crate::history::HistoryBuffer;
use crate::ui::Theme;
use crate::utils::format_bytes;

/// Modern graph widget for memory trends
pub struct GraphWidget<'a> {
    history: &'a HistoryBuffer,
    selected_pid: Option<i32>,
    theme: &'a Theme,
    focused: bool,
}

impl<'a> GraphWidget<'a> {
    pub fn new(history: &'a HistoryBuffer, theme: &'a Theme) -> Self {
        Self {
            history,
            selected_pid: None,
            theme,
            focused: false,
        }
    }

    pub fn selected_pid(mut self, pid: Option<i32>) -> Self {
        self.selected_pid = pid;
        self
    }

    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }
}

impl<'a> Widget for GraphWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        use std::time::Duration;
        /// Full leak window behind the chart; the buffer keeps it, the view
        /// only ever takes `width_points` buckets.
        const CHART_WINDOW: Duration = Duration::from_secs(300);
        /// Short window behind the title sparkline.
        const SPARK_WINDOW: Duration = Duration::from_secs(60);
        const SPARK_MAX: usize = 24;

        // Bound chart resolution by render width so huge histories and
        // narrow terminals stay cheap: at most one bucket per ~cell. The
        // title sparkline shares the same bound.
        let width_points = area.width.saturating_sub(8).max(2) as usize;
        let spark_max = SPARK_MAX.min(width_points);

        // Determine what to graph
        let (title, trend, spark, is_process) = if let Some(pid) = self.selected_pid {
            (
                "Process Memory",
                self.history
                    .process_trend_downsampled(pid, CHART_WINDOW, width_points),
                self.history.process_sparkline(pid, SPARK_WINDOW, spark_max),
                true,
            )
        } else {
            (
                "System Memory",
                self.history
                    .system_trend_downsampled(CHART_WINDOW, width_points),
                self.history.system_sparkline(SPARK_WINDOW, spark_max),
                false,
            )
        };
        if trend.is_empty() {
            render_empty(area, buf, self.theme, self.focused, "Collecting data…");
            return;
        }

        // Convert to chart data points (x = bucket index, y = bytes)
        let data: Vec<(f64, f64)> = trend
            .iter()
            .enumerate()
            .map(|(i, (_, bytes))| (i as f64, *bytes as f64))
            .collect();

        let max_bytes = trend.iter().map(|(_, b)| *b).max().unwrap_or(1) as f64;
        let min_bytes = trend.iter().map(|(_, b)| *b).min().unwrap_or(0) as f64;

        // Add 10% padding
        let range = (max_bytes - min_bytes).max(1.0);
        let y_min = (min_bytes - range * 0.1).max(0.0);
        let y_max = max_bytes + range * 0.1;

        let mid_label = format_bytes(((y_min + y_max) / 2.0) as u64);
        let y_labels = vec![
            Span::styled(
                format_bytes(y_min as u64),
                Style::default().fg(self.theme.fg_muted),
            ),
            Span::styled(mid_label, Style::default().fg(self.theme.fg_dim)),
            Span::styled(
                format_bytes(y_max as u64),
                Style::default().fg(self.theme.fg_muted),
            ),
        ];

        // Choose line color based on context
        let line_color = if is_process {
            self.theme.primary
        } else {
            self.theme.secondary
        };

        let x_max = data.len().saturating_sub(1) as f64;

        let datasets = vec![
            Dataset::default()
                .name("Memory")
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(line_color))
                .data(&data),
        ];

        // Modern time labels: the window behind the view, not the bucket
        // count (downsampling would otherwise mislabel the axis).
        let x_labels = vec![
            Span::styled(
                format!("-{}s", CHART_WINDOW.as_secs()),
                Style::default().fg(self.theme.fg_muted),
            ),
            Span::styled("now", Style::default().fg(self.theme.fg_dim)),
        ];

        // Modern title with compact sparkline and flat/unknown legend.
        // A "?" sparkline means the short window has no data (stale or
        // brand-new series); "flat" marks a constant series.
        let spark_text = sparkline_string(&spark);
        let flat = spark.len() > 1 && spark.iter().all(|point| *point == spark[0]);
        let mut title_line = Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled("◈ ", Style::default().fg(line_color)),
            Span::styled(
                title,
                Style::default()
                    .fg(self.theme.fg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {spark_text}"),
                Style::default().fg(self.theme.fg_dim),
            ),
        ]);
        if flat {
            title_line
                .spans
                .push(Span::styled(" · flat", self.theme.muted_style()));
        }

        let chart = Chart::new(datasets)
            .block(
                Block::default()
                    .title(title_line)
                    .borders(Borders::ALL)
                    .border_style(self.theme.border_style(self.focused))
                    .style(Style::default().bg(self.theme.bg)),
            )
            .x_axis(
                Axis::default()
                    .style(Style::default().fg(self.theme.border_subtle))
                    .bounds([0.0, x_max])
                    .labels(x_labels),
            )
            .y_axis(
                Axis::default()
                    .style(Style::default().fg(self.theme.border_subtle))
                    .bounds([y_min, y_max])
                    .labels(y_labels),
            );

        chart.render(area, buf);
    }
}

fn render_empty(area: Rect, buf: &mut Buffer, theme: &Theme, focused: bool, message: &str) {
    let title = Line::from(vec![
        Span::styled(" ", Style::default()),
        Span::styled("◈ ", Style::default().fg(theme.fg_muted)),
        Span::styled(
            "Memory Trend",
            Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ", Style::default()),
    ]);

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(theme.border_style(focused))
        .style(Style::default().bg(theme.bg));

    let inner = block.inner(area);
    block.render(area, buf);

    // Center the message with loading indicator
    let display = format!("◌ {}", message);
    let x = inner.x + (inner.width.saturating_sub(display.len() as u16)) / 2;
    let y = inner.y + inner.height / 2;

    buf.set_string(x, y, display, theme.muted_style());
}

/// Render normalized 0.0–1.0 points as compact block characters.
/// Empty input renders `?` (unknown); values outside range are clamped.
pub fn sparkline_string(points: &[f64]) -> String {
    const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if points.is_empty() {
        return "?".to_string();
    }
    points
        .iter()
        .map(|point| {
            let index = (point.clamp(0.0, 1.0) * 7.0).round() as usize;
            BLOCKS[index.min(7)]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::HistoryBuffer;
    use crate::test_support;
    use std::time::Duration;

    fn buffer_text(buf: &Buffer) -> String {
        buf.content.iter().map(|cell| cell.symbol()).collect()
    }

    fn filled_buffer(points: usize) -> HistoryBuffer {
        let mut buffer = HistoryBuffer::new(points + 10, Duration::from_secs(600));
        for snapshot in test_support::history_snapshots(points, 100, 10) {
            buffer.push(&snapshot);
        }
        buffer
    }

    #[test]
    fn sparkline_blocks_span_the_full_range() {
        assert_eq!(sparkline_string(&[]), "?");
        assert_eq!(sparkline_string(&[0.0]), "▁");
        assert_eq!(sparkline_string(&[0.5]), "▅");
        assert_eq!(sparkline_string(&[1.0]), "█");
        assert_eq!(sparkline_string(&[0.0, 0.5, 1.0]), "▁▅█");
        assert_eq!(sparkline_string(&[9.9]), "█");
    }

    #[test]
    fn graph_renders_downsampled_chart_with_legend() {
        let buffer = filled_buffer(40);
        let theme = Theme::dark();
        let area = Rect::new(0, 0, 60, 12);
        let mut buf = Buffer::empty(area);
        GraphWidget::new(&buffer, &theme).render(area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("System Memory"));
        assert!(!text.contains("Collecting"));
    }

    #[test]
    fn graph_marks_flat_series_and_survives_narrow_widths() {
        let mut buffer = HistoryBuffer::new(20, Duration::from_secs(600));
        for snapshot in test_support::history_snapshots(5, 100, 0) {
            buffer.push(&snapshot);
        }
        let theme = Theme::dark();
        let area = Rect::new(0, 0, 60, 12);
        let mut buf = Buffer::empty(area);
        GraphWidget::new(&buffer, &theme).render(area, &mut buf);
        assert!(buffer_text(&buf).contains("flat"));

        let narrow = Rect::new(0, 0, 12, 6);
        let mut buf = Buffer::empty(narrow);
        GraphWidget::new(&buffer, &theme).render(narrow, &mut buf);
    }

    #[test]
    fn graph_shows_collecting_state_without_data() {
        let buffer = HistoryBuffer::new(10, Duration::from_secs(60));
        let theme = Theme::dark();
        let area = Rect::new(0, 0, 60, 12);
        let mut buf = Buffer::empty(area);
        GraphWidget::new(&buffer, &theme)
            .selected_pid(Some(4242))
            .render(area, &mut buf);
        assert!(buffer_text(&buf).contains("Collecting"));
    }
}
