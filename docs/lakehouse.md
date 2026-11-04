# Lakehouse

Arrow in memory, Arrow IPC on disk while hot, Parquet in Iceberg when cold, DuckDB as engine.

## Layout (data dir `%LOCALAPPDATA%\daisyloop`, override `DAISYLOOP_DATA_DIR`)

- `hot/telemetry-<start>.arrows`, `hot/d5_raw-<start>.arrows`: Arrow IPC stream, one RecordBatch per tick, schema as the Iceberg tables, strings dictionary-encoded (Int16 keys, delta dictionaries), flushed per tick, fsync every `wal_sync_secs`. About 13 KB/s (1.1 GB/day) at 444 rows per tick versus 75 KB/s for the former JSON WAL.
- `hot/<file>.arrows.sync`: the integrity sidecar, see below. It keeps the open name (no `.sealed`) and is deleted with the data file.
- `hot/*.arrows.sealed`: a file closed by an hourly rename (`commit_secs`, default 3600). The rename is unconditional and costs nothing. Clean stop and startup only seal (startup retries a rename that another reader blocks, and never fails the start); a legacy `wal.log` is renamed to `wal-<micros>.sealed` at startup and migrated through the same path.
- `*.partial`, `*.bad`: parked files, never retried. They count against `wal_max_bytes` and are listed in one log line at start.
- `catalog.db` and `warehouse/`: Iceberg SQL catalog and tables `telemetry`, `d5_raw`. Each rotated file becomes one commit of one sorted (channel, ts) zstd Parquet file with untruncated channel statistics.
- `committed-<exe size>-<exe mtime>`: marker that this build has committed at least one telemetry row. The installer removes it on every install, so a reinstall of the same build verifies a fresh commit.

## Integrity

After every fsync of a data file the writer appends one 16 byte record to `<file>.sync` and fsyncs it: end offset (u64), crc32 of the bytes since the previous mark (u32), crc32 of those 12 bytes (u32). A new file marks its schema header at once.

Readers (Rust committer, Python view) verify before any Arrow parse and trust only the verified prefix:

- Bytes after the last valid mark are the unsynced tail of a crashed file. They are dropped with a warning; the file is then committed as usual.
- A last mark that is itself torn is part of that tail. A bad mark or checksum mismatch in the middle, a mark beyond the end of the data, or a data file without a `.sync` file keep the verified prefix and mark the file damaged: the Rust committer commits the prefix, logs an error and moves the file to `.partial`. The Python view logs the same and shows the prefix.
- The parse of the verified prefix runs under `catch_unwind` and must consume exactly the verified length, else the file is damaged as above. A panic or unreadable header moves the file to `.bad`.
- Before a commit every batch is checked: schema equals the table schema, `ts` inside `[start - 1 day, max(start, mtime) + 1 day]` of the file, no empty channel. Otherwise the file goes to `.bad`.
- The file being written is readable in the view up to its last mark, at most `wal_sync_secs` old.
- Writer side: any append, flush, fsync or dictionary overflow error abandons that stream, seals it (its tail past the last mark is dropped by the readers), logs an error and opens a fresh stream. A failed final sync in a rotation seals what is verified.
- The legacy JSON WAL has a crc per record. A torn tail is dropped; a bad or unparsable record in the middle keeps the prefix and moves the file to `.partial`. A NaN stored as `null` reads back as NaN.

## Rotation gate

The commit thread runs at low priority and handles sealed files in name order (legacy `wal-*.sealed` first, then `hot/`, which sorts `d5_raw` before `telemetry`). It starts a file only when one of these holds, and re-checks before every file, so rising load stops it after the current file:

1. Idle: CPU total load (`gate.cpu_channel`) below `gate.cpu_pct` (15) and GPU utilization (`gate.gpu_channel`) below `gate.gpu_pct` (10), both for `gate.sustain_secs` (300), from the live telemetry of the tick loop. A missing or stale (`gate.stale_secs`) sensor counts as not idle.
2. Fallback, logged as a warning, run at lowest (background) thread priority: the oldest sealed file is older than `gate.fallback_age_secs` (24 h) or sealed data exceeds half of `wal_max_bytes`.
3. First commit of a new build (no marker): unconditional, so the installer can verify `committed N telemetry`.

All thresholds are configuration. Once a day the committer logs how long the gate stayed closed and how many sealed bytes were waiting, to judge whether the idle window opens often enough.

A commit appends to Iceberg with the file name as the snapshot source, then creates `<file>.done`, removes the file and its `.sync`, then removes `.done`. The first-commit marker is written right after the append, only when at least one telemetry row was committed; a file with no rows is deleted without a log line. A crash in between is safe: a sealed file whose source is already in a snapshot is skipped and deleted. If hot plus sealed plus parked data reaches `wal_max_bytes`, new ticks are dropped with a loud error until the backlog drains.

## Reading

`python/lake.py` (venv `python/.venv`, `duckdb==2.0.0` pinned in `python/requirements.txt`, DuckDB 1.3.1 loses `channel =` filters on files without manifest bounds) opens one DuckDB connection with views `telemetry` and `d5_raw`: Iceberg (catalog pointer read at open) union all the hot Arrow files that are not yet in a snapshot. Call `refresh()` for newer data, `refresh(t0, t1)` (epoch seconds) to read only hot files that overlap that range.

- A locked catalog is waited for `lock_timeout` seconds, then raises; a missing catalog or table raises `LakeError`. The view is never silently empty.
- A rename or delete between listing and reading (`FileNotFoundError`, `PermissionError`) re-lists and retries until the catalog pointer is unchanged across the read.
- Files are read into memory and closed before they are parsed, never mapped, so the collector can rename and delete them.
- `refresh()` aborts a still-open result of the connection first.
- `RawStore.build` records the row count and refuses to rebuild without row growth unless `force=True`; it writes through a temp file.

`load.py` and `rawstore.py` use these views.

Tests: `cargo test` writes the fixture lake `target/test-tmp/lake-fixture`; then `python/.venv/Scripts/python.exe python/test_lake.py`.

## Compaction (planned, not built)

Compaction rewrites many small Parquet files into few sorted ones. The rollback needs a snapshot that survives pruning:

- `prune` in `src/lake.rs` keeps only `keep + 2` metadata versions per table and deletes the manifest lists of expired snapshots. With hourly commits a pre-compaction `metadata.json` is gone after about 12 commits, so "go back to the previous metadata.json" holds for half a day at most.
- Before compacting, copy the current `metadata.json` out of the pruned directory (for example `warehouse/rollback/<table>-<timestamp>.metadata.json`) and keep every data, manifest and manifest-list file it references until the verification is signed off. Compaction must not delete the replaced data files itself, and `prune`/expiry must skip anything the rollback copy references. Rolling back is then re-pointing the catalog row to that copy.
- Verify by comparing row counts and per channel statistics; sums must be NaN-aware (compare the count of NaN values, then sum the rest), a plain per channel sum fails on NaN.
