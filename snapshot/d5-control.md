# D5 NEXT control report 0x03: Stage 0 findings (read-only)

Date 2026-11-04, device 0C70:F00E serial 00000-00000 firmware 1025, HID usage page 0xFF00. Only GET_FEATURE_REPORT 0x03 (and passive input report 0x01 reads, as the collector does) were issued. No SET, no write. Probe: `tools/d5_probe` (own crate, hidapi). Backup: `snapshot/raw/d5_control_backup.bin` (809 bytes, ID 0x03 + 806 payload + CRC) and `.txt` (hex dump of 3 samples + 3 telemetry reports).

## Priority answers (PID)

1. Mode byte. Both subgroups are in mode 1 (set-point / PID controller): fan at 0x41 mode=1, pump at 0x96 mode=1. Mode values per liquidctl doc: 0 manual preset, 1 PID, 2 curve. The aquasuite manual names the same controllers (power preset, temperature set point, curve controller, plus a flow controller for the pump only).
   - Fan evidence that mode 1 is really regulating: history of 144k 1 Hz samples (2026-11-01..04) shows fan duty modulating (5 to 100 %, mean 68 % in the 29 C bin, 92 % in 30 C bin) and 99.9 % above 31 C; decoded fan set-point is 30.00 C. Consistent with a PID on the coolant temperature, set-point 30 C, saturated at 100 % because water is 31..41 C.
   - Pump: decoded set-point 45.00 C. Pump telemetry was 100 % (4786 rpm) for days, and switched to 19.8 % (2570 rpm) at about 07:36 local on 2026-11-04, i.e. before the probe run at 07:41:58, and stays there (580 samples, constant). Water 34 C is below the 45 C set-point, so the PID sits on its floor. The coordinator premise "pump stays at 100 %" is therefore true historically but not now. Inference (not proven): until 07:36 the pump was in power-preset mode 100 % (the preset field in the report still holds 10000 = 100 %) and was switched to set-point mode by someone (user in aquasuite or on the device menu). The saved aquasuite profile 1..4 (Settings_DeviceProfiles.xml, 2025-11-26) has pump set-point 40.00 C, live has 45.00 C, so the live pump config was edited after that profile was saved. Treat the pump as actively being configured by the user right now.
   - The probe run itself did not cause the change (change precedes it; reads are not state-changing as far as seen; rpm and report stayed identical across the 3 reads).
2. Block layout: see next section.
3. PID location: on the device (inferred from evidence, not tested by stopping the service, which is forbidden). Evidence: Aqua Computer forum answer "USB only required for RGBpx and firmware updates ... all settings (including fan curve) are saved on the pump"; manual 6.2 "an USB connection is not required for operation" and 6.1 "aquasuite will overwrite settings made on the device itself" (aquasuite is only a writer of the stored config); manual 10.6 "settings in the D5 NEXT will be used" when aquabus detaches; kernel docs "controlled using temperature curves directly from the pump"; product page "Eight further values can be transferred from the PC to the D5 NEXT" (only the software-sensor inputs need the service). Aquasuite service log (`C:\ProgramData\aquasuite-data\Log_Service.txt`) only contains Start / "Device add" lines, no control traffic. Expect the regulation to continue with aquasuite stopped; only software-sensor inputs would go stale.
4. Inputs: manual 10.2/11.2: "one of the up to nine available temperature sensors" = internal coolant sensor + 8 software sensors. `Settings_SoftwareSensors.xml` shows all 8 software sensors of this D5 unassigned (empty source), and telemetry shows them as 0x7FFF (n/a). So both PIDs currently use the internal coolant sensor (only valid source). Host can feed a CPU temperature only through the software-sensor channel (aquasuite service transmits "Virtual Software Sensors" over USB; manual 13.5, 18.2). The transport report (ID, layout, refresh timeout, what the firmware does when the host stops sending) is not documented in liquidctl, the kernel driver (read-only virtual sensors, only temp offsets +-15 K are writable) or the refs; it would need a USB capture of the aquasuite service. Unknown, not attempted.

## CRC

