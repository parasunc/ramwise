//! Modern detail panel widget with card-like sections

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

use crate::accounting::{composition_segments, system_notes};
use crate::collector::{ProcessMemory, SystemMemory};
use crate::ui::Theme;
use crate::utils::format_bytes;

/// Modern detail panel widget
pub struct DetailPanelWidget<'a> {
    process: Option<&'a ProcessMemory>,
    system: Option<&'a SystemMemory>,
    theme: &'a Theme,
    focused: bool,
}

impl<'a> DetailPanelWidget<'a> {
    pub fn new(process: Option<&'a ProcessMemory>, theme: &'a Theme) -> Self {
        Self {
            process,
            system: None,
            theme,
            focused: false,
        }
    }

    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Attach system metrics for the composition bar scale and the
    /// accounting-education section.
    pub fn system(mut self, system: &'a SystemMemory) -> Self {
        self.system = Some(system);
        self
    }
}

impl<'a> Widget for DetailPanelWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Modern title with icon-like prefix
        let title = match &self.process {
            Some(p) => Line::from(vec![
                Span::styled(" ◆ ", Style::default().fg(self.theme.primary)),
                Span::styled(
                    &p.name,
                    Style::default()
                        .fg(self.theme.fg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" · ", self.theme.muted_style()),
                Span::styled(
                    format!("PID {}", p.pid),
                    Style::default().fg(self.theme.secondary),
                ),
                Span::styled(" ", Style::default()),
            ]),
            None => Line::from(vec![
                Span::styled(" ◇ ", self.theme.muted_style()),
                Span::styled("Select a process", self.theme.muted_style()),
                Span::styled(" ", Style::default()),
            ]),
        };

        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(self.theme.border_style(self.focused))
            .style(Style::default().bg(self.theme.bg));

        let inner = block.inner(area);
        block.render(area, buf);

        let Some(proc) = self.process else {
            // Render placeholder with icon
            let lines = vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled("   ↑ ", self.theme.muted_style()),
                    Span::styled("Use ", self.theme.dim_style()),
                    Span::styled("j/k", Style::default().fg(self.theme.secondary)),
                    Span::styled(" or ", self.theme.dim_style()),
                    Span::styled("↑/↓", Style::default().fg(self.theme.secondary)),
                    Span::styled(" to navigate", self.theme.dim_style()),
                ]),
            ];
            let placeholder = Paragraph::new(lines);
            placeholder.render(inner, buf);
            return;
        };

        // Build content with modern sections
        let mut lines = Vec::new();

        // Process info row with status chip
        let state_style = match proc.state {
            'R' => Style::default().fg(self.theme.success),
            'S' => Style::default().fg(self.theme.info),
            'D' => Style::default().fg(self.theme.warning),
            'Z' => Style::default().fg(self.theme.error),
            _ => Style::default().fg(self.theme.fg_dim),
        };

        lines.push(Line::from(vec![
            Span::styled(state_chip(proc.state), state_style),
            Span::styled("  ", Style::default()),
            Span::styled("PPID ", self.theme.muted_style()),
            Span::styled(
                proc.ppid.to_string(),
                Style::default().fg(self.theme.fg_dim),
            ),
            Span::styled("  UID ", self.theme.muted_style()),
            Span::styled(proc.uid.to_string(), Style::default().fg(self.theme.fg_dim)),
        ]));

        // Command line (styled as code)
        let max_cmd_len = inner.width as usize - 2;
        let cmdline = if proc.cmdline.len() > max_cmd_len {
            format!("{}…", &proc.cmdline[..max_cmd_len.saturating_sub(1)])
        } else {
            proc.cmdline.clone()
        };
        lines.push(Line::from(vec![Span::styled(
            cmdline,
            Style::default().fg(self.theme.fg_muted),
        )]));

        // Heuristic category label so the classification is visible per process
        let category = crate::categories::classify(proc);
        lines.push(Line::from(vec![
            Span::styled("Category ", self.theme.muted_style()),
            Span::styled(
                category.label(),
                Style::default()
                    .fg(self.theme.secondary)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));

        lines.push(Line::from(""));

        // Memory section header
        lines.push(section_header("Memory", self.theme));

        // Main memory stats with visual bars
        let total_for_bar = proc.vss.max(proc.rss);

        lines.push(create_memory_row(
            "RSS",
            proc.rss,
            total_for_bar,
            "Resident memory",
            self.theme,
            true,
        ));

        lines.push(create_memory_row(
            "VSS",
            proc.vss,
            total_for_bar,
            "Virtual size",
            self.theme,
            false,
        ));

        if proc.pss > 0 {
            lines.push(create_memory_row(
                "PSS",
                proc.pss,
                proc.rss,
                "Proportional",
                self.theme,
                false,
            ));
        }

        if proc.uss > 0 {
            lines.push(create_memory_row(
                "USS", proc.uss, proc.rss, "Unique", self.theme, false,
            ));
        }

        lines.push(Line::from(""));

        // Breakdown section
        lines.push(section_header("Breakdown", self.theme));

        // Two-column layout for breakdown
        lines.push(create_two_col(
            ("Shared", proc.shared),
            ("Private", proc.private),
            self.theme,
        ));

        lines.push(create_two_col(
            ("Heap", proc.heap),
            ("Stack", proc.stack),
            self.theme,
        ));

        lines.push(create_two_col(
            ("Libraries", proc.libs),
            ("Anon", proc.anonymous),
            self.theme,
        ));

        // Stacked composition bar: private/shared/swap as RSS fractions.
        // Only collected segments render; unavailable types leave no gap.
        let segments = composition_segments(proc);
        if !segments.is_empty() {
            lines.push(Line::from(""));
            lines.push(section_header("Composition", self.theme));
            lines.push(Line::from(composition_bar(&segments, self.theme)));
            for segment in &segments {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {} ", segment.label), self.theme.muted_style()),
                    Span::styled(
                        format_bytes(segment.bytes),
                        Style::default().fg(self.theme.fg_dim),
                    ),
                    Span::styled(
                        format!(" ({:.0}%)", segment.fraction * 100.0),
                        self.theme.muted_style(),
                    ),
                ]));
            }
        }

        // System accounting education with explicit caveats.
        let notes = self.system.map(system_notes).unwrap_or_default();
        if !notes.is_empty() {
            lines.push(Line::from(""));
            lines.push(section_header("Accounting", self.theme));
            for note in &notes {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {}: ", note.label),
                        Style::default()
                            .fg(self.theme.secondary)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(&note.text, self.theme.muted_style()),
                ]));
            }
        }

        // Swap indicator (if any)
        if proc.swap > 0 {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("▲ ", Style::default().fg(self.theme.warning)),
                Span::styled("Swap: ", self.theme.dim_style()),
                Span::styled(format_bytes(proc.swap), self.theme.warning_style()),
            ]));
        }

        // Page faults with severity indication
        if proc.major_faults > 100 {
            lines.push(Line::from(vec![
                Span::styled(
                    "● ",
                    Style::default().fg(if proc.major_faults > 1000 {
                        self.theme.warning
                    } else {
                        self.theme.fg_muted
                    }),
                ),
                Span::styled("Page faults: ", self.theme.dim_style()),
                Span::styled(
                    format!("{} major", proc.major_faults),
                    if proc.major_faults > 1000 {
                        self.theme.warning_style()
                    } else {
                        Style::default().fg(self.theme.fg_dim)
                    },
                ),
            ]));
        }

        // Fragmentation indicator
        let frag_ratio = proc.fragmentation_ratio();
        if frag_ratio > 5.0 {
            lines.push(Line::from(vec![
                Span::styled(
                    "◐ ",
                    Style::default().fg(if frag_ratio > 15.0 {
                        self.theme.warning
                    } else {
                        self.theme.info
                    }),
                ),
                Span::styled("Fragmentation: ", self.theme.dim_style()),
                Span::styled(
                    format!("{:.0}x", frag_ratio),
                    if frag_ratio > 15.0 {
                        self.theme.warning_style()
                    } else {
                        Style::default().fg(self.theme.fg_dim)
                    },
                ),
                Span::styled(" VSS/RSS ratio", self.theme.muted_style()),
            ]));
        }

        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: true });
        paragraph.render(inner, buf);
    }
}

