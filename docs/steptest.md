# Step test session runbook

Goal: one evening with the owner at the computer, then the model knows how the fan and the pump change the water temperature. Nothing here was run on the real machine; the retraining and the gate were run on synthetic data only (section 9). Every command below is for the owner's PowerShell. Rules that hold for the whole session:

- The owner sits at the computer from step 2 to step 6 and watches the UI (water temperature, `Stufentest` line, fan and pump rpm). Leaving the computer is an abort.
- No game or other GPU/CPU load besides the load generator described in step 4.
- `writes_enabled` is the only gate for any D5 write. It is `false` before and after the session.
- The firmware fan set-point on this machine was raised from 30 C to 34 C on 2026-11-05/06 with `d5_fan.exe` (old reports saved as `snapshot\raw\d5_pre_*.bin`). Everything below that says 30 C (cold start, the firmware PID at full speed above 30 C, set-point 30 C in section 0) assumes the 30 C set-point. Before the session set it back to 30 C, after the session set it to 34 C again. From the repo root, owner at the computer, water below 40 C and the pump rpm steady (the tool refuses otherwise): `$env:D5_SEND='YES'; $env:D5_FAN_SETPOINT='3000'; $env:D5_RESTORE='NO'; tools\d5_probe\target\release\d5_fan.exe`, and after the session the same with `'3400'`. Without `D5_RESTORE='NO'` the tool puts the old set-point back after `D5_HOLD` seconds (60 by default), so the change would not stay. Without `D5_SEND='YES'` it is a dry run and sends nothing. It saves the old report as `snapshot\raw\d5_pre_<unix time>.bin`, reads the report back and prints `fan set-point now`; the set-point is in 1/100 C (3000 = 30 C, allowed 2800 to 3800). Remove the `D5_*` variables from the PowerShell session afterwards (`Remove-Item Env:D5_SEND, Env:D5_FAN_SETPOINT, Env:D5_RESTORE`).

## Cold start requirement

The session starts cold: the computer has been idle for at least 30 min (or off for 30 min and then booted and left idle for 10 min), water between 30.0 and 31.0 C when step 3 starts, fan and pump on the firmware PID, LibreHardwareMonitor sidecar running (the UI shows CPU and GPU power and temperature; the training needs them).

Start band 30.0 to 31.0 C: the dry run (section 9) was run for start waters of 30, 30.5 and 31 C and the criteria were met there only. How to get there: below 30.0 C, run a light load (CPU stress at about 100 W, fan and pump on the firmware PID) until the reading is 30.0 C or more and stop it the moment it is, then start step 3 within 2 min; above 31.0 C leave the machine idle with the fan on the firmware PID until the reading is 31.0 C or less. Do not start the session outside the band: wait or heat, the band costs a few minutes.

Why: the safety minimum curve of the fan (`min_curve` 35 C 20 %, 38 C 54.4 %, 42 C 100 %) overrides any step duty below it (`step_duty` takes the maximum of the step and the bound). A step of 27 % is only honoured while the water is below 35.6 C, 40 % below 36.7 C, 55 % below 38.1 C, 75 % below 39.9 C. The design water temperature of 36.6 C in `docs/thermal.md` 4.4 would therefore force the low levels up. An idle machine still makes about 35 to 70 W CPU plus GPU (5th percentile 34.2 W in the real extract) plus 4 W pump, so the low levels cannot be held by lowering the load. They are held by starting cold and by ordering the loads so that the water stays between about 28 and 35.6 C (two-lump plant of `docs/thermal.md` 4.4 with the safety curve, ambient 25 C, the dry run of section 9); starting warm leaves no room. The fan stop duty test also needs the water below 33 C to be meaningful.

## 0. Abort command, independent of the daemon

Source: `tools\d5_probe\src\bin\d5_abort.rs`, built once during preparation with `cd C:\clones\daisyloop\tools\d5_probe; cargo build --release --bins`. Run it with the daemon dead, hung or running: `C:\clones\daisyloop\tools\d5_probe\target\release\d5_abort.exe`. It is the first action on every abort and after every crash.