- Self-check: hand-written CRC-16/USB (poly 0x8005 reflected = 0xA001, init 0xFFFF, xorout 0xFFFF) on "123456789" = 0xB4C8 (pass).
- Device-produced report, all 3 samples: stored BE16 at 0x327 = 0xCEC2. CRC over bytes [0x01 .. 0x327) (ID excluded, length-3 bytes) = 0xCEC2 MATCH. Variant including the ID byte [0x00 .. 0x327) = 0xF065 no match. The liquidctl code and kernel driver (`checksum_start=1, length=size-3, offset=size-2`) are right; the liquidctl doc sentence "starting at the 0x03 report ID" is wrong. The aquasuite stored profiles are 808 bytes (ID stripped) and carry a different CRC (0xAC73) because their content differs; its range was not verified.

## Volatility

3 GETs 4 s apart: byte-identical (no volatile offsets in 0x03). The report holds configuration only; live state (rpm, temp, duty actual) is in input report 0x01. Caveat: a changing config (user in aquasuite) is not volatile in the sense of the device, but invalidates a stale backup. The backup was taken 07:41:58, after the pump change; it does not contain the pre-07:36 pump state (mode possibly 0). It is a faithful rollback of the state at 07:41:58, not of the state "as it was for days".
Telemetry (input report 0x01) via hidapi is 162 bytes, not 158; fields used by the collector are at the documented offsets (liquid 0x57, pump 0x6C, fan 0x5F).

## Subgroup layout (offsets relative to subgroup base, report incl. ID)

Fan base 0x41, pump base 0x96, each 0x55 bytes (fan 0x41..0x95, pump 0x96..0xEA). Structure is shared with the Octo fan structs in the liquidctl example. Decoded, "verified" = confirmed by kernel/liquidctl/doc, "inferred" = from observed values and manual.

| rel | size | field | fan live | pump live | status |
|---|---|---|---|---|---|
| +0x00 | u8 | mode 0/1/2 | 1 | 1 | verified |
| +0x01 | u16 BE | preset power, centi-percent (kernel `d5next_ctrl_fan_offsets` 0x42/0x97) | 10000 (100 %) | 10000 (100 %) | verified; inactive in mode 1 |
| +0x03 | u16 | 0 in both; 0xFFFF in Octo examples | 0 | 0 | unknown, candidate: sensor source index (0 = coolant) |
| +0x05 | u16 | PID set-point, C x 100 | 3000 | 4500 | inferred (fan matches observed regulation at ~30 C) |
| +0x07 | u16 | P gain | 2500 | 1400 | inferred, scale unknown (x100?) |
| +0x09 | u16 | I gain | 2000 | 1200 | inferred |
| +0x0B | u16 | D gain | 500 | 0 | inferred |
| +0x0D | u16 | unknown | 10 | 40 | unknown |
| +0x0F | u16 | unknown | 10 | 20 | unknown; pump 20 ~ observed pump floor 19.8 % |
| +0x11 | u16 | unknown, 1 in both | 1 | 1 | candidate: hold-minimum flag |
| +0x13 | u16 | C x 100 | 3150 | 3195 | unknown, candidate: curve start-up temperature |
| +0x15 | 16 x u16 | curve temperatures, C x 100 | 30.0..45.0 step 1.0 | 30.0..49.5 step ~1.3 | verified (liquidctl doc: 16 points) |
| +0x35 | 16 x u16 | curve output, centi-percent | 5.0 % .. 100 % | 15.0 % .. 100 % | verified |

Not located with certainty: min power, max power, hold-minimum flag, hysteresis, D-time, fallback behaviour, sensor source index. The kernel D5 driver does not implement them (only +0x01 duty and temp offsets at 0x2D); the kernel docs say pump configuration "seems to require sending a complete configuration". The candidates above come from pattern comparison with the Octo example (`00 28 00 14 00 01` tail) and the manual's parameter list; resolving them needs a diff between two reports of known settings (aquasuite saved profile 1 vs live is one such pair: only fan preset 0x42, pump set-point 0x9B, pump curve differ; ask the user to change exactly one setting in aquasuite, then GET again, which is read-only for us).
Observed in saved profile for comparison: fan preset 4514, pump set-point 4000, pump curve 38.75..50.0 C with output 0..100 %.
Software-sensor temp offsets: 0x2D.. (kernel), not decoded here.

