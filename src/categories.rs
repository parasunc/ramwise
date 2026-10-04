//! Heuristic process categories and heavy-app labels.
//!
//! Classification is a pure function of the executable name and command
//! line: small substring tables matched at token boundaries, evaluated in
//! priority order, with explicit per-binary overrides that always win.
//! Boundary matching keeps `edge` out of `ledger` and `code` out of
//! `encode` without a regex dependency — the tables stay legible and fast
//! enough to run per frame over hundreds of processes.
//!
//! [`summarize_by_category`] aggregates RSS/PSS/private/swap and counts so a
//! grouped view can coexist with the flat process list; PID drill-down is
//! preserved because summaries carry counts, not borrows, alongside the
//! untouched flat list.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::collector::ProcessMemory;

/// Heuristic category for a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Browser,
    Electron,
    Development,
    Service,
    System,
    Terminal,
    Media,
    Other,
}

impl Category {
    /// Short stable label for display and tests.
    pub fn label(self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::Electron => "electron",
            Self::Development => "dev",
            Self::Service => "service",
            Self::System => "system",
            Self::Terminal => "terminal",
            Self::Media => "media",
            Self::Other => "other",
        }
    }

    /// Sort rank for grouped views: user-facing apps first, unclassified
    /// last. (Kernel threads sort with system; see [`classify_with`].)
    pub fn rank(self) -> u8 {
        match self {
            Self::Browser => 0,
            Self::Electron => 1,
            Self::Media => 2,
            Self::Development => 3,
            Self::Terminal => 4,
            Self::Service => 5,
            Self::System => 6,
            Self::Other => 7,
        }
    }
}

/// One substring rule: when the haystack contains `pattern`, the process is
/// `category`. Tables are evaluated in order; first match wins.
#[derive(Debug, Clone)]
pub struct CategoryRule {
    pub pattern: &'static str,
    pub category: Category,
}

/// Configurable pattern tables plus per-binary overrides.
///
/// Overrides are keyed by lowercase executable base name and always win over
/// the tables, so a misclassified binary is one map entry away from correct.
#[derive(Debug, Clone)]
pub struct CategoryConfig {
    pub rules: Vec<CategoryRule>,
    pub overrides: HashMap<String, Category>,
}

impl Default for CategoryConfig {
    fn default() -> Self {
        let table: &[(&[&str], Category)] = &[
            (
                &[
                    "firefox", "chrome", "chromium", "zen", "brave", "edge", "opera", "vivaldi",
                ],
                Category::Browser,
            ),
            (
                &["electron", "slack", "discord", "teams", "signal-desktop"],
                Category::Electron,
            ),
            (
                &[
                    "code", "vscode", "codium", "idea", "pycharm", "clion", "webstorm", "zed",
                    "nvim", "vim", "emacs", "helix", "kakoune",
                ],
                Category::Development,
            ),
            (
                &[
                    "cargo", "rustc", "gcc", "clang", "g++", "go", "javac", "gradle", "maven",
                    "make", "cmake", "ninja", "tsc", "esbuild",
                ],
                Category::Development,
            ),
            (
                &[
                    "systemd",
                    "dbus",
                    "pipewire",
                    "pulseaudio",
                    "docker",
                    "containerd",
                    "sshd",
                    "cron",
                    "networkmanager",
                    "bluetoothd",
                    "upowerd",
                ],
                Category::Service,
            ),
            (
                &[
                    "kthreadd",
                    "kworker",
                    "migration",
                    "rcu_",
                    "kswapd",
                    "jbd2",
                    "init",
                ],
                Category::System,
            ),
            (
                &[
                    "alacritty",
                    "kitty",
                    "gnome-terminal",
                    "konsole",
                    "xterm",
                    "foot",
                    "wezterm",
                    "tmux",
                    "screen",
                ],
                Category::Terminal,
            ),
            (
                &[
                    "mpv",
                    "vlc",
                    "ffmpeg",
                    "obs",
                    "spotify",
                    "rhythmbox",
                    "audacity",
                ],
                Category::Media,
            ),
        ];
        let rules = table
            .iter()
            .flat_map(|(patterns, category)| {
                patterns.iter().map(|pattern| CategoryRule {
                    pattern,
                    category: *category,
                })
            })
            .collect();
        Self {
            rules,
            overrides: HashMap::new(),
        }
    }
}

