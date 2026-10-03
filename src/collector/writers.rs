//! JSON and CSV snapshot writers.
//!
//! File layout contracts:
//!
//! - JSON: the [`ExportSnapshot`] contract serialized compactly. Round-trips
//!   through `serde_json` and [`ExportSnapshot::validate`].
//! - CSV: one `#` comment line (`ramwise-csv schema_version=N
//!   captured_at_unix_ms=M`), one `# system_<field>=<value>` comment per
//!   system metric, then a single stable process table. The column header is
//!   part of the contract: columns are only appended, and only together with
//!   the opt-in `--export-details` flag, which adds a trailing `regions_json`
//!   column. Without details, region data is omitted, never zeroed.
//!
//! Writers refuse to overwrite an existing file unless `force` is set, so an
//! export can never silently clobber a previous capture. `-` as a path means
//! stdout (used by `--once`-style pipelines); nothing else is created.

use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result};

use super::export::ExportSnapshot;

/// Stable process-table columns. Append-only; see module docs.
const CSV_COLUMNS: &[&str] = &[
    "pid",
    "name",
    "state",
    "ppid",
    "uid",
    "rss_bytes",
    "vss_bytes",
    "shared_bytes",
    "private_bytes",
    "pss_bytes",
    "uss_bytes",
    "swap_bytes",
    "heap_bytes",
    "stack_bytes",
    "libs_bytes",
    "anonymous_bytes",
    "file_mappings_bytes",
    "minor_faults",
    "major_faults",
    "cmdline",
];

/// Serialize one snapshot to versioned JSON text.
pub fn render_json(snapshot: &ExportSnapshot) -> Result<String> {
    snapshot
        .validate()
        .map_err(|message| anyhow::anyhow!("{message}"))?;
    serde_json::to_string(snapshot).context("Failed to serialize snapshot to JSON")
}

/// Render one snapshot to the documented CSV contract.
pub fn render_csv(snapshot: &ExportSnapshot, include_details: bool) -> Result<String> {
    snapshot
        .validate()
        .map_err(|message| anyhow::anyhow!("{message}"))?;
    let system = &snapshot.system;
    let mut out = String::new();
    writeln!(
        out,
        "# ramwise-csv schema_version={} captured_at_unix_ms={}",
        snapshot.schema_version, snapshot.captured_at_unix_ms
    )
    .unwrap();
    for (field, value) in [
        ("total_bytes", system.total_bytes),
        ("available_bytes", system.available_bytes),
        ("swap_total_bytes", system.swap_total_bytes),
        ("swap_used_bytes", system.swap_used_bytes),
    ] {
        writeln!(out, "# system_{field}={value}").unwrap();
    }
    let mut header = CSV_COLUMNS.join(",");
    if include_details {
        header.push_str(",regions_json");
    }
    writeln!(out, "{header}").unwrap();
    for process in &snapshot.processes {
        let mut row = vec![
            process.pid.to_string(),
            escape_csv(&process.name),
            escape_csv(&process.state.to_string()),
            process.ppid.to_string(),
            process.uid.to_string(),
            process.rss_bytes.to_string(),
            process.vss_bytes.to_string(),
            process.shared_bytes.to_string(),
            process.private_bytes.to_string(),
            process.pss_bytes.to_string(),
            process.uss_bytes.to_string(),
            process.swap_bytes.to_string(),
            process.heap_bytes.to_string(),
            process.stack_bytes.to_string(),
            process.libs_bytes.to_string(),
            process.anonymous_bytes.to_string(),
            process.file_mappings_bytes.to_string(),
            process.minor_faults.to_string(),
            process.major_faults.to_string(),
            escape_csv(&process.cmdline),
        ];
        if include_details {
            let regions = match &process.regions {
                Some(regions) => serde_json::to_string(regions).with_context(|| {
                    format!("failed to serialize regions for pid {}", process.pid)
                })?,
                None => String::new(),
            };
            row.push(escape_csv(&regions));
        }
        writeln!(out, "{}", row.join(",")).expect("writing to a String cannot fail");
    }
    Ok(out)
}