## Pump hardware minimum

- Aqua Computer D5 NEXT manual (D5_NEXT_english.pdf, aquacomputer.de handbuecher, ch. 10.3): "output values in percent correlate to the pump speed range from ca. 2000 to 4800 rpm ... 50 % = ca. 3400 rpm, 1 % = ca. 28 rpm. If the selected speed cannot be attained, the pump controller will use the highest stable rotation speed". Ch. 10.5: output range limited by "Minimum power"/"Maximum power". Measured here: 100 % = 4786..4821 rpm, 19.8 % = 2570 rpm (2000 + 0.198 x 2800 = 2554, consistent).
- Aqua Computer forum "D5 Next minimum speed ?" (forum.aquacomputer.de, 2022): "Minimum speed is 1800", "All Aquacomputer D5 pumps have a minimum rpm of around 1800-1900; official statement: makes no sense to run below 1800 rpm". Forum "Firmware Update to 1400 RPM for D5 NEXT" (2019): staff: "the lowest possible value is ~2000 rpm" and firmware cannot change it.
- aquaero 6 manual (easymanua.ls, p.31): "D5 pumps connected via aquabus can generally not be stopped, a power setting of 0 % corresponds to the minimum pump speed"; hold-min-power unchecked means "lowest possible power setting". A D5 on a fan output (not our case) can be switched off.
- Conclusion: 0 % does not stop a D5 NEXT in its own controller; it runs at ~1800..2000 rpm. Practical safe floor: use the device's own floor (about 20 %, ~2550 rpm as now) as the minimum our software ever requests; never rely on 0 %. Flow at the floor in this loop is unmeasured. Not found: an official statement of the lowest rpm that still gives adequate flow for a given loop. Coolant range 0..50 C, "the pump must not run dry" (manual 4.7).

## Fan vs pump coupling, SET semantics

- Every SET is the whole 809-byte report (kernel: `aqc_set_ctrl_vals` = GET, patch, CRC, SET report 3, then the secondary report 0x02 `02 00 00 00 02 00 00 00 00 34 C6` after 200 ms; liquidctl: GET, patch, CRC, 0.2 s delay, SET report 3, no 0x02; docs say the device "seems to work fine without it, but official software always sends it"; the 0x02 may be the commit/save-to-flash step, unverified). So a fan SET rewrites the pump subgroup bytes too.
- Is the pump transiently affected? No evidence either way exists in kernel, liquidctl, aquasuite log or files. liquidctl does exactly this for `set_fixed_speed("fan")` and ("pump") in separate calls and the kernel does it for pwm1/pwm2; no pump glitch reports were found, but nothing documents pump rpm during a fan write. The firmware likely re-applies the whole config (possibly resetting PID integrators and restarting the pump controller), which can move the pump output. If a fan SET cannot be shown to leave the pump untouched (it cannot yet), then a fan SET is a pump-affecting action and falls under "pump only with user present" until Stage 1 measures it with the user present.

## Recommendation: who runs the control loop

Firmware PID configured by us as the base, optional software adjustment on top, no pure software duty loop for the pump.
- Firmware PID: runs on the device on the internal coolant sensor, survives daemon crash, PC hang, logoff, aquasuite stop; stored on the pump, even works without USB. Failure mode is benign: it keeps its last config. Downside: only the coolant temperature (and software sensors via the unknown transport) as input; slow loop; no CPU feed-forward.
- Software PID writing duty (mode 0): on crash or hang the preset stays at whatever was last written; no known host watchdog or fallback in the device (unknown, untested). A stuck low value on the pump is the dangerous case. Each update is a full-report SET with the unresolved side effects above, rate-limited ~0.2 s and the kernel notes devices "can not accept reports in quick succession".
- Both: firmware PID/curve stays active as fail-safe floor (pump min power set to the device floor, fan PID or curve with a sane minimum), the daemon only moves slow parameters (e.g. fan set-point or fan preset in a mode the firmware falls back from) at low rate, never the pump below its floor and never switching the pump out of a controller mode. Defer any CPU-temperature feed until the software-sensor transport is captured.
- Sim training does not need control now; collector remains read-only.

## Staged write test plan (NOT executed; each stage needs the orchestrator's go, nothing here has been run)

