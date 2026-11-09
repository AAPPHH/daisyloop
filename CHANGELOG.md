# Changelog

Grouped by date from the git history. The project has no tagged releases.

## 2026-11-09

- Sync with the private development tree: water target 32 to 36 C replaces the quiet-cool slider (persisted, fan zero-rpm hysteresis follows the target), protocol v5.
- UI: allowed target range comes from the daemon, dev fixture and mock follow the target.
- README rewritten with architecture diagram and screenshots from the dev mock, CI workflow, issue and pull request templates, contributing guide, documentation index.

## 2026-11-08

- Public export: GPL-3.0-or-later license in all crates, README, SECURITY policy, paths via environment variables, personal identifiers removed.
- Sidecar robust to disk load: fast and slow LibreHardwareMonitor groups, start grace, slow-sensor freshness.
- No warm-up after restart or input gaps (eager feature engine, gap bridging), two modes per channel (`PID`, `Modell`), persisted mode choice.

## 2026-11-07

- Water is the only forecast and plan constraint. Training, export, gate, daemon and UI follow (protocol v3).
- Online RLS correction of the water forecast per horizon with bounds, persistence and drift alarm.
- Planner refines the fan rpm by bisection and removes the two-level limit cycle.
- Step test trainer: grey-box fan and pump response, post hoc gate, two-lump estimate of the conductance, dry runs on synthetic data.
- History store (swinging-door compression of all channels, daily incremental update, scheduled task) and metaset (stratified, deduplicated, weighted training subset).
- Stale input handling: only fallback-block inputs hold the plan.

## 2026-11-06

- Operator tool `steprun` with load generator, watchdog and abort chain; daemon-independent abort in `d5_probe`.
- Step model with explicit fan and pump response and dead time in `model_infer`, golden vectors for the deployed pair.
- Linear models for 1 Hz control.

## 2026-11-05

- Rules for restart, stall, frozen water, rule bounds and plan limits; fan may stand still.
- Retraining after the step test with synthetic data, docs for the step test runbook.

## 2026-11-04

- Large first iteration: WAL durability, hot Arrow IPC store with hourly seal and idle-gated Iceberg rotation, DuckDB 2.0 lake reads.
- Supervised .NET sidecar for LibreHardwareMonitor, watchdog and SYSTEM service installer with admin-only config under Program Files.
- D5 NEXT HID layer, guarded fan and pump set-point control, model-predictive rpm control with safety bounds and firmware takeover.
- Tauri 2 UI with loop schematic, per-channel modes, forecast, alarms and stale greying, named-pipe protocol and mock daemon.
- Spec-driven causal feature pipeline, Ray experiments, fan identifiability analysis, Rust `model_infer` crate with golden tests.
- Thermal model, lakehouse and hardware documentation.

## 2026-11-01

- Initial repository: read-only system snapshot, crate skeleton, LibreHardwareMonitor, D5 NEXT and NVML sources, Iceberg writer, collector loop and logon task installer.
