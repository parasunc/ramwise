//! Optional eBPF allocation-tracing boundary.
//!
//! ramwise does not collect eBPF data today. This module is the honest
//! boundary where such an integration would plug in: capability detection
//! (backend tools, kernel version, privilege, tracefs), an explicit
//! unavailable state with reasons, and a tracer lifecycle with teardown
//! guarantees and bounded buffers. Nothing here fabricates allocation data;
//! when capabilities are missing, callers get a reason, and normal
//! monitoring continues unaffected.
//!
//! Validation matrix (what each check gates):
//!
//! | Check | Gates | Missing means |
//! |---|---|---|
//! | bcc / bpftrace on PATH | `TBackend::Bcc` / `Bpftrace` | no helper backend |
//! | kernel >= 5.8 | `TBackend::Native` (ring buffers) | native path unavailable |
//! | tracefs mounted and listable | attaching any probe | no attach without it |
//! | euid 0 or unprivileged BPF allowed | loading programs | privileged operation denied |
//! | target process identity (pid + starttime) | correlating events | process-scoped tracing refused |

use std::collections::VecDeque;
use std::path::PathBuf;

/// Diagnostic backend for allocation tracing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TracerBackend {
    Bcc,
    Bpftrace,
    Native,
}

impl TracerBackend {
    pub fn name(self) -> &'static str {
        match self {
            Self::Bcc => "bcc",
            Self::Bpftrace => "bpftrace",
            Self::Native => "native-ebpf",
        }
    }

    /// Binary a helper backend needs on PATH; native needs none.
    pub fn helper_binary(self) -> Option<&'static str> {
        match self {
            Self::Bcc => Some("bcc-trace"),
            Self::Bpftrace => Some("bpftrace"),
            Self::Native => None,
        }
    }
}

/// Whether one backend can run, with the reason either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendCapability {
    Available,
    Unavailable(String),
}

/// Full capability picture for allocation tracing.
#[derive(Debug, Clone)]
pub struct TracerCapabilities {
    pub kernel: (u64, u64),
    /// Whether BPF programs may be loaded here: root, or unprivileged
    /// BPF explicitly allowed. Named for the operation it gates, not for
    /// privilege in general.
    pub can_load_programs: bool,
    pub tracefs_accessible: bool,
    pub backends: Vec<(TracerBackend, BackendCapability)>,
}

impl TracerCapabilities {
    /// Any backend ready to attach.
    pub fn any_available(&self) -> bool {
        self.backends
            .iter()
            .any(|(_, capability)| *capability == BackendCapability::Available)
    }

    /// Human-readable report for `--trace-alloc` and diagnostics.
    pub fn report(&self) -> String {
        let mut lines = vec![format!(
            "kernel {}.{}, can load programs: {}, tracefs accessible: {}",
            self.kernel.0, self.kernel.1, self.can_load_programs, self.tracefs_accessible
        )];
        for (backend, capability) in &self.backends {
            match capability {
                BackendCapability::Available => {
                    lines.push(format!("{}: available", backend.name()));
                }
                BackendCapability::Unavailable(reason) => {
                    lines.push(format!("{}: unavailable ({reason})", backend.name()));
                }
            }
        }
        if !self.any_available() {
            lines.push("allocation tracing unavailable; normal monitoring continues".to_string());
        }
        lines.join("\n")
    }
}

/// Filesystem roots the detection reads. Injectable for fixture tests.
#[derive(Debug, Clone)]
pub struct DetectionRoots {
    pub version_file: PathBuf,
    /// tracefs candidates in preference order; the first listable wins.
    /// Kernels mount it at either `/sys/kernel/debug/tracing` (debugfs)
    /// or `/sys/kernel/tracing` (standalone tracefs).
    pub tracefs_dirs: Vec<PathBuf>,
    pub unprivileged_bpf_file: PathBuf,
    pub status_file: PathBuf,
    pub path_var: Option<std::ffi::OsString>,
}

impl Default for DetectionRoots {
    fn default() -> Self {
        Self {
            version_file: PathBuf::from("/proc/version"),
            tracefs_dirs: vec![
                PathBuf::from("/sys/kernel/tracing"),
                PathBuf::from("/sys/kernel/debug/tracing"),
            ],
            unprivileged_bpf_file: PathBuf::from("/proc/sys/kernel/unprivileged_bpf_disabled"),
            status_file: PathBuf::from("/proc/self/status"),
            path_var: std::env::var_os("PATH"),
        }
    }
}

