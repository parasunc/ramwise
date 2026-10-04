//! Process filters and ppid-tree navigation.
//!
//! [`ProcessFilter`] is a composable, typed predicate over one process:
//! every enabled condition must hold (AND). [`order_as_tree`] stably
//! reorders a slice into parent-before-child preorder so index-based
//! selection keeps working in tree mode, and [`forest_depths`] supplies the
//! indentation for rendering. Missing or reparented processes become roots;
//! ppid cycles are broken deterministically instead of recursing forever.

use std::collections::{HashMap, HashSet};

use crate::collector::ProcessMemory;
use crate::utils::format_bytes;

/// Typed memory filters. All conditions combine with AND.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProcessFilter {
    /// Keep only processes with no shared memory.
    pub only_private: bool,
    /// Keep only processes with some shared memory.
    pub only_shared: bool,
    /// Keep only processes with at least this much PSS, in bytes.
    pub min_pss_bytes: u64,
}

impl ProcessFilter {
    /// Whether any condition is enabled.
    pub fn is_active(self) -> bool {
        self.only_private || self.only_shared || self.min_pss_bytes > 0
    }

    /// Short label for titles and tests; `None` when inactive.
    /// Combined filters compose (`private-only + min-pss≥10M`) so the
    /// title never hides an active predicate.
    pub fn label(self) -> Option<String> {
        let mut parts = Vec::new();
        if self.only_private {
            parts.push("private-only".to_string());
        } else if self.only_shared {
            parts.push("shared-only".to_string());
        }
        if self.min_pss_bytes > 0 {
            parts.push(format!("min-pss≥{}", format_bytes(self.min_pss_bytes)));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" + "))
        }
    }

    /// Whether one process passes every enabled condition.
    pub fn matches(self, process: &ProcessMemory) -> bool {
        if self.only_private && process.shared != 0 {
            return false;
        }
        if self.only_shared && process.shared == 0 {
            return false;
        }
        if process.pss < self.min_pss_bytes {
            return false;
        }
        true
    }
}

/// Reorder a slice into parent-before-child preorder, preserving the
/// current relative order among roots and among siblings. Processes whose
/// parent is missing (exited, reparented, or outside the snapshot) become
/// roots; self-parents and ppid cycles resolve by first-seen order without
/// losing or duplicating any process.
pub fn order_as_tree(processes: &mut [ProcessMemory]) {
    let len = processes.len();
    if len < 2 {
        return;
    }
    let pids: HashSet<i32> = processes.iter().map(|p| p.pid).collect();
    let is_root = |p: &ProcessMemory| p.ppid == 0 || p.ppid == p.pid || !pids.contains(&p.ppid);
    // Parent PID to child indices in slice order, so each subtree visit is
    // O(children) instead of rescanning the whole slice per node.
    let mut children: HashMap<i32, Vec<usize>> = HashMap::new();
    for (index, process) in processes.iter().enumerate() {
        children.entry(process.ppid).or_default().push(index);
    }

    let mut visited = vec![false; len];
    let mut order = Vec::with_capacity(len);
    // Roots first, in slice order; then cycle members and anything skipped.
    for pass_roots_only in [true, false] {
        for start in 0..len {
            if visited[start] {
                continue;
            }
            if pass_roots_only && !is_root(&processes[start]) {
                continue;
            }
            let mut stack = vec![start];
            while let Some(index) = stack.pop() {
                if visited[index] {
                    continue;
                }
                visited[index] = true;
                order.push(index);
                // Push children in reverse so pop order keeps slice order.
                if let Some(young) = children.get(&processes[index].pid) {
                    for child in young.iter().rev() {
                        if !visited[*child] {
                            stack.push(*child);
                        }
                    }
                }
            }
        }
    }

    // Reorder by permutation. Cloning here is one O(n) pass of plain
    // moves — cheap next to a frame render — and keeps the code safe.
    let mut reordered = Vec::with_capacity(len);
    for index in order {
        reordered.push(processes[index].clone());
    }
    processes.clone_from_slice(&reordered);
}