impl CategoryConfig {
    /// Pin one binary (lowercase base name, e.g. `"myide"`) to a category.
    pub fn with_override(mut self, binary: &str, category: Category) -> Self {
        self.overrides.insert(binary.to_lowercase(), category);
        self
    }
}

/// Base executable name, lowercased, without arguments or directories.
/// Base executable name, lowercased, without arguments, directories,
/// Windows separators or a trailing `.exe`.
fn exe_base(process: &ProcessMemory) -> String {
    let first = process
        .cmdline
        .split_whitespace()
        .next()
        .filter(|arg| !arg.is_empty())
        .unwrap_or(&process.name);
    let base = first.rsplit(['/', '\\']).next().unwrap_or(first);
    base.strip_suffix(".exe").unwrap_or(base).to_lowercase()
}

/// Whether `pattern` occurs in `haystack` at a token boundary (both sides
/// are string edges or non-alphanumeric), so short patterns cannot match
/// inside unrelated words.
fn matches_boundary(haystack: &str, pattern: &str) -> bool {
    haystack.match_indices(pattern).any(|(index, _)| {
        let before = haystack[..index].chars().next_back();
        let after = haystack[index + pattern.len()..].chars().next();
        let boundary = |side: Option<char>| side.is_none_or(|cell| !cell.is_alphanumeric());
        boundary(before) && boundary(after)
    })
}

/// Shared default tables, built once: classification runs per process per
/// frame, so rebuilding the tables every call is pure waste.
fn default_config() -> &'static CategoryConfig {
    static CONFIG: OnceLock<CategoryConfig> = OnceLock::new();
    CONFIG.get_or_init(CategoryConfig::default)
}

/// Classify with an explicit config.
pub fn classify_with(process: &ProcessMemory, config: &CategoryConfig) -> Category {
    // Kernel threads have no user-space footprint to label, and overrides
    // never apply to them: there is nothing meaningful to override to.
    if process.is_kernel_thread() {
        return Category::System;
    }
    let exe = exe_base(process);
    if let Some(category) = config.overrides.get(&exe) {
        return *category;
    }
    let haystack = format!("{} {}", exe, process.cmdline.to_lowercase());
    config
        .rules
        .iter()
        .find(|rule| matches_boundary(&haystack, rule.pattern))
        .map(|rule| rule.category)
        .unwrap_or(Category::Other)
}

/// Classify with the default tables.
pub fn classify(process: &ProcessMemory) -> Category {
    classify_with(process, default_config())
}

/// Aggregated memory per category. Borrows nothing: counts and byte totals
/// are copied out so summaries outlive the snapshot borrow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategorySummary {
    pub category: Category,
    pub count: usize,
    pub rss: u64,
    pub pss: u64,
    pub private: u64,
    pub swap: u64,
}