/// Parse `Linux version 6.8.0-...` into (major, minor); unknown → (0, 0).
/// Release suffixes (`-generic`, `-rc1`) are stripped before parsing.
pub fn parse_kernel_version(text: &str) -> (u64, u64) {
    let version = text
        .strip_prefix("Linux version ")
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or("");
    let mut parts = version.split('.');
    let numeric_prefix = |part: &str| {
        part.chars()
            .take_while(|cell| cell.is_ascii_digit())
            .collect::<String>()
    };
    let major = parts
        .next()
        .and_then(|part| numeric_prefix(part).parse().ok())
        .unwrap_or(0);
    let minor = parts
        .next()
        .and_then(|part| numeric_prefix(part).parse().ok())
        .unwrap_or(0);
    (major, minor)
}

/// Parse the *effective* UID (second column) from `/proc/self/status`.
/// The real UID decides file access; the effective UID decides whether
/// privileged BPF operations succeed, which is what detection gates on.
fn read_uid(status_text: &str) -> Option<u32> {
    status_text.lines().find_map(|line| {
        let rest = line.strip_prefix("Uid:")?;
        let mut fields = rest.split_whitespace();
        fields.next()?;
        fields.next()?.parse().ok()
    })
}

/// Detect tracing capabilities from explicit roots. Every missing file is
/// an unavailable reason, never a panic and never an assumption.
pub fn detect_capabilities(roots: &DetectionRoots) -> TracerCapabilities {
    let version_text = std::fs::read_to_string(&roots.version_file).unwrap_or_default();
    let kernel = parse_kernel_version(&version_text);
    let status_text = std::fs::read_to_string(&roots.status_file).unwrap_or_default();
    let uid = read_uid(&status_text).unwrap_or(u32::MAX);
    let unprivileged_disabled = std::fs::read_to_string(&roots.unprivileged_bpf_file)
        .map(|text| text.trim() != "0")
        .unwrap_or(true);
    let can_load_programs = uid == 0 || !unprivileged_disabled;
    // Probe mount presence by listing: a tracefs we cannot even list is
    // certainly not one we can attach to. Write access itself is validated
    // at attach time by the backend, not here.
    let tracefs_accessible = roots
        .tracefs_dirs
        .iter()
        .any(|dir| std::fs::read_dir(dir).is_ok());

    let backends = [
        TracerBackend::Bcc,
        TracerBackend::Bpftrace,
        TracerBackend::Native,
    ]
    .iter()
    .map(|backend| {
        (
            *backend,
            backend_capability(
                *backend,
                roots,
                kernel,
                can_load_programs,
                tracefs_accessible,
            ),
        )
    })
    .collect();
    TracerCapabilities {
        kernel,
        can_load_programs,
        tracefs_accessible,
        backends,
    }
}

fn backend_capability(
    backend: TracerBackend,
    roots: &DetectionRoots,
    kernel: (u64, u64),
    can_load_programs: bool,
    tracefs_accessible: bool,
) -> BackendCapability {
    if let Some(binary) = backend.helper_binary()
        && !binary_on_path(binary, roots.path_var.clone())
    {
        return BackendCapability::Unavailable(format!("{binary} not on PATH"));
    }
    if backend == TracerBackend::Native && (kernel.0 < 5 || (kernel.0 == 5 && kernel.1 < 8)) {
        return BackendCapability::Unavailable("needs kernel 5.8+ for BPF ring buffers".into());
    }
    if !can_load_programs {
        return BackendCapability::Unavailable("needs root or unprivileged BPF".into());
    }
    if !tracefs_accessible {
        return BackendCapability::Unavailable("tracefs not mounted".into());
    }
    BackendCapability::Available
}

fn binary_on_path(name: &str, path_var: Option<std::ffi::OsString>) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Some(path_var) = path_var else {
        return false;
    };
    std::env::split_paths(&path_var).any(|dir| {
        if dir.as_os_str().is_empty() {
            return false;
        }
        let candidate = dir.join(name);
        std::fs::metadata(&candidate)
            .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    })
}

/// Bounded lifecycle for a future allocation tracer. `start` validates
/// capabilities and identity first; `stop` is idempotent; the event buffer
/// never exceeds capacity.
///
/// Integration seam for the later tracing work; exercised by tests until
/// a backend lands.
#[allow(dead_code)]
pub struct AllocationTracer {
    backend: TracerBackend,
    target_pid: i32,
    target_start_time: u64,
    running: bool,
    events: VecDeque<TracerEvent>,
    capacity: usize,
}

/// One buffered allocation event (placeholder shape for the integration).
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TracerEvent {
    pub pid: i32,
    pub bytes: u64,
}

/// Why a tracer refused to start.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TracerStartError {
    BackendUnavailable(String),
    InvalidTarget(String),
    AlreadyRunning,
}