/// Depth of every process in the ppid forest (roots are depth 0), computed
/// over one shared parent map. Missing parents, self-parents and cycles
/// all resolve to a finite depth instead of recursing.
pub fn forest_depths(processes: &[ProcessMemory]) -> HashMap<i32, usize> {
    let parent: HashMap<i32, i32> = processes.iter().map(|p| (p.pid, p.ppid)).collect();
    let mut depths = HashMap::with_capacity(processes.len());
    for process in processes {
        let mut depth = 0;
        let mut current = process.pid;
        let mut seen = HashSet::new();
        while let Some(ppid) = parent.get(&current) {
            if *ppid == current || !seen.insert(current) {
                break;
            }
            if *ppid == 0 {
                // PID 0 parents one level when tracked; otherwise this
                // process is a root and the walk ends here.
                if parent.contains_key(&0) {
                    depth += 1;
                }
                break;
            }
            if !parent.contains_key(ppid) {
                break;
            }
            depth += 1;
            current = *ppid;
        }
        depths.insert(process.pid, depth);
    }
    depths
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: i32, ppid: i32, rss: u64) -> ProcessMemory {
        ProcessMemory {
            pid,
            ppid,
            name: format!("p{pid}"),
            rss,
            vss: rss,
            shared: rss / 2,
            pss: rss / 2,
            ..Default::default()
        }
    }

    #[test]
    fn inactive_filter_matches_everything() {
        let filter = ProcessFilter::default();
        assert!(!filter.is_active());
        assert_eq!(filter.label(), None);
        assert!(filter.matches(&proc(1, 0, 100)));
    }

    #[test]
    fn private_and_shared_flags_are_composable() {
        let mut process = proc(1, 0, 100);
        process.shared = 0;
        let private = ProcessFilter {
            only_private: true,
            ..Default::default()
        };
        assert!(private.matches(&process));
        process.shared = 10;
        assert!(!private.matches(&process));

        let shared = ProcessFilter {
            only_shared: true,
            ..Default::default()
        };
        assert!(shared.matches(&process));
        process.shared = 0;
        assert!(!shared.matches(&process));

        // Both flags together admit nothing with nonzero shared and nothing
        // without it, which is exactly what the combination means.
        let both = ProcessFilter {
            only_private: true,
            only_shared: true,
            ..Default::default()
        };
        assert!(!both.matches(&process));
        assert_eq!(private.label().as_deref(), Some("private-only"));
        assert_eq!(shared.label().as_deref(), Some("shared-only"));
    }

    #[test]
    fn min_pss_boundary_is_inclusive() {
        let filter = ProcessFilter {
            min_pss_bytes: 50,
            ..Default::default()
        };
        let mut process = proc(1, 0, 100);
        process.pss = 50;
        assert!(filter.matches(&process));
        process.pss = 49;
        assert!(!filter.matches(&process));
    }

    #[test]
    fn tree_orders_parents_before_children() {
        let mut processes = vec![
            proc(3, 1, 10),
            proc(1, 0, 300),
            proc(4, 2, 20),
            proc(2, 1, 200),
        ];
        order_as_tree(&mut processes);
        let pids: Vec<i32> = processes.iter().map(|p| p.pid).collect();
        assert_eq!(pids, vec![1, 3, 2, 4]);
        let depths = forest_depths(&processes);
        assert_eq!(depths[&4], 2);
        assert_eq!(depths[&1], 0);
    }

    #[test]
    fn missing_and_self_parents_become_roots() {
        let mut processes = vec![proc(9, 999, 10), proc(8, 8, 20), proc(7, 0, 30)];
        order_as_tree(&mut processes);
        let pids: Vec<i32> = processes.iter().map(|p| p.pid).collect();
        assert_eq!(pids.len(), 3);
        assert!(pids.contains(&9) && pids.contains(&8) && pids.contains(&7));
        assert_eq!(forest_depths(&processes)[&9], 0);
        assert_eq!(forest_depths(&processes)[&8], 0);
    }

    #[test]
    fn ppid_cycles_terminate_without_loss() {
        let mut processes = vec![proc(1, 2, 10), proc(2, 1, 20), proc(3, 0, 30)];
        order_as_tree(&mut processes);
        let pids: Vec<i32> = processes.iter().map(|p| p.pid).collect();
        assert_eq!(pids.len(), 3);
        let mut sorted = pids.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![1, 2, 3]);
    }
    #[test]
    fn deep_chains_terminate_with_all_processes() {
        // Iterative traversal: depth is bounded only by the chain, never
        // by the call stack. 2000 links prove termination cheaply.
        let mut processes: Vec<ProcessMemory> =
            (0..2_000).map(|i| proc(i, i - 1, 10)).collect();
        processes[0].ppid = 0;
        order_as_tree(&mut processes);
        assert_eq!(processes.len(), 2_000);
        assert_eq!(processes[0].pid, 0);
        assert_eq!(forest_depths(&processes)[&1_999], 1_999);
    }

    #[test]
    fn labels_compose_across_predicates() {
        let filter = ProcessFilter {
            only_private: true,
            min_pss_bytes: 10 * 1024 * 1024,
            ..Default::default()
        };
        assert_eq!(
            filter.label().as_deref(),
            Some("private-only + min-pss≥10.0M")
        );
        assert_eq!(ProcessFilter::default().label(), None);
    }
}
