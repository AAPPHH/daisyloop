# Fan header mapping (read-only findings, 2026-11-01)

## Sources used
- LHM 0.9.6 `/data.json` (non-elevated): `snapshot/raw/lhm_data_nonelevated.json`
- aquasuite: `C:\ProgramData\aquasuite-data` (xml settings, `aquasuite_db.database` copy opened read-only, `Log_Service.txt`); `%APPDATA%\aquasuite` does not exist
- D5 NEXT sensors (LHM `/usbhid/0C70/F00E/00000-00000/...`)

## Facts
- Board: MSI MPG Z890 EDGE TI WIFI (MS-7E19), Super-IO Nuvoton NCT6687D-R, readable non-elevated (PawnIO service running).
- LHM board fan sensors (`/lpc/nct6687dr/0/fan/N`, control `/lpc/nct6687dr/0/control/N`), snapshot while CPU at ~249 W / ~100 C:
  - fan/0 CPU Fan: 0 rpm, control 100 %
  - fan/1 Pump Fan #1: 0 rpm, control/1 "Pump Fan" 100 %
  - fan/2 Chipset Fan: 0 rpm, control 0 %
  - fan/3 EZ-Connect Fan: 0 rpm, control 100 %
  - fan/10 System Fan #1: 0 rpm, 100 %
  - fan/11 System Fan #2: 699 rpm, control 26 %  (only board header with a tach signal)
  - fan/12..15 System Fan #3..#6: 0 rpm, 100 %
  - No LHM fan sensor 0 rpm means: nothing connected, or connected fan has no tach (DC/PWM only), or header stalled. Controls at 100 % on empty headers are just BIOS defaults, not evidence of a load.
- D5 NEXT (serial 00000-00000, only aquasuite device besides service): Fan channel 1463 rpm, control 99.9 %, Fan Power 9.3 W; Pump 4818 rpm, 100 %, 18.2 W; water temp 35.9 C; "Viritual Flow" 0 L/h (no flow sensor attached/configured). The D5 NEXT fan header is run at ~100 % with several fans: 9.3 W at 12 V is about 0.78 A, i.e. multiple fans via splitter/hub (a single fan is typically 1-3 W). The reported rpm is one tach (the first fan/hub tach line), not the sum.
- GPU: RTX 4080 fans 0 rpm at 30 % (zero-RPM mode, GPU 42 C) - not a case fan.
- aquasuite profiles: `Settings_DeviceProfiles.xml` holds 4 binary (base64, undocumented layout) D5 NEXT profile blobs, all identical except a trailing CRC; no human-readable fan names or curve sources. `Settings_SoftwareSensors.xml`: 8 empty software sensors (no curve source assigned). `Settings_Service.xml`: EnableOutputs/Profiles/Inputs/VirtualSoftwareSensors all false. aquasuite pages: `D5 NEXT` user page only; no named fans.
- Consequence: the only curve source for the D5 fan channel stored by aquasuite is inside the device itself (profile blob / device firmware); not decoded here.

## Working hypothesis (not verified)
- D5 NEXT fan header: radiator fans (several, hub/splitter), driven by D5 NEXT fan curve/fixed duty (currently ~100 %).
- Board System Fan #2 (699 rpm @ 26 %): one case fan (likely rear exhaust or similar), BIOS curve driven.
- Board CPU Fan/Pump Fan #1/System Fan #1,#3-6/EZ-Connect: no tach seen; either empty or unmonitored fans.

## Unknown
- Physical wiring of System Fan #2 and which fans are on the D5 splitter: needs visual inspection or toggling a duty (forbidden here).
- Whether LHM controls actually drive the NCT6687D-R headers (MSI BIOS may own them, MSI BIOS info not accessible). Control sensors reported are read-only here.
- Whether empty-looking headers use DC/PWM fans without tach; elevated LHM does not change tach readings visibly (non-elevated already reads the Super-IO), so "needs elevated LHM" is likely NOT required for these sensors; elevation may matter only for some CPU MSR items, to be confirmed by comparing an elevated run (not done).
- Fan curve/mode of D5 NEXT fan channel (profile blob undecoded).

## aquasuite data log / export
- DB (`aquasuite_db.database`, SQLite, last modified 2026-07-12): tables `log_settings`, `log_sources`, `log_profiles`, `log_data` all exist and have 0 rows. `log_settings` has columns logType, logState, logInterval, expiration*, autoexport, autoexportFile, so an auto-export (file) can be configured in the GUI, but none is configured. Nothing is being logged; no export file exists. Not changed.
- Service log: `C:\ProgramData\aquasuite-data\Log_Service.txt` (JSON lines: service start, "Device add d5next:00000-00000"), no sensor data.
- Service endpoint: `service_settings_server.xml` ports 14360-14365 on localhost (aquasuite service IPC, undocumented; MQTTnet DLLs are shipped, no MQTT config seen).
