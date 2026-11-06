# Step test session runbook

Goal: one evening with the owner at the computer, then the model knows how the fan and the pump change the water temperature. Nothing here was run on the real machine; the retraining and the gate were run on synthetic data only (section 9). Every command below is for the owner's PowerShell. Rules that hold for the whole session:

- The owner sits at the computer from step 2 to step 6 and watches the UI (water temperature, `Stufentest` line, fan and pump rpm). Leaving the computer is an abort.
- No game or other GPU/CPU load besides the load generator described in step 4.
- `writes_enabled` is the only gate for any D5 write. It is `false` before and after the session.

## Cold start requirement

The session starts cold: the computer has been idle for at least 30 min (or off for 30 min and then booted and left idle for 10 min), water at or below 31 C, fan and pump on the firmware PID, LibreHardwareMonitor sidecar running (the UI shows CPU and GPU power and temperature; the training needs them).

Why: the safety minimum curve of the fan (`min_curve` 35 C 20 %, 38 C 54.4 %, 42 C 100 %) overrides any step duty below it (`step_duty` takes the maximum of the step and the bound). A step of 27 % is only honoured while the water is below 35.6 C, 40 % below 36.7 C, 55 % below 38.1 C, 75 % below 39.9 C. The design water temperature of 36.6 C in `docs/thermal.md` 4.4 would therefore force the low levels up. The loads below are set for a water temperature of about 34.5 C so that the commanded levels hold; starting warm leaves no room. The fan stop duty test also needs the water below 33 C to be meaningful.

## 1. Preparation without any write (15 min)