impl std::fmt::Display for TracerStartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BackendUnavailable(reason) => write!(formatter, "backend unavailable: {reason}"),
            Self::InvalidTarget(reason) => write!(formatter, "invalid target: {reason}"),
            Self::AlreadyRunning => write!(formatter, "tracer already running"),
        }
    }
}

#[allow(dead_code)]
impl AllocationTracer {
    pub fn new(backend: TracerBackend, target_pid: i32, target_start_time: u64) -> Self {
        Self {
            backend,
            target_pid,
            target_start_time,
            running: false,
            events: VecDeque::new(),
            capacity: 1024,
        }
    }

    /// Start after validating backend, privilege and process identity.
    /// Common process identity (pid plus start time) correlates future
    /// events; a zero start time refuses process-scoped tracing. PID 1
    /// (init) is a legitimate target and is allowed.
    pub fn start(&mut self, capabilities: &TracerCapabilities) -> Result<(), TracerStartError> {
        if self.running {
            return Err(TracerStartError::AlreadyRunning);
        }
        if self.target_pid <= 0 || self.target_start_time == 0 {
            return Err(TracerStartError::InvalidTarget(
                "need a live PID with known start time".to_string(),
            ));
        }
        let available = capabilities.backends.iter().any(|(backend, capability)| {
            *backend == self.backend && *capability == BackendCapability::Available
        });
        if !available {
            return Err(TracerStartError::BackendUnavailable(format!(
                "{} cannot attach here",
                self.backend.name()
            )));
        }
        self.running = true;
        Ok(())
    }

