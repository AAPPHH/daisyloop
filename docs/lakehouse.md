# Lakehouse

Arrow in memory, Arrow IPC on disk while hot, Parquet in Iceberg when cold, DuckDB as engine.

## Layout (data dir `%LOCALAPPDATA%\daisyloop`, override `DAISYLOOP_DATA_DIR`)

- `hot/telemetry-<start>.arrows`, `hot/d5_raw-<start>.arrows`: Arrow IPC stream, one RecordBatch per tick, schema as the Iceberg tables, strings dictionary-encoded (Int16 keys, delta dictionaries), flushed per tick, fsync every `wal_sync_secs`. About 13 KB/s (1.1 GB/day) at 444 rows per tick versus 75 KB/s for the former JSON WAL. A torn tail is dropped by the reader.
- `hot/*.arrows.sealed`: a file closed by an hourly rename (`commit_secs`, default 3600). The rename is unconditional and costs nothing. Clean stop and startup only seal; a legacy `wal.log` is renamed to `wal-<micros>.sealed` at startup and migrated through the same path.
- `catalog.db` and `warehouse/`: Iceberg SQL catalog and tables `telemetry`, `d5_raw`. Each rotated file becomes one commit of one sorted (channel, ts) zstd Parquet file with untruncated channel statistics.
- `committed-<exe size>-<exe mtime>`: marker that this build has committed once.

## Rotation gate

The commit thread runs at low priority and handles sealed files oldest first. It starts a file only when one of these holds, and re-checks before every file, so rising load stops it after the current file:

1. Idle: CPU total load (`gate.cpu_channel`) below `gate.cpu_pct` (15) and GPU utilization (`gate.gpu_channel`) below `gate.gpu_pct` (10), both for `gate.sustain_secs` (300), from the live telemetry of the tick loop. A missing or stale (`gate.stale_secs`) sensor counts as not idle.
2. Fallback, logged as a warning, run at lowest (background) thread priority: the oldest sealed file is older than `gate.fallback_age_secs` (24 h) or sealed data exceeds half of `wal_max_bytes`.
3. First commit of a new build (no marker): unconditional, so the installer can verify `committed N telemetry`.

A commit appends to Iceberg with the file name as the snapshot source, then creates `<file>.done`, removes the file, then the marker. A crash in between is safe: a sealed file whose source is already in a snapshot is skipped and deleted. Unreadable files are renamed `.bad`. If hot plus sealed data reaches `wal_max_bytes`, new ticks are dropped with a loud error until the backlog drains.

## Reading

`python/lake.py` (venv `python/.venv`, `duckdb==2.0.0` pinned in `python/requirements.txt`, DuckDB 1.3.1 loses `channel =` filters on files without manifest bounds) opens one DuckDB connection with views `telemetry` and `d5_raw`: Iceberg (catalog pointer read at open) union all the hot Arrow files that are not yet in a snapshot. Call `refresh()` for newer data. Files are read into memory, never mapped, so the collector can rename and delete them. `load.py` and `rawstore.py` use these views.

Tests: `cargo test` writes the fixture lake `target/test-tmp/lake-fixture`; then `python/.venv/Scripts/python.exe python/test_lake.py`.
