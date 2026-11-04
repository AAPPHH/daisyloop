# daisyloop UI protocol (v1)

Transport: Windows named pipe `\\.\pipe\daisyloop`, byte mode, newline-delimited JSON, one request then one response, strictly sequential per connection. The UI process is the client, the daemon is the server. Types live in `src/protocol.rs`; the pipe client is `src/ipc.rs`.

## Envelope

Request `{"v":1,"id":7,"body":{...}}`, response `{"v":1,"id":7,"body":{...}}` with the same `id`. A server that does not support `v` answers `{"type":"error"}`.

Request bodies (`type`): `snapshot`, `history` (`channels`, `from_ms`, `to_ms`, `max_points`), `command`.
Response bodies (`type`): `snapshot`, `history`, `ack`, `error {message}`.

## Snapshot

`ts_ms`, `seq`, `channels[]` (`id` globally unique, `label`, `hardware`, `kind`, `unit`, `value`, `min`, `max`), `controls[]`, `limits[]`, `alarms[]`.
`control`: `id`, `label`, `kind` (pump|fan), `rpm_channel`, `duty_channel`, `mode` (`pid`|`manual`|`curve`, only `pid` has UI so far), `floor` (hard minimum duty, enforced by the daemon, mirrored by the UI), `pid` (`source` channel id, `setpoint`, `kp`, `ki`, `kd`, `min_duty`, `max_duty`), `state` (`output`, `error`, `p`, `i`, `d`, `saturated`).
The UI polls `snapshot` at 1 Hz. History is requested only on range or selection change and every 10 s / 60 s / 5 min for 1 h / 1 d / 1 w.

## Commands

`set_mode {id, mode}`, `set_pid {id, pid}`, `set_limit {channel, low, high}` (both null removes the limit). The daemon must clamp `min_duty >= floor` and `max_duty >= min_duty` itself; the UI clamps only for feedback. The server answers `ack` or `error`.

## Pipe ACL for a user process talking to a SYSTEM daemon

A pipe created by a SYSTEM service gets the default DACL: full control for SYSTEM and Administrators, read for Everyone. A normal user cannot open it read/write, so the daemon must pass explicit `SECURITY_ATTRIBUTES` to `CreateNamedPipeW`, for example SDDL `D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)` (interactive users read/write). Recommended hardening:

- Restrict to the installing user's SID instead of `IU` (`(A;;GRGW;;;<user-sid>)`), because every control command is a write to the pump and fans.
- Create the first instance with `FILE_FLAG_FIRST_PIPE_INSTANCE` and `PIPE_REJECT_REMOTE_CLIENTS` so no other process can squat the name and no remote client can connect.
- Verify the client with `GetNamedPipeClientProcessId` / impersonation if per-user policy is needed.
- The daemon enforces all safety limits (pump floor, duty range); never rely on the UI.
- Do not use a mandatory label lower than the client integrity level; normal UI runs at medium integrity, which the SDDL above allows.
