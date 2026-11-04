# daisyloop UI protocol (v2)

Transport: Windows named pipe `\\.\pipe\daisyloop`, byte mode, newline-delimited JSON, one request then one response, strictly sequential per connection. The UI process is the client, the daemon is the server. Types live in `src/protocol.rs`; the UI crate includes that same file by path (`ui/src/lib.rs`), so there is one copy. The pipe client is `ui/src/ipc.rs`.

## Envelope

Request `{"v":2,"id":7,"body":{...}}`, response `{"v":2,"id":7,"body":{...}}` with the same `id`. A server that does not support `v` answers `{"type":"error"}`.

Request bodies (`type`): `snapshot`, `history` (`channels` capped at 64 and deduplicated, `from_ms`, `to_ms`, `max_points`), `command`.
Response bodies (`type`): `snapshot`, `history`, `ack`, `error {message}`. A refused command is an `error` with a readable German message; the UI shows it as a toast and next to the channel.

## Snapshot

`ts_ms`, `seq`, `weight` (0 leise, 1 kuehl), `model` (`loaded`, `warm`, `warmup_left_s`, `fan_identified`, `pump_identified`, `error`), `predicted` (`horizon_s`, `water`, `cpu`, `gpu`, limits, `feasible`, `age_ms`; null before the first plan), `step_test` (`id`, `phase`, `phases`, `duty`, `left_s`; null when idle), `channels[]`, `controls[]`, `limits[]`, `alarms[]`.

`channels[]`: `id` (globally unique), `label`, `hardware` (passed through per sensor), `kind`, `unit` (normalised, for example `°C`, `rpm`), `value`, `min`, `max`, `age_ms`, `stale`. A stale channel keeps its last `value`; the UI greys it and shows the age.

`controls[]` (one per channel, `d5.fan` and `d5.pump`): `mode` (`off`|`shadow`|`active`), `allowed` (`writes_enabled` is on), `identified` (the model's response for this channel is identified), `drive` (`firmware` | `duty` | `test`), `target_rpm`, `actual_rpm`, `duty`, `commanded_duty`, `floor_duty`, `min_rpm`, `max_rpm`, `fallback` (reason while the channel sits on the firmware PID after a fault), `note`.

`alarms[]`: `kind` (`water_high`, `cpu_high`, `pump_low`, `fan_stopped`, `stale`, `fallback`, `foreign`, `budget`), `channel`, `message`, `value`, `limit`, `since_ms`.

The first snapshot is available before the first control tick: `controls` may be empty and the UI must tolerate that. The UI polls `snapshot` at 1 Hz. History is requested only on range or selection change and every 10 s / 60 s / 5 min for 1 h / 1 d / 1 w; the daemon keeps 6 h in RAM.

## Commands

- `set_mode {id, mode, confirmed}`: `active` is refused unless `writes_enabled` is set and the model declares the channel identified. The pump needs `confirmed: true` for `active` (UI two-step confirm).
- `set_weight {weight}`: 0..1.
- `start_step_test {id, confirmed}` / `stop_step_test`: duty step excitation from config, off unless started explicitly, floors and fault fallback stay active. Needs `confirmed: true`.

Commands are rate limited per connection; the daemon enforces every safety limit, never the UI.

## Pipe ACL for a user process talking to a SYSTEM daemon

The daemon passes explicit `SECURITY_ATTRIBUTES` (`SDDL` in `src/ipc.rs`): SYSTEM, Administrators and the owner full control, interactive users the mask `0x0012019b`, which allows read and write but not creating further pipe instances (`GRGW` includes `FILE_CREATE_PIPE_INSTANCE`, so a rogue server could otherwise squat the name). The first instance is created with `FILE_FLAG_FIRST_PIPE_INSTANCE` and `PIPE_REJECT_REMOTE_CLIENTS`.