- It writes the stop file `stop` in the service's `data_dir` (read from the service config, else `DAISYLOOP_DATA_DIR`, else `%LOCALAPPDATA%\daisyloop`; a failed write makes the outcome FAILED even when the fan ends on the firmware PID) and waits 6 s so that a living daemon releases first. It then reads the D5 configuration report 0x03 (809 bytes, CRC checked), sets the mode byte of the fan (0x41) and of the pump (0x96) to 1 (firmware PID, set-point mode), recomputes the CRC, sends it and reads it back after 500 ms. Only those two bytes and the CRC may change; the duty presets and the set-points (fan 0x46, pump 0x9B, normally 30 C and 45 C) are left as they are and printed. Failed reads or writes are retried up to 8 times. It then watches the device for 40 s, retrying failed reads and writes, re-opens the device after a failed session (3 sessions at most) and overrules a daemon that writes duty mode again. Expect about 46 s or more until the verdict; the fan is usually back on the firmware PID within the first seconds after the 6 s wait, so do not cut power just because the verdict is still pending.
- Success is the line `OK fan and pump are on the firmware PID`. Exit code 1 and `FAILED` otherwise: switch the PSU off to stop the heat and set both to PID by hand in aquasuite.
- Tested only against a fake HID layer (`cargo test` in `tools\d5_probe`): duty mode at 0 % returns to mode 1 on the real report layout, retries, a daemon that rewrites duty, an ignored SET, bad CRC and length refused, nothing sent when already on firmware. It has never been run against the device.
- Daemon side: the daemon marks every channel it puts in duty mode in `%LOCALAPPDATA%\daisyloop\d5-owned` and clears the mark after the release. On start with `writes_enabled = true` it returns the fan to the firmware PID whenever it finds it in duty mode, and the pump when marked. With `writes_enabled = false` it never writes (that flag is the only gate for any D5 write): a marked channel in duty mode raises the `Foreign` alarm "sofort d5_abort.exe ausführen", an unmarked one tells to set the PID in aquasuite.

## 1. Preparation without any write (15 min)

- Rehearse the retraining once on the dry run: `python python\steptrain.py dryrun` (section 9; arguments `dryrun [directory] [case,case] [seeds]`). It needs the Python environment with numpy, scipy, xgboost, duckdb, pyarrow and pytz. It runs as separate worker processes of one core each (5 by default, `workers` in the `CFG` dict), no Ray; one case takes about 20 s.
- Check the model and the data path: UI shows model loaded, `warm`; the lake has the LHM channels of the last hour.
- Build the abort command of section 0 and run `cargo test` in `tools\d5_probe` (no device access); keep a PowerShell window open at the build directory for the whole session.
- The load generator and the abort watcher is `python python\steprun.py <stage> [go]` (section 1a): CPU stress with adjustable threads plus a GPU burn, both on a closed loop and stopped by Ctrl-C. Heat at the water is about 0.96 x CPU package power + GPU power + 4 to 18 W pump.
- Rehearse it without any D5 write: `python python\steprun.py status` prints the live values and the pipe status, `python python\steprun.py heat go` runs the 100 W CPU load until the water reaches 30.0 C (the start band) and is the way to tune the closed loop.
- Expected duration 15 min. Abort: nothing is written yet, stop at any time.

### 1a. The operator tool `python\steprun.py`

Source `python\steprun.py`, tests `python\test_steprun.py` (fake telemetry, fake pipe, fake abort runner; nothing touches the device). Run it from the owner's own interactive PowerShell in `C:\clones\daisyloop\python`: the pipe `\.\pipe\daisyloop` is open to interactive logon sessions only, a scheduled task, a service or an SSH session gets access denied. The stage word is the first argument; every stage that starts something also needs the word `go`:

| Command | What it does |
|---|---|
| `python steprun.py status` | prints the live values and the pipe status (controls, drive, allowed, alarms); no action, no watchdog |
| `python steprun.py heat go [30.0]` | 100 W CPU load until the water reaches the value (default 30.0 C), then load off; refuses when the water is already there |
| `python steprun.py duty go` | step 3: StartStepTest on `d5.fan` (plan 0 %, 120 s), the 3 min rest of the table, then StartStepTest on `d5.pump`; load off; water band 30.0 to 31.0 C |
| `python steprun.py fan go` | step 4a: StartStepTest on `d5.fan`, then the load of the table for each value of `daisyloop/step/phase` (set within 1 to 2 s of the change), 5 min rest with the load off afterwards; water band 27.0 to 31.0 C |
| `python steprun.py pump go` | step 4b: StartStepTest on `d5.pump`, 200 W for all phases; water band 27.0 to 33.0 C |
| `python steprun.py cool` | step 4c: load off, watches until the water is below 31.0 C, prints the drive of both channels |

