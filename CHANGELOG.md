# Changelog

## Unreleased

Documents the roadmap tracked in issues #10–#27 (closed #25 shipped
deterministic collector/analyzer/UI coverage first):

- Deterministic test coverage for collector, analyzer and UI (#25)

- Swap-rate and pressure inputs with explicit capability gaps (#10)
- Non-interactive CLI: `--once`, `--tiny`, `--watch` (#11)
- Memory-pressure classification and indicators (#12)
- Versioned JSON/CSV snapshot exports (#13)
- Stable tiny status-bar output (#14)
- Heuristic process categories (#15)
- Memory filters and process-tree navigation (#16)
- Snapshot comparison with PID-reuse safety (#17)
- Bounded sparklines and history views (#18)
- Calibrated 0–100 leak score (#19)
- Memory-type visualization and accounting notes (#20)
- Calm mode and alert foundations (#21)
- External-tool actions (#22)
- eBPF tracing boundary (#23)
- VRAM provider boundary (#24)
- Documentation and release gates (#26, #27)

## v0.1.0

Initial release: TUI process monitor with rule-based insights, process
control, themes, and the versioned snapshot export contract.