Common rules for every SET stage:
- Pre: fresh GET immediately before; compare with `d5_control_backup.bin`; if they differ (user or aquasuite edited), stop and re-baseline, do not blindly write the old backup. Verify CRC [1..0x327) on the fresh GET before patching.
- Build the new report from the fresh GET, patch only the intended bytes, recompute the CRC with the hand CRC, assert in code a byte diff against the fresh GET: only the intended offsets plus the two CRC bytes (0x327, 0x328) changed. Abort if any other offset differs, especially 0x96..0xEA (pump subgroup) in fan stages.
- Send once (no retry loop), 200 ms after the last report, without the 0x02 secondary report in Stage 1 (observe whether the change persists after readback; decide on 0x02 afterwards).
- Post: GET after 0.5 s, 2 s and 10 s; byte diff vs the intended report; CRC check; then rollback or hold as described.
- Monitoring in parallel from the passive 1 Hz input report: baseline over the 30 s before: pump rpm, pump duty, fan rpm, fan duty, water temp. Abort criteria: pump rpm deviates more than 100 rpm from its baseline in any sample, pump duty changes, water above 40 C, GET fails or CRC invalid, device disappears. On abort: immediate SET of the fresh pre-test report (same procedure), then GET compare.
- Rollback: SET the report captured by the fresh GET directly before this test, saved to a timestamped file (CRC already valid; no patching), and verify GET equals it byte for byte. `snapshot/raw/d5_control_backup.bin` is the state of 2026-11-04 07:41 and would overwrite later aquasuite edits; use it only as a reference. Second reference: aquasuite profiles in `Settings_DeviceProfiles.xml` (differs: pump set-point 40, fan preset 4514). Last resort: user restores via aquasuite or device menu; factory defaults (manual 4.8) lose the whole config including RGB and are not an acceptable plan.

Fan stages (user at the computer with confirmation directly before each run, like every SET; never autonomous, even after Stage 1):
- F1: identity write needs the user present as it also rewrites the pump subgroup (see Stage 1 below).
- F2: change fan preset power at 0x42..0x43 from 10000 to 9000 (field inactive in mode 1). Verify: byte diff = 2 bytes + CRC; telemetry fan duty unchanged (still PID driven), pump rpm unchanged; GET readback equal. Roll back to 10000.
- F3: change fan set-point at 0x46..0x47 from 3000 to 3600 (water ~34 C, below set-point, so a working PID must visibly lower the fan duty). Verify: fan duty drops within seconds while pump rpm and water stay; abort if water above 40 C. Roll back and verify duty returns to ~100 %.
- F4 (optional): P/I/D at 0x48/0x4A/0x4C one at a time, small steps, to establish scaling from the response.
Pump stages (user present, explicit acknowledgement, pump currently at its 19.8 % floor):
- Stage 1, identity write (user present): GET, SET the same bytes (valid CRC), no 0x02. Verify: GET unchanged, pump rpm within +-100 rpm of 2570 for 60 s, fan duty unchanged. This is the only way to learn whether a whole-report SET disturbs the pump; the result shows whether a whole-report SET disturbs the pump. It does not allow fan stages without the user.
- P2: pump preset 0x97..0x98 (inactive in mode 1) 10000 to 9000. Same verification, roll back.
- P3: pump set-point 0x9B..0x9C from 4500 to 4400 (still above water, expected: no change in rpm). Roll back.
- P4: only after P2/P3: set-point close to water temperature to see the PID leave the floor; abort on rpm below 1800 or water above 40 C. Never write mode, min power or curve for the pump without extra approval.

## Still unknown

- Which bytes are min power, max power, hold-minimum, hysteresis, sensor index, fallback; scaling of P/I/D; meaning of +0x0D/+0x0F/+0x11/+0x13; value of the pump flow-controller mode byte.
- Whether a full-report SET disturbs the pump; whether 0x02 secondary report is a flash-commit; whether an unsaved SET reverts on power cycle (potential safety net).
- Software-sensor transport and its failure behaviour (host stops sending).
- What changed the pump at ~07:36 on 2026-11-04 and the pump mode before that.
- Pump flow at the 2000 rpm floor in this loop.