- Rehearse the retraining once on the dry run: `python python\steptrain.py dryrun` (section 9). It needs the Python environment with numpy, scipy, xgboost, duckdb, pyarrow, pytz and ray, and a running Ray cluster that this Python can join (`ray status` must work; the ray version of the environment must equal the cluster's). The job declares 4 cores and 6 GB.
- Check the model and the data path: UI shows model loaded, `warm`; the lake has the LHM channels of the last hour.
- Decide the load generator: a GPU burn with adjustable power and a CPU stress with adjustable threads, both stoppable in one click. Heat at the water is about 0.96 x CPU package power + GPU power + 4 to 18 W pump.
- Expected duration 15 min. Abort: nothing is written yet, stop at any time.

## 2. Enable writes and the step plans, restart (needs UAC, 5 min)

1. Elevated edit of the admin config: `Start-Process notepad 'C:\Program Files\daisyloop\config.toml' -Verb RunAs`. Windows asks for UAC. Keep the existing `data_dir` line and set:

```
writes_enabled = true
[control.step_test]
fan = [[0.0, 120.0]]
pump = [[15.0, 120.0]]
```

   This is the plan for the duty test (step 3). `control` is an admin key, a user config cannot set it.
2. Clean restart: `New-Item C:\Users\user\AppData\Local\daisyloop\stop -ItemType File`. The daemon stops (it releases its channels, seals and commits the hot store); the watchdog task relaunches it within about 2 min. Because the model is seeded from the hot store, the restart no longer costs the 30 min warm-up: the UI shows the model warm again as soon as the daemon is back (if the gap was longer than 300 s the old warm-up applies).
3. Expected: UI shows `Schreiben erlaubt` on both channels, mode `shadow`, drive `firmware`, model warm. If the UI shows `Schreiben ist deaktiviert`, the config was not saved.
- Watch: water temperature, both channels on `firmware`. Abort: set `writes_enabled = false` again (UAC) and restart; nothing was written.
- Restore: same edit, `writes_enabled = false`, stop file, wait for the watchdog.

## 3. Duty test (fan 0 %, pump 15 %, 10 min)

Purpose: learn what the D5 really does at these two settings before the long test relies on them.

1. UI, fan card, `Stufentest starten ...`, `Ja, starten`. The fan goes to 0 % for 120 s and returns to the firmware PID. Read: `d5next/fan/rpm` (UI actual rpm) after 30, 60 and 120 s. Expected: 0 rpm or the lowest the fan header allows; the UI shows `Stufentest, Duty 0 %`. If the water was 35 C or more the bound lifts the duty and the UI shows the lifted duty: that is the safety curve working, not a failure.
2. Wait 3 min (water must not rise by more than 1.5 K/min; abort below).
3. UI, pump card, same buttons. The pump goes to 15 % for 120 s. Read `d5next/pump/rpm` after 30, 60 and 120 s. Expected: roughly 1800 to 2000 rpm (calibration says 2432, the measured floor is about 1835). Below 1800 rpm the daemon raises `pump_low` and falls back to the firmware PID by itself; note the value.
4. Whether the firmware takes over at 48 C: not tested and not testable without risk. It would need water at 48 C, 4 K above the abort limit and 2 K below the coolant limit of the manual (50 C). What exists: the daemon's own takeover at `water_limit` 48 C is covered by the simulation tests (`cargo test safety_tests`, `cargo test fan_stop_tests`), the device-side behaviour is unknown. Do not try.
5. Whether a SET survives a D5 power cycle: optional, only if all of the following hold, otherwise skip it (the answer is not needed for the model): idle computer, water at or below 31 C, owner present, `writes_enabled = true` stays on for the check, and the owner accepts that for the minutes between boot and the daemon start the fan may stand at 0 %. Procedure: start the fan step (0 %), and during its 120 s hold shut Windows down; switch the PSU off for 15 s (this cuts the pump and the D5), on again, boot. Read the fan duty and rpm in the UI as soon as it is up, before the daemon has been running 2 min. Result A: fan at the firmware PID (the SET did not survive: a daemon crash plus a power cycle is safe). Result B: fan still at 0 % (the duty survives power loss: the daemon restores it on start because the channel is marked as owned and writes are enabled; check that it does within 3 min, otherwise restore by hand as in "If the PC crashes"). Without a power cycle nothing in this item is run.
- Abort (any): water at or above 38 C at idle, rpm of the pump below 1500, no response of the UI for 10 s. Press `Abbrechen` in the step card (command `stop_step_test`, releases to the firmware PID at once). If the UI does not answer: step 2 restore (stop file).

## 4. Step test, 64 min plus 15 min cool-down

Replace the plan (elevated edit as in step 2, keep `writes_enabled = true`), clean restart, wait for the model to be warm and both channels on `firmware`:

```
[control.step_test]
fan = [[100.0, 300.0], [75.0, 480.0], [55.0, 480.0], [40.0, 480.0], [27.0, 480.0], [100.0, 300.0], [100.0, 300.0]]
pump = [[15.0, 300.0], [100.0, 180.0], [15.0, 180.0], [100.0, 180.0], [15.0, 180.0]]
```

The step test is per channel: the fan plan and the pump plan are two separate starts. The channel that is not tested stays on the firmware PID (fan at full speed while water is above its 30 C set-point; pump on its floor at about 20 %).

### 4a. Fan test, 47 min

Start: UI fan card, `Stufentest starten ...`, `Ja, starten`. Phases of the plan and the load the owner sets by hand (the daemon does not control the load; targets for a water temperature of about 34.5 C with the pump at its floor, G from `docs/thermal.md` 4.4, adjust to what the thermometer says):

| Phase | Fan duty | Time | Load (CPU + GPU power) |
|---|---|---|---|
| 1 lead | 100 % | 5 min | start the load, raise to about 250 W, water rises toward 34 C |
| 2 | 75 % | 8 min | about 150 W |
| 3 | 55 % | 8 min | about 90 W |
| 4 | 40 % | 8 min | about 55 W |
| 5 | 27 % | 8 min | idle load only (about 30 W is below what the idle machine makes) |
| 6 | 100 % | 5 min | 250 W again |
| 7 | 100 % | 5 min | half of that, about 125 W (separates sink temperature from load scale) |

The loads are held at constant values inside a phase, changed within 30 s of the phase change (the UI line `Stufe n von 7 ... noch s s` is the clock). Steady state is not needed; the fit uses the whole curve with the dead time. If the water crosses 35.5 C at phase 2 to 5 the commanded duty is overridden by the safety curve and recorded as such (the fit uses the recorded rpm); reduce the load to bring it back under, do not chase exact numbers.

- Watch: water temperature (limit below), fan rpm following the duty within about 10 s, CPU and GPU die temperatures, `Stufentest` line.
- Abort limits (any one): water at or above 44 C; water rising faster than 1.5 K/min at a level for more than 2 min; CPU package at or above 95 C or GPU at or above 80 C (these two are my choice for "within 5 K of its limit", the owner adjusts to the cards); no telemetry update for 10 s (UI shows stale); owner leaves.
- Abort action: stop the load first, then `Abbrechen` in the fan card. The fan returns to the firmware PID, which drives it to 100 % above 30 C. The daemon does the same on its own at `water_limit` 48 C and on sensor faults, but the owner's abort at 44 C comes first.
- Rest 5 min after phase 7 (load off, fan on firmware), longer if the water is above 33 C. The rest must be at least 3 min, otherwise the two tests merge into one window for the retraining.

### 4b. Pump test, 17 min

Start: UI pump card. Fan stays on the firmware PID at full speed. Load about 250 W constant for the whole test (full load). Phases: 5 min at the pump floor 15 % (lead), then 100 %, floor, 100 %, floor for 3 min each. Watch pump rpm (about 1835 at 15 %, 4800 at 100 %), `pump_low` alarm, water. Abort limits as in 4a, plus pump rpm below 1500. Abort action: stop the load, `Abbrechen` in the pump card.

### 4c. Cool-down, 15 min

Load off, both channels on `firmware`, wait until the water is below 31 C. The cool-down data is part of the training window (900 s after the window are used).

## 5. Close the session and commit the data (10 min)

1. Check in the UI that both channels show drive `firmware` and the log line `back on the firmware PID` for both (`%LOCALAPPDATA%\daisyloop\collector.log`). Do not continue if a channel is still on `duty` or `test`: set `writes_enabled` only to false after the release, because a daemon started with writes disabled does not release a channel it owns (`start_releases_only_a_channel_marked_as_owned_and_only_with_writes_enabled`).
2. Elevated edit: `writes_enabled = false`, remove the `[control.step_test]` block (the defaults of the code are not used for the evening). Clean restart with the stop file. The clean stop commits the hot store to the lake, so the step test rows (`daisyloop/step/phase`, `daisyloop/step/duty`) are in the lake now.

## 6. Retrain (3 to 10 min)

Work on a copy, never on the live data:

```
robocopy "$env:LOCALAPPDATA\daisyloop" C:\steptest-copy /E /XD hot
$env:DAISYLOOP_DATA_DIR = 'C:\steptest-copy'
cd C:\clones\daisyloop\python
python -c "from rawstore import RawStore; RawStore().build(force=True)"
python steptrain.py
```

`rawstore` rebuilds the 1 Hz matrix in `%LOCALAPPDATA%\daisyloop-ml\cache` from the copy. `steptrain.py` finds the windows from the phase channel (or `windows` in its config dict), trains, runs the identification and the gate through the Ray cluster (4 cores declared) and writes `models\steptrain\`:

- `model_deploy.json` and `feature_spec_deploy.json` if the gate passes, `candidate_model_deploy.json` and `candidate_feature_spec_deploy.json` if not;
- `report.json` with the gate verdict, the reasons, the identification of fan and pump (excitation, fitted plant, counterfactual curves with intervals), the holdout comparison against persistence and against the deployed model, and the weights of the step rows.

The gate (also written in the `criteria` field of the report): ship only if fan and pump are identified with the physically right sign and the water 2 min RMSE on the old test block is not worse than the deployed model's. A refusal is not a failure of the session: read `reasons`, deploy nothing, and the deployed model keeps running exactly as before.

## 7. Deploy the model (5 min, needs UAC)

```
Copy-Item C:\clones\daisyloop\models\steptrain\model_deploy.json, C:\clones\daisyloop\models\steptrain\feature_spec_deploy.json C:\clones\daisyloop\models\
Start-Process powershell -Verb RunAs -ArgumentList "-Command Copy-Item C:\clones\daisyloop\models\model_deploy.json,C:\clones\daisyloop\models\feature_spec_deploy.json 'C:\Program Files\daisyloop\models' -Force"
New-Item C:\Users\user\AppData\Local\daisyloop\stop -ItemType File
```

After the relaunch the UI shows the model loaded and warm (seeded) and `fan identified` / `pump identified` as in the artifact. Active mode still needs `writes_enabled = true`, which stays false until the owner decides.
Restore: copy the previous `model_deploy.json` and `feature_spec_deploy.json` from git (`git show HEAD:models/model_deploy.json`) the same way.

## 8. Writes back to false

`writes_enabled = false` is already set in step 5 and must be checked again after the deploy: `Select-String writes_enabled 'C:\Program Files\daisyloop\config.toml'` shows `false`; the UI shows `Schreiben ist deaktiviert`. Remove `C:\steptest-copy` when the report is archived.

## If the PC crashes or hangs mid-test

- The D5 has no known timeout in the direct duty mode (`snapshot/d5-control.md`). A dead daemon leaves the last duty on the device: during a fan test that can be 27 % while the computer is hung. A crash of the daemon alone is relaunched by the watchdog task within about 2 min; on start it releases every channel it owns, but only when `writes_enabled = true`.
- Owner actions, in this order: (1) stop the heat: stop the load, or if the screen is frozen switch the PSU off (this also stops the pump, nothing produces heat any more); (2) boot; (3) keep `writes_enabled = true` until the UI shows both channels on `firmware` and `collector.log` shows `back on the firmware PID`; (4) only then set `writes_enabled = false` (step 5). If the daemon does not come back, start `C:\Program Files\daisyloop\daisyloop.exe` from the install directory or run the watchdog task once (`Start-ScheduledTask daisyloop-watchdog`).
- A crashed session produces a partial window in the lake. The retraining then finds a window whose fan span or pump span is too small and the gate refuses ("not identified"); repeat the missing test on another evening.

## 9. Dry run on synthetic data (what was verified)

`python python\steptrain.py dryrun` injects a step window generated from the plant of `docs/thermal.md` (two-lump heat capacity, fan exponent m, pump coefficient gamma, dead time 31 s at the pump floor and 11 s at 100 %, fan spool 4.5 s, sensor noise 5 mK, load noise 8 W) into a copy of the real lake matrix (4 days from 2026-11-01), with the fan test protocol of section 4a (`matched`) and the rejected constant-load protocol (`constant`), each with and without a fan response (m set to 0). Results are written to a temp directory. The numbers of the last run are in `docs/model.md` ("Step test retraining").
