# Unclean restarts

Source: Windows System log, Kernel-Power 41. Boot time is when the event is logged, not when the machine died.

## Baseline before the C-state change

16 events from 2026-10-23 09:59 to 2026-11-04 16:32, about 1.3 per day, 4 of them on 2026-11-04. 11 predate the first LibreHardwareMonitor install (2026-11-01 10:22). Bugcheck 0 throughout, except 0x1E on 2026-10-31 19:17 with minidump 103126-31546-01.dmp. Power button timestamp set only on 2026-11-04 06:21.

Last collector data before each outage (up to 60 s missing, old build):

| Last data | CPU package W | Core max C | CPU load % | GPU W | Water C |
|---|---|---|---|---|---|
| 11-04 09:24:11 | ~180 | ~97 (104 peak in 4 min) | ~43 | ~111 | 35 |
| 11-04 16:11:12 | ~195 | ~98 (104 peak in 4 min) | ~52 | ~115 | 37 |
| 11-04 16:31:12 | 50..130, bursting | 66..76 | 16..44 | ~15 | 31 |

Time of the unexpected shutdown per event 6008: 11-01 06:03:25, 11-02 22:11:55, 11-04 03:42:45, 09:23:51, 16:11:15, 16:31:41 (the last possibly a reset by the user). They match the table within the collector's commit interval.

Corrections: the collector's data ended 11-03 13:12:38 with an idle PC, but the machine ran on until 03:42:45, so that is a silent collector stop by the old build for an unknown reason, 14.5 h before the crash, and there is no data for the 03:42 crash. 11-02 22:10:58 was a requested shutdown (event 1074) which then failed uncleanly at 22:11:55.

No common load pattern across the outages with data, which are too few (3) to conclude much.

## Test: package C-states limited to C6

Change by the user in BIOS, booted 2026-11-04 16:37. Hypothesis: deep C-states cause the hard resets.

Fixed before the result: supported if there is no event 41 during 72 h of normal use including load (about 2 % chance at the baseline rate). Falsified as the sole cause by any event 41 in that window. Each new event gets a row here with the collector's last data.

## Other suspects, unchanged so far

- RAM: CUDIMM per user, 8200 MT/s, 1.40 V, 2 x 24 GB (Win32_PhysicalMemory). Normal for CUDIMM XMP, but above the CPU's official DDR5-6400 as far as known, unverified here. Next test if C6 fails: XMP off or 6400.
- BIOS 2.A70 dated 2025-07-14.
- CPU 270 W peaks at 104 C.
- Power supply.
