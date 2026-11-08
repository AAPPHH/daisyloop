# daisyloop

daisyloop is a Windows daemon with a desktop UI. It monitors the sensors of a PC through LibreHardwareMonitor and Aqua Computer HID devices and controls the pump and fan of a D5 NEXT water loop. Control is driven by a learned water-temperature model with an online RLS correction, a planner that picks fan and pump set points, and a fallback to the device firmware.

Only the water temperature is forecast and planned. There are no CPU or GPU temperature targets.

## Architecture

- `src/`: Rust daemon. Reads sensors, stores telemetry (hot store plus Iceberg/DuckDB lake), runs the model, the online correction and the planner, writes HID reports, serves the UI over a local IPC protocol.
- `sidecar/`: C# sidecar that exposes LibreHardwareMonitor readings to the daemon.
- `ui/`: Tauri desktop UI (web front end in `ui/web`, protocol in `ui/PROTOCOL.md`).
- `python/`: training and analysis. `steptrain.py` fits the step-test model, `steprun.py` and `stepsim.py` run and simulate step tests, `features.py`, `metastore.py`, `metaset.py` hold features and metadata.
- `tools/model_infer`: Rust inference crate for the exported model.
- `tools/d5_probe`: probe tool for the D5 NEXT HID interface.
- `models/`: deployed model, feature spec and golden test vectors.
- `docs/`: model, thermal, lakehouse and step test documentation.

## Requirements

- Windows 11
- LibreHardwareMonitor
- Aqua Computer D5 NEXT (USB VID/PID 0C70:F00E)
- Rust toolchain, .NET 8 SDK (installed by `sidecar/build.ps1` if missing), Python with `python/requirements.txt` for training

## Build and install

1. `cargo build --release`
2. `sidecar/build.ps1`
3. Elevated PowerShell: `scripts/install-service.ps1`. It installs to `%ProgramFiles%\daisyloop`, registers a watchdog scheduled task running as SYSTEM and verifies the new collector before removing the old setup.
4. Edit `config.toml` in `%ProgramFiles%\daisyloop` (admin only). Set `writes_enabled = true` to let the daemon write to the devices. It is `false` after installation. Security-relevant keys are only read from this file, not from the user config.
5. UI: `cd ui && cargo build --release`.

## Models are specific to one loop

The shipped models and golden vectors were fitted on one specific loop (its radiators, pump, fans, loads and sensors). Before active control on any other system, retrain with `python/steptrain.py` on data from your own loop, recorded with the step test described in `docs/steptest.md`. Without that, the model predictions and the planner output do not apply to your hardware.

## Language

The UI and the daemon's user-facing messages are in German. Code and documentation are in English.

## Status

Working: monitoring, model, online correction, planner, firmware fallback, UI, test suites. Active fan and pump control has been run on a single machine only. There is no support for other pump or fan controllers. See `SECURITY.md` before enabling writes.

## License

GPL-3.0-or-later, see `LICENSE`. Third-party assets keep their own licenses (for example `ui/web/fonts/OFL.txt`).
