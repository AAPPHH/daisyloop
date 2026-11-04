# Corsair iCUE LINK fans: how to talk to them

Research only, nothing was sent to any device. No LINK hub is attached yet, so nothing here has been verified on hardware.

## Answer

- No official interface for fan speed. The CUE SDK covers lighting ("control device LEDs"), and the fan and pump request in cue-sdk issue #13 is open since 2020. https://github.com/CorsairOfficial/cue-sdk
- LINK fans need the hub's own USB HID protocol, reverse engineered. The D5 NEXT and its headers can't drive them; no PWM path was found (inference, Corsair doesn't state it outright).
- iCUE and the Corsair service must be stopped while we control the hub. Last writer wins.
- Reference for Windows: FanControl.CorsairLink, supports hub `0C3F` with RPM read and power set, needs iCUE stopped, firmware 2.5+ for the higher device count, uses the global mutex `Global\CorsairLinkReadWriteGuardMutex`. Local clone `refs/FanControl.CorsairLink`.
- Protocol reference: `refs/OpenLinkHub/src/devices/lsh/lsh.go`, Linux only. liquidctl has no support.

## Protocol (lsh.go line numbers)

- Hub `1B1C:0C3F`, HID interface 0, keyed by serial. On Windows check for InterfaceNbr -1 and the report length, use read with timeout.
- Write 513 bytes `00 v2 01 CMD ARG`, v2 is 00 for hub commands (6039-6087). Read 512 bytes after every write. Response: status at [3] (0 = OK), type echo [4..5], count [6], payload from [7].
- Read: close `05 01 01 EP`, open `0D 01 EP`, `08 01 EP`, second read if the type repeats, close (5355-5389).
- Write: close, open, `06 01 LL LL 00 00 TYPE PAYLOAD` with LL = payload length + 2, little endian, close (5769-5808).
- Init order: firmware `02 13`; software mode `01 03 00 02`, wait 500 ms; device table; LED types; color endpoint; set speeds (370-460). Hardware mode `01 03 00 01`.
- Devices: EP `36` type `21 00`; count [6], records at [7]: type +2, model +3, id length +7, id +8; channels are 1-based (3950-4254).
- RPM: EP `17` type `25 00`, triplets `status rpm_lo rpm_hi` from [7] (3689-3725). The [7] handling in OpenLinkHub needs a check against the Windows reference.
- Temperature: EP `21` type `10 00`, triplets, int16 LE / 10 (3731-3757).
- Set duty: EP `18` type `07 00`, payload `n` then `channel 00 duty 00` for each channel; retry on status != 0, OpenLinkHub up to 20 x 100 ms (3595-3632).
- Avoid: LCD, color, PSU commands.

## Open questions for the fail-safe

- No keepalive in OpenLinkHub, and no evidence of what the hub does when the host goes silent. Inference: it holds the last duty in software mode, so a PC crash could leave fans stuck low until a restart. This must be tested with the hub, user at the computer, with the fans pulled low.
- Hardware mode on exit is in the code, but a crash can't send it. What hardware mode runs without iCUE having stored a preset ("Device Memory Mode") is unknown. Prepare a safe preset in iCUE once before removing it, if the hub keeps it.
- Whether OS hang leaves USB power and hub state intact, and what state the hub boots into, unknown.
- Fan floors in OpenLinkHub: minimum 20 % in manual and curve paths. Duty 0 is only by a PSU profile, unverified.
- Device limit per hub: sources disagree (14 vs 24).

## Design consequences

- Software PID in the daemon: water temp from D5, duty to the hub. Pump stays on the D5 firmware PID.
- Dead-man: daemon crash and PC hang must fail to a safe airflow; depends on the tests above. Until known, only a duty profile acceptable if frozen under load may be used: conservative minimum duty.
- Writes only on change, read verify after write, per channel floor, RPM plausibility check per fan.
- New fans change the thermal behaviour, so the fan response must be measured after the swap.
