# Contributing

daisyloop is a personal project for one machine, but issues and pull requests are welcome.

- Open an issue first for larger changes. Anything touching pump or fan writes, bounds or the firmware fallback needs a test and a short safety rationale.
- Build and test: `cargo test --release` in the repository root, `tools/model_infer` and `tools/d5_probe`; `cd python && python -m pytest`. CI runs the same on Windows. Tests that need hardware or local data are `#[ignore]`d.
- Style: small focused commits, no comments or docstrings, plain dict configuration in Python, related code in one file.
- Never commit serial numbers, device GUIDs, user names, local paths or real telemetry dumps.
- Contributions are licensed under GPL-3.0-or-later.