/// Minimal CSV escaping: quote when the field contains a comma, quote,
/// carriage return or newline, doubling embedded quotes.
fn escape_csv(field: &str) -> String {
    if field.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

/// Write `contents` to `path`, refusing to overwrite unless `force`.
/// A path of `-` writes to stdout instead of touching the filesystem.
///
/// Overwrite refusal is atomic: without `force` the file is created with
/// `create_new`, so a pre-existing file (or a symlink planted between a
/// check and the write) fails instead of being clobbered. With `force`
/// the file is truncated as requested. Symlinks themselves always resolve
/// — never point an export at a link you do not trust.
pub fn write_target(path: &Path, contents: &str, force: bool) -> Result<()> {
    if path.as_os_str() == "-" {
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(contents.as_bytes())
            .context("failed to write to stdout")?;
        stdout.flush().context("failed to flush stdout")?;
        return Ok(());
    }
    let mut options = OpenOptions::new();
    options.write(true);
    if force {
        options.create(true).truncate(true);
    } else {
        // Atomic: fails when the path already exists, no check-then-act race.
        options.create_new(true);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        // create_new failed because the file exists: keep the clear refusal
        // message instead of leaking the OS error.
        Err(_) if !force && path.exists() => {
            anyhow::bail!("refusing to overwrite existing file {}", path.display())
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to open {} for writing", path.display()));
        }
    };
    file.write_all(contents.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collector::ExportProcessMemory;

    fn snapshot_with_process() -> ExportSnapshot {
        let mut snapshot = ExportSnapshot::fixture();
        snapshot.processes = vec![ExportProcessMemory {
            pid: 7,
            name: "worker".into(),
            cmdline: "/bin/worker --flag=\"a,b\"\nsecond".into(),
            state: 'R',
            rss_bytes: 2048,
            ..Default::default()
        }];
        snapshot
    }

    #[test]
    fn json_round_trips_through_text() {
        let snapshot = snapshot_with_process();
        let text = render_json(&snapshot).unwrap();
        let decoded: ExportSnapshot = serde_json::from_str(&text).unwrap();
        assert_eq!(decoded, snapshot);
    }

    #[test]
    fn json_rejects_unsupported_schema_versions() {
        let mut snapshot = snapshot_with_process();
        snapshot.schema_version += 1;
        assert!(render_json(&snapshot).is_err());
    }

    #[test]
    fn csv_header_is_stable_and_comments_carry_metadata() {
        let snapshot = snapshot_with_process();
        let csv = render_csv(&snapshot, false).unwrap();
        let mut lines = csv.lines();
        let meta = lines.next().unwrap();
        assert!(meta.starts_with("# ramwise-csv schema_version=1 captured_at_unix_ms="));
        assert!(lines.any(|line| line.starts_with("# system_total_bytes=")));
        let header = csv.lines().find(|line| !line.starts_with('#')).unwrap();
        assert_eq!(header, CSV_COLUMNS.join(","));
    }

    #[test]
    fn csv_escapes_commas_quotes_and_newlines() {
        let csv = render_csv(&snapshot_with_process(), false).unwrap();
        assert!(csv.contains("\"/bin/worker --flag=\"\"a,b\"\"\nsecond\""));
    }

    #[test]
    fn csv_escapes_carriage_returns_and_state_fields() {
        assert_eq!(escape_csv("a\rb"), "\"a\rb\"");
        let mut snapshot = snapshot_with_process();
        snapshot.processes[0].state = ',';
        let csv = render_csv(&snapshot, false).unwrap();
        assert!(csv.contains("\",\","));
    }

    #[test]
    fn details_flag_appends_regions_column_only() {
        let snapshot = snapshot_with_process();
        let plain = render_csv(&snapshot, false).unwrap();
        let detailed = render_csv(&snapshot, true).unwrap();
        let plain_header = plain.lines().find(|line| !line.starts_with('#')).unwrap();
        let detailed_header = detailed
            .lines()
            .find(|line| !line.starts_with('#'))
            .unwrap();
        assert_eq!(detailed_header, format!("{plain_header},regions_json"));
    }

    #[test]
    fn writer_refuses_to_overwrite_without_force() {
        let dir = std::env::temp_dir().join(format!(
            "ramwise-export-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("snap.json");
        std::fs::write(&path, "old").unwrap();
        assert!(write_target(&path, "new", false).is_err());
        write_target(&path, "new", true).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn writer_reports_unwritable_paths_clearly() {
        let path = Path::new("/nonexistent-ramwise-dir/sub/snap.json");
        let err = write_target(path, "data", true).unwrap_err();
        assert!(err.to_string().contains("nonexistent-ramwise-dir"));
    }
}
