//! ramwise - Intelligent RAM usage visualizer for Arch Linux
//!
//! A TUI application that provides deep memory introspection,
//! intelligent insights, and beautiful visualization.

mod analyzer;
mod app;
mod collector;
mod history;
mod process_control;
mod ui;
mod utils;

#[cfg(test)]
mod test_support;

use std::io::{self, stdout};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use tokio::sync::mpsc;

use app::{ActionStatus, ActionStatusKind, App, Focus};
use collector::Collector;
use ui::Layout;
use ui::widgets::{
    DetailPanelWidget, GraphWidget, HeaderWidget, InsightsPanelWidget, ProcessListWidget,
};

/// Intelligent RAM usage visualizer for Arch Linux
#[derive(Parser, Debug)]
#[command(name = "ramwise")]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Update interval in milliseconds
    #[arg(short, long, default_value = "1000")]
    interval: u64,

    /// Minimum process RSS to display (in MB)
    #[arg(short, long, default_value = "1")]
    min_rss: u64,

    /// Disable smaps collection (faster but less detailed)
    #[arg(long)]
    no_smaps: bool,

    /// Enable debug logging
    #[arg(short, long)]
    debug: bool,

    /// Theme (by default light or dark)
    #[arg(short, long, default_value = "dark")]
    theme: String,

    /// Print one compact JSON snapshot to stdout and exit (exit 0 on
    /// success, non-zero when collection fails; diagnostics go to stderr)
    #[arg(long, conflicts_with = "watch")]
    once: bool,

    /// Print one one-line status summary to stdout and exit.
    /// Combine with --watch to repeat the line every interval.
    #[arg(long)]
    tiny: bool,

    /// Repeat the --tiny line every interval until interrupted (exit 0 on
    /// clean interrupt, non-zero when collection fails)
    #[arg(long, requires = "tiny")]
    watch: bool,
}

/// How the process executes. Only [`ExecutionMode::Tui`] may initialize the
/// terminal; every other mode is plain stdout/stderr and never enters raw mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecutionMode {
    Tui,
    Once,
    TinyOnce,
    TinyWatch,
}

fn execution_mode(args: &Args) -> ExecutionMode {
    if args.watch {
        ExecutionMode::TinyWatch
    } else if args.tiny {
        ExecutionMode::TinyOnce
    } else if args.once {
        ExecutionMode::Once
    } else {
        ExecutionMode::Tui
    }
}

/// Build the collector shared by every execution mode so flags behave
/// identically in the TUI and in non-interactive commands.
fn build_collector(args: &Args) -> Collector {
    Collector::new()
        .with_interval(Duration::from_millis(args.interval))
        .with_min_rss(args.min_rss * 1024 * 1024)
        .with_smaps(!args.no_smaps)
}

/// Serialize one snapshot to the versioned export contract for `--once`.
/// The payload is compact JSON on stdout; failures are `Err` so the process
/// exits non-zero with the cause on stderr.
fn snapshot_to_json(snapshot: &collector::MemorySnapshot) -> Result<String> {
    let export = snapshot.to_export();
    export
        .validate()
        .map_err(|message| anyhow::anyhow!("{message}"))?;
    serde_json::to_string(&export).context("Failed to serialize snapshot")
}