/// Create a section header with modern styling
fn section_header<'a>(label: &'a str, theme: &'a Theme) -> Line<'a> {
    Line::from(vec![Span::styled(
        label,
        Style::default()
            .fg(theme.primary)
            .add_modifier(Modifier::BOLD),
    )])
}

/// Create a memory row with mini bar
fn create_memory_row<'a>(
    label: &'a str,
    value: u64,
    max_value: u64,
    _desc: &'a str,
    theme: &'a Theme,
    is_primary: bool,
) -> Line<'a> {
    let bar_width = 12;
    let percent = if max_value > 0 {
        (value as f64 / max_value as f64) * 100.0
    } else {
        0.0
    };

    let bar = create_mini_bar(percent.min(100.0), bar_width);
    let color = if is_primary {
        theme.primary
    } else {
        theme.fg_dim
    };

    Line::from(vec![
        Span::styled(format!("{:<4}", label), Style::default().fg(color)),
        Span::styled(bar, Style::default().fg(color)),
        Span::raw(" "),
        Span::styled(
            format!("{:>9}", format_bytes(value)),
            if is_primary {
                Style::default().fg(theme.fg).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.fg_dim)
            },
        ),
    ])
}

/// Create a two-column stat row
fn create_two_col<'a>(left: (&'a str, u64), right: (&'a str, u64), theme: &'a Theme) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{:<9}", left.0), theme.muted_style()),
        Span::styled(
            format!("{:>9}", format_bytes(left.1)),
            Style::default().fg(theme.fg_dim),
        ),
        Span::styled("   ", Style::default()),
        Span::styled(format!("{:<9}", right.0), theme.muted_style()),
        Span::styled(
            format!("{:>9}", format_bytes(right.1)),
            Style::default().fg(theme.fg_dim),
        ),
    ])
}