The load table is `SIM["inject"]["protocols"]["runbook"]` in `python\stepsim.py`; the tool reads it, a load at or below its `idle_w` (70 W) means the load generator is off. The load is the total of CPU package power and GPU power read from the newest `%LOCALAPPDATA%\daisyloop\hot\telemetry-*.arrows`: up to 100 W on the CPU (8 worker processes with PWM duty, `load.cpu_threads` in the `CFG` dict), the rest on the GPU (torch CUDA, PWM duty), regulated at 1 Hz. If the CPU temperature reaches 90 C the CPU share is reduced by 10 W per second and moved to the GPU (the GPU share is reduced the same way at 75 C). Children die with the tool (Windows job object plus a liveness check of the parent); the GPU worker is stopped, which frees its memory, when its share has been zero for 20 s or the load is halted. The closed loop gains in `CFG["load"]` are not tuned on the real machine: tune them with the `heat` stage first.

A stage refuses to start (exit code 2, reasons in the log) unless: the word `go` is given, telemetry is fresh (every safety channel under 10 s old), the water is inside the band of the stage, both channels are on `firmware`, no step test runs, writes are allowed and the tested channel is not in active mode, and the foreign load (CPU plus GPU power minus the tool's own load) is at most `foreign_max_w` (90 W). If the daemon's plan has a different number of phases than the table, the tool aborts at phase 1.

Watchdog, own thread at 1 Hz, independent of the load loop: water at or above 44 C; water rising faster than 1.5 K/min (slope over 60 s) for more than 2 min; CPU package (`/intelcpu/0/temperature/0`) at or above 95 C or GPU (`nvml/0/temp`) at or above 80 C; pump rpm below 1500; one of these channels older than 10 s or missing. On a hit it stops the load first, then sends StopStepTest through the pipe and checks the status for up to 15 s; if the tested channel is not on firmware by then, or the pipe fails, it runs `tools\d5_probe\target\release\d5_abort.exe` and logs its verdict (d5_abort writes the stop file, so the daemon stops and the watchdog task relaunches it). Ctrl-C is the same abort. The tool prints one line per phase change and per abort and a status line every 30 s and appends everything to `%LOCALAPPDATA%\daisyloop\steprun\steprun-<time>.log`. If the tool itself dies, the abort command of section 0 is the first action.

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
- Abort (any): water at or above 38 C at idle, rpm of the pump below 1500, no response of the UI for 10 s. Press `Abbrechen` in the step card (command `stop_step_test`, releases to the firmware PID at once). If the UI does not answer, or a release failed, the first action is the abort command of section 0, before anything else. The old advice (`writes_enabled = false` plus the stop file) is wrong for this case: a daemon relaunched with writes off does not touch a channel left in duty mode, so the fan would stay at 0 %.

## 4. Step test, 64 min plus 15 min cool-down

Replace the plan (elevated edit as in step 2, keep `writes_enabled = true`), clean restart, wait for the model to be warm and both channels on `firmware`:

```
[control.step_test]
fan = [[100.0, 300.0], [75.0, 480.0], [55.0, 480.0], [40.0, 480.0], [27.0, 480.0], [100.0, 300.0], [100.0, 300.0]]
pump = [[15.0, 300.0], [100.0, 180.0], [15.0, 180.0], [100.0, 180.0], [15.0, 180.0]]
```

The step test is per channel: the fan plan and the pump plan are two separate starts. The channel that is not tested stays on the firmware PID (fan at full speed while water is above its 30 C set-point; pump on its floor at about 20 %).

### 4a. Fan test, 47 min

Start: `python steprun.py fan go` (or the UI fan card, `Stufentest starten ...`, `Ja, starten`, with the load set by hand). Phases of the plan and the load (the daemon does not control the load, `steprun.py` sets it from this table; the loads are the ones of the dry run, `SIM["inject"]["protocols"]["runbook"]` in `python/stepsim.py`, and a test compares this table with it; adjust to what the thermometer says):

| Phase | Fan duty | Time | Load (CPU + GPU power) |
|---|---|---|---|
| 1 lead | 100 % | 5 min | 250 W (full load, CPU stress plus GPU burn) |
| 2 | 75 % | 8 min | 100 W (a light load) |
| 3 | 55 % | 8 min | 75 W (lighter still) |
| 4 | 40 % | 8 min | 70 W (idle machine, nothing running; lower is fine, 35 W in phases 4 and 5 shipped 3 of 3 in the dry run) |
| 5 | 27 % | 8 min | 70 W (idle machine, nothing running) |
| 6 | 100 % | 5 min | 250 W (full load) |
| 7 | 100 % | 5 min | 125 W (half of that, separates sink temperature from load scale) |

Why a 250 W lead: phases 6 and 7 are held out of the fit and predicted by it (section 6). With a 120 W lead the held-out 250 W of phase 6 lay far outside the loads in the fit (70 to 120 W) and the check was a load extrapolation: in the dry run (4 seeds each, hold-out limit 0.15 K at the time) 4 of 4 shipped at a start of 30 C, 1 of 4 at 30.5 C and 0 of 4 at 31 C (hold-out RMSE 0.07 to 0.29 K against the limit); with a restart gap 4 of 4 at 30.5 C. With the lead the fit contains 250 W and 4 of 4 shipped at every start, with and without the gap (hold-out RMSE 0.05 to 0.06 K).

The plant model gives these water temperatures at the end of phases 1 to 5 with these loads: 31.8, 31.7, 32.3, 33.6 and 35.6 C (about 28.4 C at the start of phase 1 after the idle wait). At 27 % the safety curve lifts the duty at 35.6 C and the water rises at about 0.004 K/s there, so phase 5 holds for the full 8 min only if the idle heat is at most about 76 W in total (CPU plus GPU about 72 W). At 80 W it crosses after about 7 min: the duty is then overridden for the last minute and recorded as such, the fit loses that minute of excitation, nothing unsafe happens. The load cannot be lowered below idle, so there is nothing to do in phase 5 except watch; if the water is above 34.5 C when phase 5 starts, accept the override.

The loads are held at constant values inside a phase, changed within 30 s of the phase change (the UI line `Stufe n von 7 ... noch s s` is the clock). Steady state is not needed; the fit uses the whole curve with the dead time. If the water crosses 35.5 C in phases 2 to 4 the commanded duty is overridden by the safety curve and recorded as such (the fit uses the recorded rpm). Once the bound has lifted the duty it holds it for a while (latch), so reducing the load does not bring the level back within the phase; reduce the load anyway and do not chase exact numbers.

- Watch: water temperature (limit below), fan rpm following the duty within about 10 s, CPU and GPU die temperatures, `Stufentest` line.
- Abort limits (any one): water at or above 44 C; water rising faster than 1.5 K/min at a level for more than 2 min; CPU package at or above 95 C or GPU at or above 80 C (these two are my choice for "within 5 K of its limit", the owner adjusts to the cards); no telemetry update for 10 s (UI shows stale); owner leaves.
- Abort action: stop the load first, then `Abbrechen` in the fan card. The fan returns to the firmware PID, which drives it to 100 % above 30 C. If the UI does not answer or the fan card does not show `firmware`, run the abort command of section 0 immediately; it does not need the daemon. The daemon does the same on its own at `water_limit` 48 C and on sensor faults, but the owner's abort at 44 C comes first.
- Rest 5 min after phase 7 (load off, fan on firmware), longer if the water is above 33 C. The rest must be at least 3 min, otherwise the two tests merge into one window for the retraining.

### 4b. Pump test, 17 min

Start: UI pump card. Fan stays on the firmware PID at full speed. Load 200 W constant for the whole test. Phases: 5 min at the pump floor 15 % (lead), then 100 %, floor, 100 %, floor for 3 min each. Watch pump rpm (about 1835 at 15 %, 4800 at 100 %), `pump_low` alarm, water. Abort limits as in 4a, plus pump rpm below 1500. Abort action: stop the load, `Abbrechen` in the pump card.

### 4c. Cool-down, 15 min

Load off, both channels on `firmware`, wait until the water is below 31 C. The cool-down data is part of the training window (900 s after the window are used).

## 5. Close the session and commit the data (10 min)

1. Check in the UI that both channels show drive `firmware` and the log line `back on the firmware PID` for both (`%LOCALAPPDATA%\daisyloop\collector.log`). Do not continue if a channel is still on `duty` or `test`: run the abort command of section 0 and check for its `OK` line. Set `writes_enabled` to false only after the release, because a daemon started with writes disabled does not release a channel it owns (`start_releases_only_a_channel_marked_as_owned_and_only_with_writes_enabled`); it only raises the `Foreign` alarm that names the abort command (`a_step_test_abort_whose_release_fails_leaves_duty_until_the_abort_command_or_a_writes_on_start`).
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

`rawstore` rebuilds the 1 Hz matrix in `%LOCALAPPDATA%\daisyloop-ml\cache` from the copy. `steptrain.py` finds the windows from the phase channel (or `windows` in its config dict), trains, runs the identification and the gate in one process (single thread, a few minutes, no Ray) and writes `models\steptrain\`:

- `model_deploy.json` and `feature_spec_deploy.json` if the gate passes, `candidate_model_deploy.json` and `candidate_feature_spec_deploy.json` if not. The two files of a run are always written together, and files of earlier runs in the directory are deleted first, so a spec never belongs to another run's model;
- `report.json` with the gate verdict and the reasons, the identification (fitted plant, excitation, fit and hold-out RMSE, fan effect at the held-out states), the comparison with persistence and with the deployed model on the final fold, and `hold_rows` and `overlap_rows` (the latter is 0: the held-out rows are neither in the fit nor in the response term).

Rows with a missing water, rpm or power value are not used in the fit, and a gap of 10 s or more (a daemon restart) ends a segment; the next segment starts at the first complete row after it. A restart shortly before the fan test therefore costs the context before the gap, nothing else.

The gate (also written in the `criteria` field of the report): ship only if (1) the fan was held at three or more duty levels for 2 min each over a span of 40 points, (2) the grey-box fit RMSE is at most `grey.fit_rmse_max`, (3) the fan exponent is positive with a one-sided 95 % likelihood-ratio bound against the model without fan response but with ambient drift, and the conductance at the lowest held running duty level is at least 25 % below full fan, (4) fan phases 6 and 7 (not used in the fit or in the response term) are predicted with RMSE at most `grey.hold_rmse_max` and 1.5 times better than by the no-fan model refit on the same rows, (5) the water 30 s and 120 s RMSE on the final fold is not worse than the deployed model's beyond the bootstrap noise band, and on the held-out step rows the 120 s water, CPU and GPU forecasts are not worse than persistence beyond the same band. The two RMSE limits are 1.25 times the largest RMSE the correct plant produced in the dry run (section 9), so they follow the noise of the data instead of a hand value. The pump is not part of the gate: `identified.pump` is always `false`, the pump stays on the firmware PID and the planner does not use the model for it. A refusal is not a failure of the session: read `reasons`, deploy nothing, and the deployed model keeps running exactly as before.

## 7. Deploy the model (5 min, needs UAC)

```
Copy-Item C:\clones\daisyloop\models\steptrain\model_deploy.json, C:\clones\daisyloop\models\steptrain\feature_spec_deploy.json C:\clones\daisyloop\models\
Start-Process powershell -Verb RunAs -ArgumentList "-Command Copy-Item C:\clones\daisyloop\models\model_deploy.json,C:\clones\daisyloop\models\feature_spec_deploy.json 'C:\Program Files\daisyloop\models' -Force"
New-Item C:\Users\user\AppData\Local\daisyloop\stop -ItemType File
```

The two files exist only when the gate passed; after a refusal the directory holds `candidate_*` files and nothing is copied. After the relaunch the UI shows the model loaded and warm (seeded) and `fan identified` (`pump identified` is always off). Active mode still needs `writes_enabled = true`, which stays false until the owner decides.
Restore: copy the previous `model_deploy.json` and `feature_spec_deploy.json` from git (`git show HEAD:models/model_deploy.json`) the same way.

## 8. Writes back to false

`writes_enabled = false` is already set in step 5 and must be checked again after the deploy: `Select-String writes_enabled 'C:\Program Files\daisyloop\config.toml'` shows `false`; the UI shows `Schreiben ist deaktiviert`. Remove `C:\steptest-copy` when the report is archived.

## If the PC crashes or hangs mid-test

- The D5 has no known timeout in the direct duty mode (`snapshot/d5-control.md`). A dead daemon leaves the last duty on the device: during a fan test that can be 27 % (phase 5) or, in the duty test of step 3, 0 % while the computer is hung. A crash of the daemon alone is relaunched by the watchdog task within about 2 min; on start it releases every channel it owns, but only when `writes_enabled = true`.
- Owner actions, in this order: (0) as soon as the PC answers again, run the abort command of section 0 (it works with the daemon dead, hung or missing); (1) stop the heat: stop the load, or if the screen is frozen switch the PSU off (this also stops the pump, nothing produces heat any more); (2) boot; (3) the abort command prints `OK`; a daemon relaunched with `writes_enabled = true` also releases the fan (any fan in duty mode) and the pump (only when marked as owned in `%LOCALAPPDATA%\daisyloop\d5-owned`), while one with `writes_enabled = false` writes nothing and raises the `Foreign` alarm that names the abort command; (4) only then set `writes_enabled = false` (step 5). If the daemon does not come back, start `C:\Program Files\daisyloop\daisyloop.exe` from the install directory or run the watchdog task once (`Start-ScheduledTask daisyloop-watchdog`).
- A crashed session produces a partial window in the lake. The retraining then finds a window whose fan span or pump span is too small and the gate refuses ("not identified"); repeat the missing test on another evening.

## 9. Dry run on synthetic data (what was verified)

`python python\steptrain.py dryrun [directory] [case,case] [seeds]` injects a step window generated from the plant of `docs/thermal.md` (two-lump heat capacity, fan exponent m, pump coefficient gamma, dead time 31 s at the pump floor and 11 s at 100 %, fan spool 4.5 s, sensor noise 5 mK, load noise 8 W, the safety curve of section 3 emulated) into a copy of the real lake matrix (4 days from 2026-11-01), following the protocol of sections 3 and 4 as one table (`SIM["inject"]["protocols"]["runbook"]`; `test_steptrain.py` compares the table of 4a and the plans of steps 2 and 4 with it). Each run goes through the same `StepTrainer.run` as the real retraining and writes its output directory under the temp directory. Cases, 10 seeds each: fan response at start water 30, 30.5 and 31 C, each without and with a restart gap (all channels missing for 120 s, ending 30 s before the fan test), no fan response, linear drift of the sink temperature of 5.5 K over the run, bump-shaped drift of 5.5 K (sine), each at 30.5 C without and with the gap, and one structurally different plant (`alt_fan`: m 1.2, A 30 W/K, C1 8.5 kJ/K, K 15 W/K, gamma 0.1, sink 22 C, spool 8 s) with and without the gap. Result of the last full run (`ship` = the gate shipped, `ok` = shipped with the fan effect within a factor of 1.5 of the plant's):

| Case | Seeds | Ship | Ship and ok | Fit RMSE K | Hold-out RMSE K | Effect / plant |
|---|---|---|---|---|---|---|
| fan, start 30 / 30.5 / 31 C, no gap | 30 | 30 | 30 | 0.038 to 0.051 | 0.043 to 0.068 | 0.97 to 1.02 |
| same with restart gap | 30 | 30 | 30 | 0.039 to 0.051 | 0.041 to 0.088 | 0.98 to 1.03 |
| no fan response, with and without gap | 20 | 0 | 0 | 0.055 to 0.074 | 0.088 to 0.129 | |
| linear drift 5.5 K, no fan response | 20 | 0 | 0 | 0.047 to 0.065 | 0.051 to 0.109 | |
| bump drift 5.5 K, no fan response | 20 | 0 | 0 | 0.122 to 0.147 | 0.140 to 0.206 | |
| alt_fan | 20 | 0 | 0 | 0.045 to 0.055 | 0.061 to 0.164 | 0.86 to 0.96 (refused) |
| alt_fan without fan response (alt_nofan) | 20 | 0 | 0 | 0.042 to 0.055 | 0.028 to 0.044 | |

The alt_fan plant is refused in 20 of 20 runs (the hold-out RMSE of the grey-box fit is above its limit, or the final-fold 120 s RMSE is worse than the deployed model's); the fan effect it would have shipped is within a factor of 1.5 of the plant's in all 20, so nothing wrong was shipped. Seeds 10 to 14, not used for the limits, gave 30 of 30 shipped and ok for the six fan variants (hold-out RMSE 0.038 to 0.064 K), and starts of 28.5, 29 and 29.5 C (5 seeds each, no gap) shipped 15 of 15 with the 250 W lead (hold-out RMSE up to 0.100 K against 0.110), so the 30 to 31 C band is the owner's decision and not something the dry run needs. Reproducibility: with one BLAS thread and six the model files are byte-identical. The two RMSE limits of the gate come from the fan rows of this table (section 6).

What this does not show: nothing here ran on the real machine, so structure error of the real plant against the grey-box model is untested; the Python plant holds the firmware fan at 100 % below its set-point and has no bound latch, while the real firmware regulates to its set-point and the daemon has the latch; a start below 30.0 C was not part of the criteria; `d5_abort`, the UI and the daemon were not involved.