/// Aggregate processes by category, sorted by RSS descending. Flat-list order
/// is untouched — summaries are a parallel grouped view over the same data.
pub fn summarize_by_category(processes: &[ProcessMemory]) -> Vec<CategorySummary> {
    let mut summaries: HashMap<Category, CategorySummary> = HashMap::new();
    for process in processes {
        let category = classify(process);
        let entry = summaries.entry(category).or_insert(CategorySummary {
            category,
            count: 0,
            rss: 0,
            pss: 0,
            private: 0,
            swap: 0,
        });
        entry.count += 1;
        entry.rss = entry.rss.saturating_add(process.rss);
        entry.pss = entry.pss.saturating_add(process.pss);
        entry.private = entry.private.saturating_add(process.private);
        entry.swap = entry.swap.saturating_add(process.swap);
    }
    let mut summaries: Vec<_> = summaries.into_values().collect();
    summaries.sort_by_key(|summary| std::cmp::Reverse(summary.rss));
    summaries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process_named(name: &str, cmdline: &str) -> ProcessMemory {
        ProcessMemory {
            name: name.into(),
            cmdline: cmdline.into(),
            rss: 1024,
            vss: 2048,
            ..Default::default()
        }
    }

    #[test]
    fn known_binaries_classify_into_expected_categories() {
        let cases = [
            ("firefox", "/usr/lib/firefox/firefox", Category::Browser),
            ("chrome", "/opt/google/chrome/chrome --x", Category::Browser),
            ("slack", "/usr/bin/slack --no-sandbox", Category::Electron),
            (
                "code",
                "/usr/share/code/code --unity",
                Category::Development,
            ),
            ("nvim", "nvim src/main.rs", Category::Development),
            ("cargo", "cargo test --workspace", Category::Development),
            ("systemd", "/usr/lib/systemd/systemd", Category::Service),
            ("docker", "dockerd --group docker", Category::Service),
            (
                "alacritty",
                "alacritty --working-directory ~",
                Category::Terminal,
            ),
            ("mpv", "mpv video.mkv", Category::Media),
            ("kworker", "[kworker/0:1-events]", Category::System),
            ("mystery", "/opt/mystery/bin/run", Category::Other),
        ];
        for (name, cmdline, expected) in cases {
            assert_eq!(
                classify(&process_named(name, cmdline)),
                expected,
                "binary {name}"
            );
        }
    }

    #[test]
    fn matching_is_case_insensitive_and_path_agnostic() {
        assert_eq!(
            classify(&process_named("x", "C:\\Tools\\FIREFOX.EXE")),
            Category::Browser
        );
        assert_eq!(
            classify(&process_named("CODE", "/usr/bin/code")),
            Category::Development
        );
    }

    #[test]
    fn overrides_always_win_over_tables() {
        let config = CategoryConfig::default().with_override("firefox", Category::Development);
        assert_eq!(
            classify_with(&process_named("firefox", "firefox"), &config),
            Category::Development
        );
        // Default tables are unchanged for everything else.
        assert_eq!(
            classify(&process_named("firefox", "firefox")),
            Category::Browser
        );
    }

    #[test]
    fn kernel_threads_are_system() {
        let mut kernel = process_named("kthreadd", "");
        kernel.rss = 0;
        kernel.vss = 0;
        assert_eq!(classify(&kernel), Category::System);
    }

    #[test]
    fn summaries_aggregate_and_sort_by_rss() {
        let processes = vec![
            ProcessMemory {
                rss: 300,
                pss: 200,
                private: 100,
                swap: 10,
                ..process_named("firefox", "firefox")
            },
            ProcessMemory {
                rss: 100,
                ..process_named("chrome", "chrome")
            },
            ProcessMemory {
                rss: 500,
                ..process_named("code", "code")
            },
        ];
        let summaries = summarize_by_category(&processes);
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].category, Category::Development);
        assert_eq!(summaries[0].count, 1);
        assert_eq!(summaries[1].category, Category::Browser);
        assert_eq!(summaries[1].count, 2);
        assert_eq!(summaries[1].rss, 400);
        assert_eq!(summaries[1].swap, 10);
    }

    #[test]
    fn category_labels_and_ranks_are_stable() {
        assert_eq!(Category::Browser.label(), "browser");
        assert_eq!(Category::Other.label(), "other");
        assert!(Category::Browser.rank() < Category::System.rank());
    }

    #[test]
    fn substring_tables_do_not_match_inside_words() {
        // Adversarial near-misses that naive substring matching mislabels.
        let cases = [
            ("citizen", "/usr/bin/citizen"),
            ("ledger", "/opt/ledger/ledger-live"),
            ("encode", "/usr/bin/encode-video"),
            ("screenshot", "screenshot-tool"),
            ("jobs", "background-jobs"),
            ("civil", "/usr/bin/civil-editor"),
            ("operate", "operate-dashboard"),
        ];
        for (name, cmdline) in cases {
            let category = classify(&process_named(name, cmdline));
            assert_ne!(category, Category::Browser, "binary {name}");
            assert_ne!(category, Category::Development, "binary {name}");
            assert_ne!(category, Category::Terminal, "binary {name}");
            assert_ne!(category, Category::Media, "binary {name}");
        }
    }

    #[test]
    fn windows_paths_and_exe_suffixes_resolve() {
        assert_eq!(
            classify(&process_named("x", "C:\\Tools\\FIREFOX.EXE")),
            Category::Browser
        );
        assert_eq!(
            classify(&process_named("CODE.EXE", "C:\\VS\\CODE.EXE")),
            Category::Development
        );
    }

    #[test]
    fn overrides_do_not_apply_to_kernel_threads() {
        let config = CategoryConfig::default().with_override("kworker", Category::Browser);
        let mut kernel = process_named("kworker", "[kworker/0:1-events]");
        kernel.rss = 0;
        kernel.vss = 0;
        assert_eq!(classify_with(&kernel, &config), Category::System);
    }
}
