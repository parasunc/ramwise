//! Validated external-tool actions for selected processes.
//!
//! Tools launch through an explicit confirmation with an argv array that is
//! spawned directly — process data (PID, executable path) always travels as
//! discrete arguments, never interpolated into a shell string, so a hostile
//! process name cannot escape into command execution. Missing binaries and
//! permission failures report through the existing status mechanism instead
//! of failing silently.
//!
//! Fullscreen tools (htop) need the terminal back: the caller suspends the
//! TUI (leave the alternate screen, disable raw mode), runs
//! [`run_interactive`], then resumes. That handoff lives with the terminal
//! owner, not here.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use crate::collector::ProcessMemory;

/// External diagnostic tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalTool {
    Htop,
    Strace,
    Valgrind,
}

impl ExternalTool {
    pub fn name(self) -> &'static str {
        match self {
            Self::Htop => "htop",
            Self::Strace => "strace",
            Self::Valgrind => "valgrind",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Htop => "inspect in htop",
            Self::Strace => "trace syscalls",
            Self::Valgrind => "relaunch under memcheck",
        }
    }
}

/// A validated launch: resolved binary plus discrete argv, ready to spawn
/// without a shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
}

/// Why a launch cannot proceed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    /// Binary not found on PATH.
    MissingBinary(&'static str),
    /// Target cannot take this action (kernel thread, PID 0/1, own PID).
    UnsupportedTarget(String),
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingBinary(name) => write!(formatter, "{name} is not installed"),
            Self::UnsupportedTarget(reason) => write!(formatter, "{reason}"),
        }
    }
}

