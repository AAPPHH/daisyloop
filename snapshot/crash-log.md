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

Event 6008's time runs early: other_project_rl metric files were written later, at 03:43:40, 09:24:12 and 16:11:44 (reported by the other project session). So the true resets are at or after those times.

Load at the resets (other project session): 03:43, 09:24 and 16:11 under full RL training (long-run profile: CPU 60 %, GPU 71 %, 106 W, 256 envs; sampled 2026-11-04 16:47..16:53), with low-priority other_tests tests possibly on top; 09:24 also had reading agents. 16:31 was 2 min after a job start, GPU 11..28 W, cause open (possibly a reset by the user). 2026-11-04 14:57 was no reset, but a process death with 0xC0000005 in a job's start phase that didn't recur on retry.

So 3 of 4 outages fall in partial-load training. How much of the uptime that makes up is not computed, so this is no correlation yet. Too few outages with data to conclude much.

## Test: package C-states limited to C6

Change by the user in BIOS, booted 2026-11-04 16:37. Hypothesis: deep C-states cause the hard resets.

Fixed before the result: supported if there is no event 41 during 72 h of normal use including load (about 2 % chance at the baseline rate). Falsified as the sole cause by any event 41 in that window. Each new event gets a row here with the collector's last data.

Result: failed as the sole cause. Event 41 logged at the 2026-11-04 18:18 boot, event 6008 time 17:32:52, 55 min after the C6 boot. No power button timestamp, bugcheck 0. Last data 17:32:47: CPU ~58 W (144 W peak in 4 min), core max ~67 C (82 C peak), load ~22 %, GPU ~116 W and 75 % (training), water ~36 C, pump 100 %. Not thermal: this is the coolest, lowest-CPU outage with GPU training on. Whether the user's BIOS change took effect is taken from the user's report; not verified from software.

## 2026-11-04 19:13:48, first outage captured by the new daemon

C6 still set, RAM still 8200 MT/s per Win32_PhysicalMemory. Bugcheck 0, no power button. WAL replay after the reset committed 320,228 rows; data ends 19:13:48, exactly the 6008 time, with a 1 s fsync. Boot-start proved: the service came up on its own after this and after the user's restart at 19:22.

Last 60 s: CPU 196..253 W (273 W peak in 4 min), core max 93..97 C (102 C peak), CPU load 58..100 % falling, GPU 11 W idle, water 33 C, pump 19.8 %. Compare 17:32: CPU ~58 W and 67 C, GPU 116 W with training. So no shared load: CPU heavy with GPU idle, and the reverse. The CPU load may be partly this project's cargo builds.

Uptime at outage from 6008: 182.4, 100.4, 5.4, 55.4, 55.4 min; seconds are all :23..:24, so the stamp is probably rastered about once a minute and not exact (metric files were written up to 55 s later on earlier outages). Two 55.4 min values are probably coincidence: earlier uptimes range from 17 min to 13,592 min.

Trend from 6008 over 60 days: 29 outages since 2026-09-08. About 11 in the 40 days to 10-18, 12 in 10-23..11-02, and 5 on 11-04 alone. The rate is rising, which fits something degrading or a new load; cause open.

## Other suspects, unchanged so far

- RAM: CUDIMM per user, 8200 MT/s, 1.40 V, 2 x 24 GB (Win32_PhysicalMemory). Normal for CUDIMM XMP, but above the CPU's official DDR5-6400 as far as known, unverified here. Next test if C6 fails: XMP off or 6400.
- BIOS 2.A70 dated 2025-07-14.
- CPU 270 W peaks at 104 C.
- Power supply.

## RAM test: 6400 MT/s

- 2026-11-06 10:48:46: outage under load, event 41 and 6008. Last sample 10:48:38: CPU 174 W, core max 82 C, load 75 %, water 33.6 C, GPU 11.5 W, fan 72 %, pump 2568 rpm. So not thermal.
- 2026-11-06 10:58: owner boots at 6400 MT/s, 1.10 V (Win32_PhysicalMemory, both modules). Before: 8200 MT/s, 1.40 V. No outage counted from here on decides the test.