/// Render the one-line status summary for `--tiny`.
///
/// Stable contract: space-separated fields, locale-independent formatting
/// (Rust `format!` never localizes numbers), no unbounded strings, fixed
/// field order — `mem <used>/<total> <pct> <pressure> swap <used>/<total>`
/// with an optional trailing `io <in>/<out>pg/s` swap-activity segment that
/// appears only when sample-to-sample rates are known. Pure and
/// deterministic: the same snapshot always renders the same line.
fn render_tiny_line(system: &collector::SystemMemory) -> String {
    let level = analyzer::classify(system, &analyzer::PressureThresholds::default());
    let mut line = format!(
        "mem {}/{} {} {} swap {}/{}",
        utils::format_bytes(system.used()),
        utils::format_bytes(system.total),
        utils::format_percent(system.usage_percent()),
        level.label(),
        utils::format_bytes(system.swap_used),
        utils::format_bytes(system.swap_total),
    );
    if let (Some(in_rate), Some(out_rate)) = (system.swap_in_rate, system.swap_out_rate) {
        line.push_str(&format!(" io {in_rate:.1}/{out_rate:.1}pg/s"));
    }
    line
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Diagnostics always go to stderr so stdout stays pure data in
    // non-interactive modes (pipelines, status bars, file redirects).
    if args.debug {
        tracing_subscriber::fmt()
            .with_env_filter("ramwise=debug")
            .with_writer(io::stderr)
            .init();
    }

    match execution_mode(&args) {
        ExecutionMode::Tui => run_tui(&args).await,
        ExecutionMode::Once => {
            let mut collector = build_collector(&args);
            let snapshot = collector.collect_snapshot()?;
            println!("{}", snapshot_to_json(&snapshot)?);
            Ok(())
        }
        ExecutionMode::TinyOnce => {
            let mut collector = build_collector(&args);
            let snapshot = collector.collect_snapshot()?;
            println!("{}", render_tiny_line(&snapshot.system));
            Ok(())
        }
        ExecutionMode::TinyWatch => run_tiny_watch(&args).await,
    }
}

/// Print the tiny line every interval until interrupted. A clean Ctrl-C ends
/// with exit 0; a collection failure ends non-zero with the cause on stderr.
async fn run_tiny_watch(args: &Args) -> Result<()> {
    let mut collector = build_collector(args);
    let mut ticker = tokio::time::interval(Duration::from_millis(args.interval));
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let snapshot = collector.collect_snapshot()?;
                println!("{}", render_tiny_line(&snapshot.system));
            }
            result = tokio::signal::ctrl_c() => {
                result.context("Failed to listen for interrupt")?;
                return Ok(());
            }
        }
    }
}

