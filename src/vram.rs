//! Provider-based VRAM metrics boundary.
//!
//! GPU memory arrives through vendor tools, not /proc, so every backend
//! hides behind [`VramProvider`]: identity, capability detection, and a
//! sample that is either real data or an explicit failure. The core
//! collector never touches this module, so ramwise works identically with
//! no drivers, no tools, and no GPU. [`MockVramProvider`] pins the contract
//! for tests; [`NvidiaSmiProvider`] proves the seam against real tooling,
//! while AMD and Intel report detected-but-unsupported until their vendor
//! sampling lands.

#![allow(dead_code)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// GPU vendor backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VramBackend {
    Nvidia,
    Amd,
    Intel,
}

impl VramBackend {
    pub fn name(self) -> &'static str {
        match self {
            Self::Nvidia => "nvidia",
            Self::Amd => "amd",
            Self::Intel => "intel",
        }
    }

    /// Vendor CLI this backend shells out to, if any.
    pub fn tool_binary(self) -> Option<&'static str> {
        match self {
            Self::Nvidia => Some("nvidia-smi"),
            Self::Amd => Some("rocm-smi"),
            Self::Intel => Some("intel_gpu_top"),
        }
    }
}

/// One VRAM sample. `available` is false exactly when the data is missing,
/// so consumers never mistake a gap for an idle GPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VramSample {
    pub backend: VramBackend,
    pub device: String,
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available: bool,
    pub reason: String,
}

/// Explicit provider failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VramError {
    ToolMissing(&'static str),
    ToolFailed(String),
    Unsupported(String),
}

impl std::fmt::Display for VramError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ToolMissing(binary) => write!(formatter, "{binary} not on PATH"),
            Self::ToolFailed(reason) => write!(formatter, "vendor tool failed: {reason}"),
            Self::Unsupported(reason) => write!(formatter, "unsupported: {reason}"),
        }
    }
}

impl std::error::Error for VramError {}

/// Capability and sampling contract every backend implements.
///
/// Error channels are deliberate: `Err` means the provider failed
/// (missing tool, failed run, unimplemented backend), while
/// `Ok` with `available: false` means it sampled successfully but the
/// data is missing (no device, unknown values) — the sample carries the
/// reason either way.
pub trait VramProvider: Send + Sync {
    fn backend(&self) -> VramBackend;
    /// Display name: the vendor binary, or `mock-<backend>` for tests.
    /// Owned because mock and vendor display names are composed dynamically.
    fn name(&self) -> String;
    /// Detect without sampling: tool presence only, not driver sanity
    /// (proving the driver would require running the tool).
    fn detect(&self) -> Result<(), VramError>;
    /// Take one sample; failures stay explicit.
    fn sample(&self) -> Result<VramSample, VramError>;
}

/// PATH lookup shared by vendor providers. `path_var` injects fixtures.
/// Entries must be executable files: a same-named non-executable never
/// counts, so a stray text file cannot fake a backend.
pub fn tool_path(binary: &str, path_var: Option<OsString>) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path_var = path_var?;
    std::env::split_paths(&path_var)
        .filter(|dir| !dir.as_os_str().is_empty())
        .map(|dir| dir.join(binary))
        .find(|candidate| {
            std::fs::metadata(candidate)
                .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        })
}

/// NVIDIA sampling through `nvidia-smi`. The only backend that shells out
/// today; AMD and Intel reuse the trait with detected-but-unsupported
/// sampling until their parsers land.
pub struct NvidiaSmiProvider {
    path_var: Option<OsString>,
}

impl NvidiaSmiProvider {
    pub fn new(path_var: Option<OsString>) -> Self {
        Self { path_var }
    }

    fn tool(&self) -> Result<PathBuf, VramError> {
        tool_path("nvidia-smi", self.path_var.clone()).ok_or(VramError::ToolMissing("nvidia-smi"))
    }
}

impl VramProvider for NvidiaSmiProvider {
    fn backend(&self) -> VramBackend {
        VramBackend::Nvidia
    }

    fn name(&self) -> String {
        "nvidia-smi".to_string()
    }

    fn detect(&self) -> Result<(), VramError> {
        self.tool().map(|_| ())
    }