    /// Stop and drop buffered events. Idempotent by design so teardown in
    /// `Drop` and explicit teardown cannot double-free state.
    pub fn stop(&mut self) {
        self.running = false;
        self.events.clear();
    }

    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Buffer one event, evicting the oldest past capacity. Events are
    /// only buffered while running; a zero capacity disables buffering.
    pub fn push_event(&mut self, event: TracerEvent) {
        if !self.running || self.capacity == 0 {
            return;
        }
        if self.events.len() >= self.capacity {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    pub fn buffered(&self) -> usize {
        self.events.len()
    }
}

impl Drop for AllocationTracer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Build fixture roots: version/status/sysctl files plus a tracefs dir,
    /// all under a unique temp dir. Missing pieces stay missing.
    fn fixture_roots(
        version: Option<&str>,
        status: Option<&str>,
        unprivileged: Option<&str>,
        with_tracefs: bool,
        path_var: Option<std::ffi::OsString>,
    ) -> (PathBuf, DetectionRoots) {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ramwise-trace-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, contents: &str| {
            let path = dir.join(name);
            std::fs::write(&path, contents).unwrap();
            path
        };
        let version_file = version.map_or_else(
            || dir.join("version-missing"),
            |text| write("version", text),
        );
        let status_file =
            status.map_or_else(|| dir.join("status-missing"), |text| write("status", text));
        let unprivileged_bpf_file = unprivileged.map_or_else(
            || dir.join("unprivileged-missing"),
            |text| write("unprivileged", text),
        );
        let tracefs_dir = dir.join("tracing");
        if with_tracefs {
            std::fs::create_dir_all(&tracefs_dir).unwrap();
        }
        let roots = DetectionRoots {
            version_file,
            tracefs_dirs: vec![tracefs_dir],
            unprivileged_bpf_file,
            status_file,
            path_var,
        };
        (dir, roots)
    }

    fn cleanup(dir: &PathBuf) {
        let _ = std::fs::remove_dir_all(dir);
    }

    fn executable_fixture(dir: &Path, name: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();
    }

    #[test]
    fn kernel_versions_parse_deterministically() {
        assert_eq!(
            parse_kernel_version("Linux version 6.8.0-41-generic (buildd@lcy02-amd64)"),
            (6, 8)
        );
        assert_eq!(parse_kernel_version("Linux version 5.4.0"), (5, 4));
        assert_eq!(parse_kernel_version("garbage"), (0, 0));
        assert_eq!(parse_kernel_version(""), (0, 0));
    }

    #[test]
    fn uid_parses_effective_uid_from_status_text() {
        assert_eq!(
            read_uid("Name:\tramwise\nUid:\t1000\t1000\t1000\t1000\n"),
            Some(1000)
        );
        assert_eq!(read_uid("Uid:\t0\t0\t0\t0\n"), Some(0));
        // Setuid context: real 1000, effective 0 — detection gates on the
        // effective identity that decides BPF success.
        assert_eq!(read_uid("Uid:\t1000\t0\t0\t0\n"), Some(0));
        assert_eq!(read_uid("no uid here\n"), None);
    }

    #[test]
    fn missing_inputs_yield_reasons_not_panics() {
        let (dir, roots) = fixture_roots(None, None, None, false, None);
        let capabilities = detect_capabilities(&roots);
        assert_eq!(capabilities.kernel, (0, 0));
        assert!(!capabilities.can_load_programs);
        assert!(!capabilities.tracefs_accessible);
        assert!(!capabilities.any_available());
        let report = capabilities.report();
        assert!(report.contains("not on PATH") || report.contains("needs kernel"));
        assert!(report.contains("normal monitoring continues"));
        cleanup(&dir);
    }

    #[test]
    fn old_kernels_block_native_but_helpers_may_pass() {
        let bindir = std::env::temp_dir().join(format!(
            "ramwise-trace-tools-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos())
        ));
        std::fs::create_dir_all(&bindir).unwrap();
        executable_fixture(&bindir, "bpftrace");
        // A non-executable file with the right name must not count.
        std::fs::write(bindir.join("bcc-trace"), "#!/bin/sh").unwrap();
        let path_var: std::ffi::OsString = bindir.clone().into_os_string();
        let (dir, roots) = fixture_roots(
            Some("Linux version 5.4.0"),
            Some("Uid:\t0\t0\t0\t0\n"),
            Some("0"),
            true,
            Some(path_var),
        );
        let capabilities = detect_capabilities(&roots);
        assert!(capabilities.can_load_programs);
        let native = capabilities
            .backends
            .iter()
            .find(|(backend, _)| *backend == TracerBackend::Native)
            .unwrap();
        assert!(matches!(
            native.1,
            BackendCapability::Unavailable(ref reason) if reason.contains("5.8")
        ));
        let bpftrace = capabilities
            .backends
            .iter()
            .find(|(backend, _)| *backend == TracerBackend::Bpftrace)
            .unwrap();
        assert_eq!(bpftrace.1, BackendCapability::Available);
        assert!(capabilities.any_available());
        let bcc = capabilities
            .backends
            .iter()
            .find(|(backend, _)| *backend == TracerBackend::Bcc)
            .unwrap();
        // Present but not executable: still unavailable.
        assert!(matches!(bcc.1, BackendCapability::Unavailable(_)));
        cleanup(&dir);
        cleanup(&bindir);
    }

    #[test]
    fn tracer_lifecycle_validates_identity_and_backend() {
        let (dir, roots) = fixture_roots(
            Some("Linux version 6.8.0"),
            Some("Uid:\t0\t0\t0\t0\n"),
            Some("0"),
            true,
            Some(std::ffi::OsString::new()),
        );
        let capabilities = detect_capabilities(&roots);
        // No helper binaries on the empty PATH: helpers unavailable, native
        // available (root + new kernel + tracefs present).
        assert!(capabilities.any_available());

        let mut tracer = AllocationTracer::new(TracerBackend::Native, 4242, 999);
        assert!(tracer.start(&capabilities).is_ok());
        assert!(tracer.is_running());
        assert_eq!(
            tracer.start(&capabilities),
            Err(TracerStartError::AlreadyRunning)
        );
        tracer.push_event(TracerEvent {
            pid: 4242,
            bytes: 64,
        });
        assert_eq!(tracer.buffered(), 1);
        tracer.stop();
        assert!(!tracer.is_running());
        assert_eq!(tracer.buffered(), 0);
        tracer.stop();

        let mut unknown_target = AllocationTracer::new(TracerBackend::Native, 4242, 0);
        assert!(matches!(
            unknown_target.start(&capabilities),
            Err(TracerStartError::InvalidTarget(_))
        ));
        let mut missing_backend = AllocationTracer::new(TracerBackend::Bcc, 4242, 999);
        assert!(matches!(
            missing_backend.start(&capabilities),
            Err(TracerStartError::BackendUnavailable(_))
        ));
        let mut init_target = AllocationTracer::new(TracerBackend::Native, 1, 999);
        assert!(init_target.start(&capabilities).is_ok());
        cleanup(&dir);
    }

    #[test]
    fn event_buffer_is_bounded() {
        let mut tracer = AllocationTracer::new(TracerBackend::Native, 7, 7);
        tracer.capacity = 4;
        tracer.running = true;
        for index in 0..10 {
            tracer.push_event(TracerEvent {
                pid: 7,
                bytes: index,
            });
        }
        assert_eq!(tracer.buffered(), 4);
        tracer.stop();
        tracer.push_event(TracerEvent { pid: 7, bytes: 99 });
        assert_eq!(tracer.buffered(), 0);
        tracer.capacity = 0;
        tracer.running = true;
        tracer.push_event(TracerEvent { pid: 7, bytes: 99 });
        assert_eq!(tracer.buffered(), 0);
    }
}
