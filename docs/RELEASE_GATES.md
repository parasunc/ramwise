# Release gates

Measurable pass/fail gates for the roadmap. A gate passes when **every**
check is green; a gate with an open linked PR merges nothing until the PR
lands and the checks re-run on `main`.

Conventions used below:

- `cargo fmt --check`, `cargo clippy --all-targets --all-features`
  (no new warnings), `cargo test` — always required. `cargo test` runs
  every target, including the `tests/readme_assets.rs` integration suite
  that keeps README examples honest.
- "Live" checks run the built binary, never the test harness.
- Optional integrations (eBPF, VRAM) additionally require
  capability/error/performance coverage: unavailable states print reasons,
  failures are explicit, and no integration slows the default path.

## Gate 1 — Foundation: pressure, tiny output, exports, stability

| Check | Linked |
|---|---|
| Swap-rate and pressure inputs with explicit gaps; `cargo test` green | #10 / PR #33 |
| Non-interactive CLI (`--once`, `--tiny`, `--watch`); stdout pure, stderr diagnostics | #11 / PR #34 |
| Pressure classification with boundary/zero-swap/unknown tests; header + insights indicators | #12 / PR #35 |
| JSON/CSV writers with round-trip, header-stability, overwrite-refusal tests | #13 / PR #36 |
| Stable tiny contract with golden tests; live `--tiny --once` deterministic | #14 / PR #39 |
| TUI/process-control stability: existing interaction tests green (`cargo test`, app/widget suites) | CI on `main` |
| Deterministic tests: full suite passes on a clean checkout | `cargo test` |

Gate 1 passes when PRs #33, #34, #35, #36, #39 are merged and the checks
above are green on `main`.

## Gate 2 — Organization: categories, navigation, comparison, foresight

| Check | Linked |
|---|---|
| Heuristic categories with tables/overrides/aggregation tests; grouped sort | #15 / PR #37 |
| Typed filters, ppid tree with orphan/cycle tests, TUI controls | #16 / PR #38 |
| Snapshot compare with PID-reuse, threshold and back-compat tests | #17 / PR #40 |
| Bounded sparklines with bucketing/window/category/narrow-render tests | #18 / PR #41 |
| Calibrated leak score with steady/stable/shrinking/noisy/guard tests | #19 / PR #42 |
| Memory-type visualization with omission (never zero-fill) tests | #20 / PR #44 |
| User documentation: every README example runs against the CLI | #26 / PR #49 |

Gate 2 passes when PRs #37, #38, #40, #41, #42, #44, #49 are merged and
the checks above are green on `main`.

## Gate 3 — Later: alerts, actions, deep integrations

| Check | Linked |
|---|---|
| Calm mode engages under Critical pressure (deterministic via fixture: MemAvailable ≤10% of MemTotal; live via memory load); alerts honor cooldown/dedup/dry-run | #21 / PR #45 |
| External tools launch without shell interpolation; missing tools explicit | #22 / PR #46 |
| eBPF boundary: unsupported systems report reasons; lifecycle/buffer tests | #23 / PR #47 |
| VRAM boundary: mock contract tests; core runs without drivers | #24 / PR #48 |
| Optional-integration coverage: capability output, error paths and the
  default-path performance check — median of 3 cold-start `ramwise --once`
  runs within 10% of the pre-PR baseline on the same 2+ core machine | live runs |

Gate 3 passes when PRs #45, #46, #47, #48 are merged and the checks above
are green on `main`.

## Capability / error / performance coverage (gates 1–3, integrations)

- Every unavailable input prints its reason (`unknown`, `not configured`,
  `not on PATH`, backend reasons) — verify with:
  `ramwise --once | grep -o '"swap_in_rate_per_sec":[^,]*'` → `null`
  (not `0`) on a first sample, and `--trace-alloc` / `--vram` naming each
  missing backend.
- Every failure path exits non-zero with the cause on stderr; verify with
  bad flag combos, missing files, and schema mismatches.
- `time ramwise --once` before/after each integration PR: no measurable
  cold-start regression on machines without the optional hardware.