/// Create a mini progress bar
fn create_mini_bar(percent: f64, width: usize) -> String {
    let chars = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];
    let total_eighths = ((percent / 100.0) * (width * 8) as f64).round() as usize;
    let full_blocks = total_eighths / 8;
    let partial = total_eighths % 8;

    let mut bar = "█".repeat(full_blocks.min(width));

    if partial > 0 && bar.chars().count() < width {
        bar.push(chars[partial]);
    }

    let remaining = width.saturating_sub(bar.chars().count());
    bar.push_str(&"░".repeat(remaining));

    bar
}

/// Stacked composition bar: one glyph run per segment, widths proportional
/// to RSS fractions. Colors distinguish types; the legend lines below the
/// bar carry the labels and byte values.
fn composition_bar(
    segments: &[crate::accounting::CompositionSegment],
    theme: &Theme,
) -> Vec<Span<'static>> {
    const WIDTH: usize = 24;
    let palette = [theme.primary, theme.secondary, theme.warning, theme.fg_dim];
    // Largest remainder: floors plus leftover cells to the largest
    // fractions, so rounded widths sum to exactly WIDTH.
    let mut widths: Vec<usize> = segments
        .iter()
        .map(|segment| (segment.fraction * WIDTH as f64).floor() as usize)
        .collect();
    let mut remainder = WIDTH.saturating_sub(widths.iter().sum());
    let mut order: Vec<usize> = (0..segments.len()).collect();
    order.sort_by(|&a, &b| {
        let frac = |index: usize| segments[index].fraction * WIDTH as f64 - widths[index] as f64;
        frac(b)
            .partial_cmp(&frac(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for index in order {
        if remainder == 0 {
            break;
        }
        widths[index] += 1;
        remainder -= 1;
    }
    let mut spans = vec![Span::styled("  ", Style::default())];
    for (index, width) in widths.iter().enumerate() {
        // No minimum: hairline fractions render as nothing in the bar but
        // stay listed in the legend below; the row sums to exactly WIDTH.
        spans.push(Span::styled(
            "█".repeat((*width).min(WIDTH)),
            Style::default().fg(palette[index % palette.len()]),
        ));
    }
    spans
}

/// State chip with icon
fn state_chip(state: char) -> String {
    match state {
        'R' => "● Running".to_string(),
        'S' => "○ Sleeping".to_string(),
        'D' => "◐ Disk Wait".to_string(),
        'Z' => "✕ Zombie".to_string(),
        'T' => "◼ Stopped".to_string(),
        't' => "◻ Tracing".to_string(),
        'I' => "◌ Idle".to_string(),
        _ => format!("? {}", state),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;

    fn buffer_text(buf: &Buffer) -> String {
        buf.content.iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn detail_panel_shows_composition_and_accounting() {
        let theme = Theme::dark();
        let process = test_support::process(100 * 1024 * 1024);
        let system = test_support::system_memory();
        let area = Rect::new(0, 0, 60, 40);
        let mut buf = Buffer::empty(area);
        DetailPanelWidget::new(Some(&process), &theme)
            .system(&system)
            .render(area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("Composition"));
        assert!(text.contains("private"));
        assert!(text.contains("Accounting"));
        assert!(text.contains("available"));
        assert!(text.contains("reclaimable") || text.contains("reclaimed"));
    }

    #[test]
    fn detail_panel_omits_swap_section_without_swap() {
        let theme = Theme::dark();
        let process = test_support::process(100 * 1024 * 1024);
        let mut system = test_support::system_memory();
        system.swap_total = 0;
        system.swap_used = 0;
        let area = Rect::new(0, 0, 60, 40);
        let mut buf = Buffer::empty(area);
        DetailPanelWidget::new(Some(&process), &theme)
            .system(&system)
            .render(area, &mut buf);
        let text = buffer_text(&buf);
        assert!(text.contains("not configured"));
    }

    #[test]
    fn detail_panel_survives_small_areas() {
        let theme = Theme::dark();
        let process = test_support::process(100 * 1024 * 1024);
        let system = test_support::system_memory();
        for (width, height) in [(20, 8), (30, 12), (60, 40)] {
            let area = Rect::new(0, 0, width, height);
            let mut buf = Buffer::empty(area);
            DetailPanelWidget::new(Some(&process), &theme)
                .system(&system)
                .render(area, &mut buf);
        }
    }
}