/// Find an executable on PATH without spawning a shell or `which`.
/// `path_var` is injected for tests; callers pass `std::env::var_os("PATH")`.
/// Only absolute directories participate: a relative PATH entry resolves
/// against whatever the working directory happens to be at spawn time.
pub fn resolve_on_path(name: &str, path_var: Option<std::ffi::OsString>) -> Option<PathBuf> {
    let path_var = path_var?;
    for dir in std::env::split_paths(&path_var) {
        if !dir.is_absolute() {
            continue;
        }
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Whether a resolved program is still executable (spawn-time recheck
/// against PATH swaps between confirmation and launch).
pub fn program_executable(program: &Path) -> bool {
    is_executable(program)
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Validate the target and build the argv for one tool.
///
/// - htop/strace attach to the running PID (`-p <pid>` as separate args).
/// - valgrind cannot attach to a running process, so it relaunches the
///   executable (argv[0] only, never the original arguments) under memcheck
///   behind the same explicit confirmation.
pub fn build_command(
    tool: ExternalTool,
    process: &ProcessMemory,
    path_var: Option<std::ffi::OsString>,
) -> Result<CommandSpec, ToolError> {
    if process.pid <= 1 {
        return Err(ToolError::UnsupportedTarget(format!(
            "refusing to target PID {}",
            process.pid
        )));
    }
    if process.is_kernel_thread() {
        return Err(ToolError::UnsupportedTarget(
            "kernel threads have no user-space target".to_string(),
        ));
    }
    if process.pid == std::process::id() as i32 {
        return Err(ToolError::UnsupportedTarget(
            "refusing to target ramwise itself".to_string(),
        ));
    }
    let program =
        resolve_on_path(tool.name(), path_var).ok_or(ToolError::MissingBinary(tool.name()))?;
    let args = match tool {
        ExternalTool::Htop => vec!["-p".to_string(), process.pid.to_string()],
        ExternalTool::Strace => vec!["-p".to_string(), process.pid.to_string()],
        ExternalTool::Valgrind => {
            // Valgrind cannot attach: relaunch the executable behind the
            // same confirmation. Resolve through /proc/<pid>/exe so a
            // relative argv[0] cannot hijack the child PATH; fall back to
            // an absolute argv[0], and refuse anything else.
            vec![
                "--tool=memcheck".to_string(),
                "--".to_string(),
                resolve_target_exe(process)?,
            ]
        }
    };
    Ok(CommandSpec { program, args })
}

/// Absolute executable path for a target: the kernel's view first.
fn resolve_target_exe(process: &ProcessMemory) -> Result<String, ToolError> {
    let proc_exe = PathBuf::from(format!("/proc/{}/exe", process.pid));
    if let Ok(target) = std::fs::read_link(&proc_exe) {
        // Deleted binaries report " (deleted)"; still absolute, still fine.
        if target.is_absolute() {
            return Ok(target.to_string_lossy().into_owned());
        }
    }
    let first = process
        .cmdline
        .split_whitespace()
        .next()
        .filter(|arg| !arg.is_empty())
        .unwrap_or(&process.name);
    if first.starts_with('/') {
        return Ok(first.to_string());
    }
    Err(ToolError::UnsupportedTarget(
        "cannot determine an absolute executable path for valgrind relaunch".to_string(),
    ))
}

/// Spawn a validated command interactively, waiting for exit. The caller
/// must have suspended the TUI first; see the module docs.
pub fn run_interactive(spec: &CommandSpec) -> io::Result<ExitStatus> {
    Command::new(&spec.program).args(&spec.args).status()
}

/// Human-readable launch description for the confirmation prompt.
pub fn describe(spec: &CommandSpec) -> String {
    let mut parts = vec![spec.program.to_string_lossy().into_owned()];
    parts.extend(spec.args.iter().cloned());
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture_path_with_tools(tools: &[&str]) -> (PathBuf, std::ffi::OsString) {
        let dir = std::env::temp_dir().join(format!(
            "ramwise-tools-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for tool in tools {
            let path = dir.join(tool);
            let mut file = std::fs::File::create(&path).unwrap();
            writeln!(file, "#!/bin/sh").unwrap();
            let mut permissions = file.metadata().unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&path, permissions).unwrap();
        }
        let path_var: std::ffi::OsString = dir.clone().into_os_string();
        (dir, path_var)
    }

    fn cleanup(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    fn target_process() -> ProcessMemory {
        ProcessMemory {
            pid: 4242,
            name: "worker".into(),
            cmdline: "/usr/bin/worker --flag; rm -rf ~".into(),
            rss: 1024,
            vss: 2048,
            ..Default::default()
        }
    }

    #[test]
    fn resolve_finds_executables_and_skips_missing() {
        let (dir, path) = fixture_path_with_tools(&["htop"]);
        assert!(resolve_on_path("htop", Some(path.clone())).is_some());
        assert_eq!(resolve_on_path("strace", Some(path)), None);
        assert_eq!(resolve_on_path("htop", None), None);
        cleanup(&dir);
    }

    #[test]
    fn pid_travels_as_a_discrete_argument_never_a_shell_string() {
        let (dir, path) = fixture_path_with_tools(&["htop", "strace"]);
        let process = target_process();
        let htop = build_command(ExternalTool::Htop, &process, Some(path.clone())).unwrap();
        assert_eq!(htop.args, vec!["-p".to_string(), "4242".to_string()]);
        // The hostile cmdline above must not leak into the argv...
        assert!(!htop.args.iter().any(|arg| arg.contains("rm")));
        // ...and the command is spawned directly, never through sh -c.
        assert!(htop.program.ends_with("htop"));
        let strace = build_command(ExternalTool::Strace, &process, Some(path)).unwrap();
        assert_eq!(strace.args, vec!["-p".to_string(), "4242".to_string()]);
        cleanup(&dir);
    }

    #[test]
    fn valgrind_relaunches_the_executable_without_original_args() {
        let (dir, path) = fixture_path_with_tools(&["valgrind"]);
        let spec = build_command(ExternalTool::Valgrind, &target_process(), Some(path)).unwrap();
        assert_eq!(
            spec.args,
            vec![
                "--tool=memcheck".to_string(),
                "--".to_string(),
                "/usr/bin/worker".to_string()
            ]
        );
        cleanup(&dir);
    }

    #[test]
    fn valgrind_refuses_relative_executables() {
        let (dir, path) = fixture_path_with_tools(&["valgrind"]);
        let mut process = target_process();
        process.pid = 987_654_321;
        process.cmdline = "relative-worker --flag".into();
        process.name = "relative-worker".into();
        assert!(matches!(
            build_command(ExternalTool::Valgrind, &process, Some(path)),
            Err(ToolError::UnsupportedTarget(_))
        ));
        cleanup(&dir);
    }

    #[test]
    fn missing_binaries_and_bad_targets_are_explicit() {
        let (dir, path) = fixture_path_with_tools(&[]);
        let process = target_process();
        assert_eq!(
            build_command(ExternalTool::Htop, &process, Some(path)),
            Err(ToolError::MissingBinary("htop"))
        );
        cleanup(&dir);
        let (dir, path) = fixture_path_with_tools(&["htop"]);
        let mut kernel = target_process();
        kernel.rss = 0;
        kernel.vss = 0;
        assert!(matches!(
            build_command(ExternalTool::Htop, &kernel, Some(path.clone())),
            Err(ToolError::UnsupportedTarget(_))
        ));
        let mut low = target_process();
        low.pid = 1;
        assert!(matches!(
            build_command(ExternalTool::Htop, &low, Some(path)),
            Err(ToolError::UnsupportedTarget(_))
        ));
        cleanup(&dir);
    }

    #[test]
    fn describe_joins_program_and_args() {
        let spec = CommandSpec {
            program: PathBuf::from("/usr/bin/htop"),
            args: vec!["-p".into(), "7".into()],
        };
        assert_eq!(describe(&spec), "/usr/bin/htop -p 7");
    }
}