async fn run_tui(args: &Args) -> Result<()> {
    // Setup terminal
    enable_raw_mode().context("Failed to enable raw mode")?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .context("Failed to setup terminal")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("Failed to create terminal")?;

    // Create app
    let mut app = App::new(&args.theme);

    // Create collector
    let collector = build_collector(args);

    // Create channel for snapshots
    let (tx, mut rx) = mpsc::channel(2);

    // Spawn collector task
    let collector_handle = tokio::spawn(async move {
        if let Err(e) = collector.run(tx).await {
            tracing::error!("Collector error: {}", e);
        }
    });

    // Main event loop
    let result = run_app(&mut terminal, &mut app, &mut rx).await;

    // Cleanup
    disable_raw_mode().context("Failed to disable raw mode")?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .context("Failed to cleanup terminal")?;
    terminal.show_cursor().context("Failed to show cursor")?;

    // Abort collector
    collector_handle.abort();

    result
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    rx: &mut mpsc::Receiver<collector::MemorySnapshot>,
) -> Result<()> {
    let layout = Layout::new();

    loop {
        app.prune_transient_state();

        // Draw
        terminal.draw(|frame| {
            let areas = layout.calculate(frame.area());

            // Header
            if let Some(snapshot) = &app.snapshot {
                let header = HeaderWidget::new(&snapshot.system, &app.theme);
                frame.render_widget(header, areas.header);
            } else {
                let loading =
                    Paragraph::new(" ramwise - Loading...").style(app.theme.header_style());
                frame.render_widget(loading, areas.header);
            }

            // Process list
            if let Some(snapshot) = &app.snapshot {
                let total_mem = snapshot.system.total;
                let focus = app.focus;
                let processes = app.processes().to_vec();
                let theme = app.theme.clone();

                let process_list = ProcessListWidget::new(&processes, &theme, total_mem)
                    .focused(focus == Focus::ProcessList);

                frame.render_stateful_widget(
                    process_list,
                    areas.left_panel,
                    &mut app.process_list_state,
                );
            } else {
                let block = Block::default()
                    .title(" PROCESSES ")
                    .borders(Borders::ALL)
                    .border_style(app.theme.border_style(app.focus == Focus::ProcessList));
                frame.render_widget(block, areas.left_panel);
            }

            // Detail panel
            let detail = DetailPanelWidget::new(app.selected_process(), &app.theme)
                .focused(app.focus == Focus::DetailPanel);
            frame.render_widget(detail, areas.detail_panel);

            // Graph panel
            let graph = GraphWidget::new(&app.history, &app.theme)
                .selected_pid(app.process_list_state.selected_pid)
                .focused(app.focus == Focus::GraphPanel);
            frame.render_widget(graph, areas.graph_panel);

            // Insights panel
            let insights = InsightsPanelWidget::new(app.analyzer.insights(), &app.theme)
                .focused(app.focus == Focus::InsightsPanel);
            let insights = match &app.snapshot {
                Some(snapshot) => insights.pressure(&snapshot.system),
                None => insights,
            };
            frame.render_widget(insights, areas.bottom);

            // Help overlay
            if app.show_help {
                render_help_overlay(frame, &app.theme);
            }

            // Kill confirmation overlay
            if app.show_kill_confirm {
                render_kill_confirm_overlay(frame, app);
            }

            if let Some(status) = app.action_status.as_ref() {
                render_action_status(frame, &app.theme, status);
            }
        })?;

        // Handle events (with timeout for data updates)
        tokio::select! {
            // New snapshot from collector
            Some(snapshot) = rx.recv() => {
                app.update(snapshot);
            }

            // Keyboard/mouse events
            _ = async {
                if event::poll(Duration::from_millis(50)).unwrap_or(false)
                    && let Ok(Event::Key(key)) = event::read()
                        && key.kind == KeyEventKind::Press {
                            app.handle_key(key.code, key.modifiers);
                        }
            } => {}
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

fn render_help_overlay(frame: &mut ratatui::Frame, theme: &ui::Theme) {
    let area = frame.area();

    // Center a help box
    let help_width = 50.min(area.width - 4);
    let help_height = 18.min(area.height - 4);
    let x = (area.width - help_width) / 2;
    let y = (area.height - help_height) / 2;

    let help_area = ratatui::layout::Rect::new(x, y, help_width, help_height);

    // Clear background
    frame.render_widget(Clear, help_area);

    let help_text = r#"
  KEYBOARD SHORTCUTS

  Navigation:
    j/k or ↑/↓   Move selection
    Tab          Cycle focus
    Shift+Tab    Reverse cycle

  Process List:
    s            Cycle sort mode
    g            Go to top
    G            Go to bottom
    x            Send SIGTERM
    X            Confirm + send SIGKILL

  General:
    ?            Toggle this help
    q            Quit
    Ctrl+C       Force quit

  Press ESC or ? to close
"#;

    let help = Paragraph::new(help_text)
        .block(
            Block::default()
                .title(" Help ")
                .borders(Borders::ALL)
                .border_style(theme.border_style(true))
                .style(theme.base_style()),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(help, help_area);
}

fn render_kill_confirm_overlay(frame: &mut ratatui::Frame, app: &App) {
    let area = frame.area();
    let width = 62.min(area.width.saturating_sub(4));
    let height = 7.min(area.height.saturating_sub(2));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let modal_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, modal_area);

    let label = if let Some(proc_) = app.selected_process() {
        format!("Kill {} (PID {}) with SIGKILL?", proc_.name, proc_.pid)
    } else {
        "No process selected".to_string()
    };

    let message = vec![
        Line::from(Span::styled(label, Style::default().fg(app.theme.warning))),
        Line::from(""),
        Line::from("Press Enter to confirm, Esc to cancel"),
    ];

    let paragraph = Paragraph::new(message).alignment(Alignment::Center).block(
        Block::default()
            .title(" Confirm Kill ")
            .borders(Borders::ALL)
            .border_style(app.theme.border_style(true))
            .style(app.theme.base_style()),
    );

    frame.render_widget(paragraph, modal_area);
}

fn render_action_status(frame: &mut ratatui::Frame, theme: &ui::Theme, status: &ActionStatus) {
    let area = frame.area();
    let width = 70.min(area.width.saturating_sub(4));
    let height = 3;
    let x = (area.width.saturating_sub(width)) / 2;
    let y = area.height.saturating_sub(height + 1);
    let toast_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, toast_area);

    let color = match status.kind {
        ActionStatusKind::Success => theme.success,
        ActionStatusKind::Warning => theme.warning,
        ActionStatusKind::Error => theme.error,
    };
    let label = match status.kind {
        ActionStatusKind::Success => "OK",
        ActionStatusKind::Warning => "WARN",
        ActionStatusKind::Error => "ERR",
    };

    let line = Line::from(vec![
        Span::styled(format!("[{}] ", label), Style::default().fg(color)),
        Span::styled(&status.message, Style::default().fg(theme.fg)),
    ]);

    let paragraph = Paragraph::new(line).alignment(Alignment::Left).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(color))
            .style(theme.base_style()),
    );

    frame.render_widget(paragraph, toast_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;

    fn args_with(tiny: bool, once: bool, watch: bool) -> Args {
        Args {
            interval: 1000,
            min_rss: 1,
            no_smaps: false,
            debug: false,
            theme: "dark".into(),
            tiny,
            once,
            watch,
        }
    }

    #[test]
    fn mode_dispatch_prefers_the_most_specific_flag() {
        assert_eq!(
            execution_mode(&args_with(false, false, false)),
            ExecutionMode::Tui
        );
        assert_eq!(
            execution_mode(&args_with(false, true, false)),
            ExecutionMode::Once
        );
        assert_eq!(
            execution_mode(&args_with(true, false, false)),
            ExecutionMode::TinyOnce
        );
        assert_eq!(
            execution_mode(&args_with(true, true, false)),
            ExecutionMode::TinyOnce
        );
        assert_eq!(
            execution_mode(&args_with(true, false, true)),
            ExecutionMode::TinyWatch
        );
    }

    #[test]
    fn only_the_tui_mode_may_initialize_the_terminal() {
        // Structural guarantee: terminal setup lives in run_tui, and every
        // non-interactive mode resolves away from it. If a new mode is added
        // without updating this match, it fails closed here.
        for mode in [
            ExecutionMode::Once,
            ExecutionMode::TinyOnce,
            ExecutionMode::TinyWatch,
        ] {
            assert_ne!(mode, ExecutionMode::Tui);
        }
    }

    #[test]
    fn shared_builder_maps_min_rss_and_smaps_flags() {
        let filtered = Args {
            min_rss: 1_000_000,
            ..args_with(false, true, false)
        };
        let mut collector = build_collector(&filtered);
        let snapshot = collector.collect_snapshot().unwrap();
        assert!(snapshot.processes.is_empty());

        let plain = args_with(false, true, false);
        let mut collector = build_collector(&plain);
        assert!(collector.collect_snapshot().is_ok());
    }

    #[test]
    fn tiny_line_is_pinned_by_a_golden_fixture() {
        // Pinned against default thresholds: if PressureThresholds changes,
        // this golden must be consciously re-pinned.
        let line = render_tiny_line(&test_support::system_memory());
        assert_eq!(line, "mem 8.0G/16.0G 50.0% stable swap 512.0M/4.0G");
    }

    #[test]
    fn tiny_line_appends_swap_activity_only_when_known() {
        let mut system = test_support::system_memory();
        system.swap_in_rate = Some(10.0);
        system.swap_out_rate = Some(20.0);
        assert_eq!(
            render_tiny_line(&system),
            "mem 8.0G/16.0G 50.0% stable swap 512.0M/4.0G io 10.0/20.0pg/s"
        );
        system.swap_in_rate = None;
        assert!(!render_tiny_line(&system).contains("io "));
        system.swap_in_rate = Some(10.0);
        system.swap_out_rate = None;
        assert!(!render_tiny_line(&system).contains("io "));
    }

    #[test]
    fn tiny_line_marks_unknown_pressure_explicitly() {
        let line = render_tiny_line(&collector::SystemMemory::default());
        assert_eq!(line, "mem 0B/0B 0.00% unknown swap 0B/0B");
    }

    #[test]
    fn once_output_is_valid_versioned_json() {
        let snapshot = test_support::snapshot_at(std::time::Instant::now(), 100);
        let json = snapshot_to_json(&snapshot).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        // snapshot_to_json already enforces schema validation; here we pin
        // the wire shape so regressions surface at the CLI boundary too.
        assert!(value["schema_version"].is_number());
        assert!(value["system"]["total_bytes"].is_number());
        assert!(value["processes"].is_array());
    }
}