    fn sample(&self) -> Result<VramSample, VramError> {
        let tool = self.tool()?;
        let output = run_with_timeout(
            &tool,
            &[
                "--query-gpu=name,memory.total,memory.used",
                "--format=csv,noheader,nounits",
            ],
            Duration::from_secs(5),
        )
        .map_err(|error| VramError::ToolFailed(error.to_string()))?;
        if !output.status.success() {
            return Err(VramError::ToolFailed(format!(
                "exit {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let rows = parse_nvidia_smi_csv(&text);
        if rows.is_empty() {
            return Err(VramError::ToolFailed("unparseable output".into()));
        }
        let (device, total_mib, used_mib) = if rows.len() == 1 {
            rows.into_iter().next().expect("checked non-empty")
        } else {
            let count = rows.len();
            let (total, used) =
                rows.into_iter()
                    .fold((0u64, 0u64), |(total, used), (_, row_total, row_used)| {
                        (
                            total.saturating_add(row_total),
                            used.saturating_add(row_used),
                        )
                    });
            (format!("{count} GPUs"), total, used)
        };
        Ok(VramSample {
            backend: VramBackend::Nvidia,
            device,
            total_bytes: total_mib.saturating_mul(1024 * 1024),
            used_bytes: used_mib.saturating_mul(1024 * 1024),
            available: true,
            reason: String::new(),
        })
    }
}

/// Run a vendor tool with a timeout: hung drivers must fail the sample,
/// never the caller. Kills the child past the deadline.
fn run_with_timeout(
    tool: &Path,
    args: &[&str],
    timeout: Duration,
) -> std::io::Result<std::process::Output> {
    use std::process::{Command, Stdio};
    use std::time::Instant;
    let mut child = Command::new(tool)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let start = Instant::now();
    loop {
        match child.try_wait()? {
            Some(_) => return child.wait_with_output(),
            None if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("{} timed out after {timeout:?}", tool.display()),
                ));
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

/// Parse every `name, total MiB, used MiB` CSV row, tolerating quoted
/// commas, blank lines, header rows and `MiB` suffixes. Unparseable lines
/// are skipped; an empty result means nothing usable was reported.
pub fn parse_nvidia_smi_csv(text: &str) -> Vec<(String, u64, u64)> {
    text.lines().filter_map(parse_nvidia_row).collect()
}

fn parse_nvidia_row(line: &str) -> Option<(String, u64, u64)> {
    if line.trim().is_empty() {
        return None;
    }
    let mut parts = split_csv_line(line);
    if parts.len() < 3 {
        return None;
    }
    let used = parse_mib(parts.pop()?)?;
    let total = parse_mib(parts.pop()?)?;
    let device = parts.join(", ");
    if device.is_empty() {
        return None;
    }
    Some((device, total, used))
}

fn parse_mib(raw: String) -> Option<u64> {
    raw.trim()
        .strip_suffix("MiB")
        .unwrap_or(raw.trim())
        .trim()
        .parse()
        .ok()
}

/// One CSV line split honoring double-quoted fields.
fn split_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(cell) = chars.next() {
        match cell {
            '"' if in_quotes => {
                if chars.peek() == Some(&'"') {
                    current.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            }
            '"' => in_quotes = true,
            ',' if !in_quotes => fields.push(std::mem::take(&mut current)),
            _ => current.push(cell),
        }
    }
    fields.push(current);
    fields
}

/// Detected-but-unsupported vendor backend: presence is reported, sampling
/// stays an explicit error until the vendor parser lands.
pub struct UnsupportedVendorProvider {
    backend: VramBackend,
    path_var: Option<OsString>,
}

impl UnsupportedVendorProvider {
    pub fn new(backend: VramBackend, path_var: Option<OsString>) -> Self {
        Self { backend, path_var }
    }
}

impl VramProvider for UnsupportedVendorProvider {
    fn backend(&self) -> VramBackend {
        self.backend
    }

    fn name(&self) -> String {
        self.backend.tool_binary().unwrap_or("unknown").to_string()
    }

    fn detect(&self) -> Result<(), VramError> {
        let binary = self.backend.tool_binary().ok_or_else(|| {
            VramError::Unsupported(format!("{} sampling not implemented", self.backend.name()))
        })?;
        tool_path(binary, self.path_var.clone())
            .map(|_| ())
            .ok_or(VramError::ToolMissing(binary))
    }

    fn sample(&self) -> Result<VramSample, VramError> {
        Err(VramError::Unsupported(format!(
            "{} sampling not yet implemented",
            self.backend.name()
        )))
    }
}

/// Scripted provider pinning the consumer contract in tests.
pub struct MockVramProvider {
    pub backend: VramBackend,
    pub samples: Vec<Result<VramSample, VramError>>,
    pub calls: std::sync::atomic::AtomicUsize,
    /// Mirrors real detection: a failing detect means the tool is missing.
    pub detect_result: Result<(), VramError>,
}

impl MockVramProvider {
    pub fn available_sample(backend: VramBackend) -> VramSample {
        VramSample {
            backend,
            device: "Mock GPU".to_string(),
            total_bytes: 8 * 1024 * 1024 * 1024,
            used_bytes: 2 * 1024 * 1024 * 1024,
            available: true,
            reason: String::new(),
        }
    }

    pub fn unavailable_sample(backend: VramBackend, reason: &str) -> VramSample {
        VramSample {
            backend,
            device: String::new(),
            total_bytes: 0,
            used_bytes: 0,
            available: false,
            reason: reason.to_string(),
        }
    }
}

impl VramProvider for MockVramProvider {
    fn backend(&self) -> VramBackend {
        self.backend
    }

    fn name(&self) -> String {
        format!("mock-{}", self.backend.name())
    }

    fn detect(&self) -> Result<(), VramError> {
        self.detect_result.clone()
    }

    fn sample(&self) -> Result<VramSample, VramError> {
        let call = self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.samples.get(call).cloned().unwrap_or_else(|| {
            // Fail closed: an over-called script is a test bug, and silent
            // default data would hide it.
            Err(VramError::ToolFailed("mock script exhausted".into()))
        })
    }
}

/// Detect every backend against one PATH. Vendor implementations stay in
/// this module, far from the core collector.
pub fn detect_providers(path_var: Option<OsString>) -> Vec<Box<dyn VramProvider>> {
    vec![
        Box::new(NvidiaSmiProvider::new(path_var.clone())),
        Box::new(UnsupportedVendorProvider::new(
            VramBackend::Amd,
            path_var.clone(),
        )),
        Box::new(UnsupportedVendorProvider::new(VramBackend::Intel, path_var)),
    ]
}

/// One-line capability report for diagnostics.
pub fn report(path_var: Option<OsString>) -> String {
    detect_providers(path_var)
        .iter()
        .map(|provider| match provider.detect() {
            Ok(()) => format!("{}: tool present", provider.backend().name()),
            Err(error) => format!("{}: {error}", provider.backend().name()),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_path(tools: &[&str]) -> (PathBuf, OsString) {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ramwise-vram-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for tool in tools {
            let path = dir.join(tool);
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&path, permissions).unwrap();
        }
        let path_var = dir.clone().into_os_string();
        (dir, path_var)
    }

    fn cleanup(dir: &PathBuf) {
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn nvidia_csv_parses_all_device_rows() {
        let rows = parse_nvidia_smi_csv(
            "NVIDIA GeForce RTX 4070, 12282, 1843 \n\"Tesla, V100\", 16384 MiB, 0 MiB\n",
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "NVIDIA GeForce RTX 4070");
        assert_eq!((rows[0].1, rows[0].2), (12282, 1843));
        assert_eq!(rows[1].0, "Tesla, V100");
        assert_eq!((rows[1].1, rows[1].2), (16384, 0));
    }

    #[test]
    fn nvidia_csv_rejects_malformed_output() {
        assert!(parse_nvidia_smi_csv("").is_empty());
        assert!(parse_nvidia_smi_csv("only, two\n").is_empty());
        assert!(parse_nvidia_smi_csv("GPU, lots, nope\n").is_empty());
        assert!(parse_nvidia_smi_csv(", 12282, 1843\n").is_empty());
        // Header rows and blanks are skipped, not fatal.
        assert_eq!(
            parse_nvidia_smi_csv("name, memory.total, memory.used\nGPU0, 100, 10\n").len(),
            1
        );
    }

    #[test]
    fn missing_tool_is_an_explicit_detection_failure() {
        let (dir, path) = fixture_path(&[]);
        let provider = NvidiaSmiProvider::new(Some(path));
        assert_eq!(provider.detect(), Err(VramError::ToolMissing("nvidia-smi")));
        assert_eq!(provider.sample(), Err(VramError::ToolMissing("nvidia-smi")));
        cleanup(&dir);
    }

    #[test]
    fn unsupported_vendors_report_presence_but_no_samples() {
        let (dir, path) = fixture_path(&["rocm-smi"]);
        let present = UnsupportedVendorProvider::new(VramBackend::Amd, Some(path));
        assert!(present.detect().is_ok());
        assert!(matches!(present.sample(), Err(VramError::Unsupported(_))));
        cleanup(&dir);
        let (dir, path) = fixture_path(&[]);
        let absent = UnsupportedVendorProvider::new(VramBackend::Intel, Some(path));
        assert_eq!(
            absent.detect(),
            Err(VramError::ToolMissing("intel_gpu_top"))
        );
        cleanup(&dir);
    }

    #[test]
    fn fake_smi_script_drives_a_full_sample() {
        let (dir, _) = fixture_path(&[]);
        std::fs::write(
            dir.join("nvidia-smi"),
            "#!/bin/sh\necho 'Mock GPU, 8192, 2048'\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("nvidia-smi");
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).unwrap();
        let provider = NvidiaSmiProvider::new(Some(dir.clone().into_os_string()));
        assert!(provider.detect().is_ok());
        let sample = provider.sample().unwrap();
        assert_eq!(sample.device, "Mock GPU");
        assert_eq!(sample.total_bytes, 8192 * 1024 * 1024);
        assert_eq!(sample.used_bytes, 2048 * 1024 * 1024);
        assert!(sample.available);
        cleanup(&dir);
    }

    #[test]
    fn failing_smi_script_is_an_explicit_sample_error() {
        let (dir, _) = fixture_path(&[]);
        std::fs::write(dir.join("nvidia-smi"), "#!/bin/sh\nexit 3\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("nvidia-smi");
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).unwrap();
        let provider = NvidiaSmiProvider::new(Some(dir.clone().into_os_string()));
        assert!(matches!(provider.sample(), Err(VramError::ToolFailed(_))));
        cleanup(&dir);
    }

    #[test]
    fn mock_provider_pins_the_consumer_contract() {
        let provider = MockVramProvider {
            backend: VramBackend::Nvidia,
            samples: vec![
                Ok(MockVramProvider::available_sample(VramBackend::Nvidia)),
                Err(VramError::ToolFailed("driver reset".into())),
            ],
            calls: std::sync::atomic::AtomicUsize::new(0),
            detect_result: Ok(()),
        };
        assert_eq!(provider.backend(), VramBackend::Nvidia);
        assert_eq!(provider.name(), "mock-nvidia");
        assert!(provider.detect().is_ok());
        let first = provider.sample().unwrap();
        assert!(first.available);
        assert_eq!(first.used_bytes, 2 * 1024 * 1024 * 1024);
        // Provider failures stay explicit; consumers must handle Err.
        assert_eq!(
            provider.sample(),
            Err(VramError::ToolFailed("driver reset".into()))
        );
        // Exhausted scripts fail closed instead of inventing data.
        assert!(matches!(provider.sample(), Err(VramError::ToolFailed(_))));
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::Relaxed), 3);

        let failing = MockVramProvider {
            backend: VramBackend::Amd,
            samples: vec![],
            calls: std::sync::atomic::AtomicUsize::new(0),
            detect_result: Err(VramError::ToolMissing("rocm-smi")),
        };
        assert_eq!(failing.detect(), Err(VramError::ToolMissing("rocm-smi")));
    }

    #[test]
    fn unavailable_samples_carry_reasons_not_zeros() {
        let sample = MockVramProvider::unavailable_sample(VramBackend::Amd, "no driver");
        assert!(!sample.available);
        assert!(!sample.reason.is_empty());
    }

    #[test]
    fn registry_reports_every_backend() {
        let (dir, path) = fixture_path(&[]);
        let text = report(Some(path));
        assert!(text.contains("nvidia:"));
        assert!(text.contains("amd:"));
        assert!(text.contains("intel:"));
        cleanup(&dir);
    }
}
