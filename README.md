# ramwise

> Your memory's wise advisor - Intelligent RAM usage visualizer for Arch Linux

![License](https://img.shields.io/badge/license-MIT-blue.svg)
![Rust](https://img.shields.io/badge/rust-1.70+-orange.svg)

**ramwise** is a terminal-based RAM usage visualizer that goes beyond basic memory monitoring. It provides deep memory introspection, intelligent leak detection, and beautiful visualization all in a lightweight TUI application.

> **Unreleased roadmap surface.** This README documents the complete CLI
> across the open roadmap PRs (#33–#50); features land progressively as
> those merge. Anything already on `main` works today; the rest is marked
> by its PR and verified against that branch (see CHANGELOG).

## Features

- **Deep Memory Introspection** - See RSS, PSS, USS, shared/private breakdown per process
- **Intelligent Insights** - Rule-based analysis detects memory leaks, hogs, and anomalies
- **Calibrated Leak Score** - 0–100 explainable score (growth, monotonicity, stability, coverage)
- **Memory-Pressure Levels** - Documented stable/elevated/critical classification with unknown states
- **Non-interactive Modes** - `--once` JSON, `--tiny` status line, `--watch` streaming, file exports
- **Snapshot Compare** - Diff two captures with PID-reuse safety
- **Categories, Filters & Tree** - Heuristic app labels, typed filters, ppid-tree navigation
- **Calm Mode & Alerts** - Load shedding with cooldown/dedup/dry-run notifications
- **External Tools** - Confirmed htop/strace/valgrind launches without shell interpolation
- **Beautiful TUI** - Modern interface with graphs, sparklines, colors, and intuitive navigation
- **Minimal Footprint** - Written in Rust for maximum efficiency
- **Real-time Updates** - Live monitoring with configurable refresh rate
- **Process Control** - Stop (`SIGTERM`) or kill (`SIGKILL`) selected processes directly from the TUI

### Snapshot exports

The collector exposes a versioned `ExportSnapshot` contract (schema 1) for
integrations and export formats. It uses wall-clock Unix milliseconds,
explicit byte/count units, collector metadata, and capability markers.
Runtime-only monotonic `Instant` values are intentionally not serialized;
unavailable features remain explicit in the exported capability map
(`processes`, `smaps_rollup`, `regions`, `swap_rates`, `pressure`).

```bash
# One JSON snapshot to stdout (pipes and scripts)
ramwise --once

# Versioned JSON / CSV to files (`-` streams to stdout instead).
# Existing files are never overwritten unless --force is given.
ramwise --export-json snap.json --export-csv snap.csv

# Include expensive per-region details (adds a regions_json CSV column)
ramwise --export-json full.json --export-details
```

The CSV contract starts with `# ramwise-csv schema_version=1
captured_at_unix_ms=…` metadata comments, `# system_*` totals, then a
stable append-only process table (`pid,name,state,…,cmdline`).

### Comparing snapshots

```bash
# Capture two points in time, then diff them (human, json or csv output)
ramwise --export-json before.json
# ... run the workload ...
ramwise --export-json after.json
ramwise --compare before.json after.json
ramwise --compare before.json after.json --compare-format csv
# Only report processes moving at least 50 MB RSS
ramwise --compare before.json after.json --compare-min-delta-mb 50
```

Processes match by PID **plus** start time, so PID reuse reports as a
removal plus an addition (with a warning), never a silent match. Different
schema versions fail before any comparison.

## Screenshots

![Screenshot](public/screenshot.png)

## Installation

### From Source

```bash
# Clone the repository
git clone https://github.com/Duckaet/ramwise
cd ramwise

# Build release binary
cargo build --release

# Install (optional)
sudo cp target/release/ramwise /usr/local/bin/
```

## Usage

```bash
# Run with default settings (1s refresh, 1MB minimum RSS)
ramwise

# Custom refresh interval (500ms)
ramwise --interval 500

# Show all processes (including small ones)
ramwise --min-rss 0

# Disable smaps collection (faster but less detailed)
ramwise --no-smaps

# Enable debug logging
ramwise --debug

# Use light mode
ramwise -t light

# Show only processes with no shared memory / with shared memory
ramwise --only-private
ramwise --only-shared

# Only processes with at least 100 MB PSS
ramwise --min-pss 100

# Start calm (trend rendering paused); dry-run alert delivery
ramwise --calm --alert-dry-run
```

Example tiny output (stable, locale-independent fields):

```text
mem 8.0G/16.0G 50.0% stable swap 512.0M/4.0G io 10.0/20.0pg/s
```

The trailing `io` segment appears only once swap-activity rates are known
(from the second sample on); pressure reads `stable`, `elevated`,
`critical`, or `unknown` when inputs are unavailable.

## Status bar integration

```ini
# Waybar (custom module, polling every 5s)
"custom/ramwise": {
    "exec": "ramwise --tiny",
    "interval": 5,
    "format": "{}"
}
```

```tmux
# tmux status-right (polling every 5s)
set -g status-interval 5
set -g status-right "#(ramwise --tiny)"
```

```ini
# Polybar (custom script module)
[module/ramwise]
type = custom/script
exec = ramwise --tiny
interval = 5
```

## Pressure semantics

Memory pressure classifies available RAM plus swap usage/activity into
`stable`, `elevated`, `critical`, or `unknown` (no usable input — rendered
dim, never as zero). Defaults: elevated at ≥75% used, ≥50% swap fill, or
sustained swap-out activity; critical at ≥90% used, ≥80% swap fill, or heavy
swap-out. Swap-less machines classify on RAM only. The header shows the
level; the insights panel explains used-versus-available (reclaimable cache
counts as available, not pressure).

## Categories, filters and tree

- Processes classify heuristically (browser, electron, dev, service,
  system, terminal, media) with per-binary overrides; `s` cycles sort modes
  including a grouped Category view, and the detail panel labels each
  process.
- `--only-private`, `--only-shared` and `--min-pss <MB>` filter before
  sorting; `f` cycles the memory filter and `F` the PSS floor in the TUI.
- `t` toggles ppid-tree preorder with indentation; missing parents become
  roots and selection stays PID-stable.

## Alerts and calm mode

Warning/Critical insights dispatch through cooldown (default 60s,
`--alert-cooldown-secs`), deduplication, and `--alert-dry-run` previews to
`--alert-sink log|stderr`. Critical pressure auto-engages calm mode, which
pauses trend rendering (`c` toggles, `--calm` starts) while alert dispatch
keeps running — monitoring degrades visibly, never silently.

## External tools

`H` (htop), `R` (strace) and `V` (valgrind relaunch) validate the selected
process, resolve the binary via `PATH`, and ask for explicit confirmation
(`Enter` runs, `Esc` cancels). Launches use direct argv spawning — process
data never passes through a shell. Missing tools and permission failures
report through the status line; fullscreen tools suspend and resume the TUI.

## Diagnostics

```bash
# eBPF allocation-tracing capabilities (backends, kernel, privilege)
ramwise --trace-alloc

# GPU VRAM provider capabilities (NVIDIA/AMD/Intel tool presence)
ramwise --vram
```

Both are standalone read-only diagnostics; unsupported systems print the
gap and normal monitoring is unaffected.

## Leak investigations

1. Watch the leak score (`Likely`/`Severe` insights explain growth,
   monotonicity, stability and coverage) and the trend sparkline.
2. Capture `ramwise --export-json before.json`, reproduce, capture
   `after.json`, then `ramwise --compare before.json after.json`.
3. Drill into the mover: category, composition bar (private/shared/swap),
   page faults and command line in the detail panel.
4. Confirm with `R` (strace) or relaunch under valgrind (`V`).

## Permissions and kernel requirements

- Reading other users' processes needs matching UID or root; permission
  failures surface as capability gaps and explicit errors, never silent
  zeros. `smaps_rollup` (PSS/USS) requires read access to the target.
- Kernel 2.6.28+ for `smaps_rollup`; 5.8+ for native eBPF ring buffers;
  `/proc/pressure/*` for PSI metrics (older kernels report `unknown`).
- eBPF program loading needs root or unprivileged BPF; tracing a PID needs
  its start time (unknown start times refuse process-scoped tracing).

## Capability limits and accounting

- Unavailable metrics are omitted, never zeroed: no smaps means no
  PSS/USS segments; machines without swap say "not configured" in the
  accounting notes (the `--tiny` line still prints its fixed `swap 0B/0B`
  segment, and the header drops the swap segment instead).
- Page cache is reclaimable and counts as available — it is not pressure
  and not a leak. Slab is mostly unreclaimable kernel memory. `shmem`/tmpfs
  is counted in processes too. Summed process RSS overcounts shared memory;
  PSS apportions it.
- Optional dependencies: `htop`, `strace`, `valgrind` (actions),
  `bcc-trace`/`bpftrace` (tracing helpers), `nvidia-smi`/`rocm-smi`/
  `intel_gpu_top` (VRAM). Everything is detected at use time; nothing is
  required to run ramwise.

## Schema migrations

Snapshot schema 1 is current. Additive optional fields do not bump the
version; breaking changes will bump it, ship a migration note here, and
keep the previous version readable for one release. Exports record their
`schema_version`; `--compare` refuses mixed versions.

## Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `j/k` or `↑/↓` | Navigate process list |
| `Tab` | Cycle focus between panels |
| `Shift+Tab` | Reverse cycle focus |
| `s` | Cycle sort mode (RSS/PSS/Private/Name/PID/Category) |
| `f` / `F` | Cycle memory filter / PSS floor |
| `t` | Toggle process tree |
| `c` | Toggle calm mode |
| `g` | Go to top of list |
| `G` | Go to bottom of list |
| `H` | Inspect selected process in htop |
| `R` | Trace selected process syscalls (strace) |
| `V` | Relaunch selected binary under valgrind |
| `x` | Send `SIGTERM` to selected process |
| `X` | Confirm and send `SIGKILL` to selected process |
| `?` | Toggle help overlay |
| `q` | Quit |

Notes:
- `SIGKILL` and external tools require confirmation in-app (`Enter` confirm, `Esc` cancel).
- Process control follows OS permissions; root-owned processes may return permission errors.

## Process Control

ramwise now supports direct process signaling from the process list:

1. Select a process with `j/k` or `↑/↓`.
2. Press `x` to send `SIGTERM` (graceful stop).
3. Press `X` to open kill confirmation, then:
   - `Enter` to send `SIGKILL`
   - `Esc` to cancel

Action results are shown as in-app status messages (success, warning, or error).

## Configuration

### Themes
ramwise includes built-in themes selectable via `--theme` or `-t`:
```bash
# Launch with dark theme (default)
ramwise -t dark

# Launch with light theme (Gruvbox Light)
ramwise -t light
```

Custom themes can be added to `src/ui/theme.rs` and registered in `App::new` (`src/app.rs`).

### Layout Customization
Layout dimensions and panel splits can be customized in `src/ui/layout.rs` (`Layout::new`):
- `header_height`: Height of the top status bar.
- `center_height`: Minimum height of the main process/detail panels.
- `bottom_height`: Height of the insights panel.
- `left_width_percent`: Width percentage allocated to the process list.
- `side_vertical_split_percent`: Height percentage allocated to process details vs memory graph.
- `invert_horizontal_split`: Swap process list and side panels.
- `invert_side_vertical_split`: Swap detail view and trend graph.
- `put_insights_on_top`: Place the insights panel below the header instead of at the bottom.


## Insight Rules

ramwise includes intelligent analysis rules:

| Rule | Severity | Description |
|------|----------|-------------|
| Memory Leak Detector | Warning/Critical | Detects consistent RSS growth patterns |
| Leak Score | Warning/Critical | Calibrated 0–100 score with components (Likely/Severe bands) |
| Memory Hog | Warning | Flags processes using >30% of total RAM |
| Sudden Spike | Warning | Alerts on rapid memory increases (>100MB in 10s) |
| OOM Risk | Critical | Warns when system is at risk of OOM |
| Swap Pressure | Warning | Detects excessive swap usage |
| Fragmentation | Info | Identifies high VSS/RSS ratios |
| Cache Info | Info | Explains high page cache usage |

## Architecture

```
ramwise/
├── src/
│   ├── main.rs              # Entry point, CLI modes, async event loop
│   ├── app.rs               # Application state
│   ├── collector/           # Memory data collection from /proc (+ exports)
│   ├── analyzer/            # Rule engine, pressure levels, leak score
│   ├── alerts.rs            # Alert dispatch, cooldown, calm mode
│   ├── accounting.rs        # Memory-type composition and notes
│   ├── categories.rs        # Heuristic process classification
│   ├── process_view.rs      # Filters and ppid-tree ordering
│   ├── compare.rs           # Snapshot diffing
│   ├── external_tools.rs    # Validated htop/strace/valgrind launches
│   ├── tracer.rs            # eBPF tracing boundary and capabilities
│   ├── vram.rs              # GPU VRAM provider boundary
│   ├── history/             # Time-series buffer, sparklines
│   ├── ui/                  # Ratatui TUI components
│   └── utils/               # Formatting utilities
```

## Requirements

- Linux kernel 4.14+ (for `/proc/[pid]/smaps_rollup`), 5.8+ for native eBPF ring buffers
- Terminal with color support

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## License

MIT License - see [LICENSE](LICENSE) for details.

## Credits

Built with:
- [Ratatui](https://github.com/ratatui/ratatui) - TUI framework
- [procfs](https://github.com/eminence/procfs) - /proc filesystem parser
- [Tokio](https://tokio.rs/) - Async runtime
