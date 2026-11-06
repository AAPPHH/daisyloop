import glob
import json
import logging
import os
import pathlib
import sqlite3
import struct
import time
import zlib
import duckdb
import pyarrow as pa
import pyarrow.ipc as ipc

CONFIG = {
    "data_dir": os.environ.get("DAISYLOOP_DATA_DIR") or os.path.join(os.environ.get("LOCALAPPDATA", "."), "daisyloop"),
    "catalog": "daisyloop",
    "namespace": "daisyloop",
    "source_key": "daisyloop.source",
    "retries": 20,
    "retry_wait": 0.1,
    "lock_timeout": 10,
    "tables": {
        "telemetry": {
            "columns": {"ts": "timestamptz", "source": "varchar", "channel": "varchar", "label": "varchar", "kind": "varchar", "unit": "varchar", "role": "varchar", "value": "double"},
            "empty": pa.schema([("ts", pa.timestamp("us", "+00:00")), *[(n, pa.string()) for n in ("source", "channel", "label", "kind", "unit", "role")], ("value", pa.float64())]),
        },
        "d5_raw": {
            "columns": {"ts": "timestamptz", "report": "blob"},
            "empty": pa.schema([("ts", pa.timestamp("us", "+00:00")), ("report", pa.large_binary())]),
        },
    },
}


MARK = struct.Struct("<QII")
BODY = 12
TAIL_FLOOR = 128 << 10
TAIL_SPANS = 8
SCHEMA_MAX = 1024
LOG = logging.getLogger("lake")


class LakeError(RuntimeError):
    pass


class Lake:
    def __init__(self, config=CONFIG):
        self.cfg = config
        self.dir = os.path.abspath(config["data_dir"])
        self.con = duckdb.connect()
        self.con.execute("INSTALL iceberg")
        self.con.execute("LOAD iceberg")
        self.refresh()

    def location(self, table):
        path = os.path.join(self.dir, "catalog.db")
        if not os.path.exists(path):
            raise LakeError(f"no catalog at {path}")
        db = sqlite3.connect(pathlib.Path(path).as_uri() + "?mode=ro", uri=True, timeout=self.cfg["lock_timeout"])
        try:
            row = db.execute(
                "select metadata_location from iceberg_tables where catalog_name=? and table_namespace=? and table_name=?",
                (self.cfg["catalog"], self.cfg["namespace"], table),
            ).fetchone()
        finally:
            db.close()
        if row is None:
            raise LakeError(f"table {table} is not in the catalog at {path}")
        return row[0]

    def snapshots(self, location):
        with open(location.removeprefix("file:///").removeprefix("file://"), encoding="utf-8") as f:
            return json.load(f).get("snapshots") or []

    def hot_files(self, table, t0=None, t1=None):
        found = {}
        for ext in (".arrows", ".arrows.sealed"):
            for p in glob.glob(os.path.join(self.dir, "hot", f"{table}-*{ext}")):
                found.setdefault(os.path.basename(p).removesuffix(".sealed"), []).append(p)
        return {stem: paths for stem, paths in sorted(found.items()) if self.in_range(stem, paths, t0, t1)}

    @staticmethod
    def in_range(stem, paths, t0, t1):
        if t0 is None and t1 is None:
            return True
        start = int(stem.split("-", 1)[1].split(".", 1)[0]) / 1e6
        if t1 is not None and start >= t1:
            return False
        sealed = [p for p in paths if p.endswith(".sealed")]
        return t0 is None or not sealed or os.path.getmtime(sealed[0]) >= t0

    @staticmethod
    def walk(data, marks):
        recs = [marks[i:i + MARK.size] for i in range(0, len(marks) - MARK.size + 1, MARK.size)]
        broken = [zlib.crc32(r[:BODY]) != MARK.unpack(r)[2] for r in recs]
        at = widest = 0
        for i, r in enumerate(recs):
            if broken[i]:
                return at, (None if all(broken[i:]) else f"mark {i} is corrupt"), widest
            end, crc, _ = MARK.unpack(r)
            if end < at or end > len(data):
                return at, f"mark {i} ends at {end}, file has {len(data)} bytes, verified {at}", widest
            if zlib.crc32(data[at:end]) != crc:
                return at, f"checksum mismatch in bytes {at}..{end}", widest
            widest = max(widest, end - at)
            at = end
        return at, None, widest

    @classmethod
    def verified(cls, data, marks):
        at, flaw, widest = cls.walk(data, marks)
        tail, limit = len(data) - at, max(TAIL_FLOOR, TAIL_SPANS * widest)
        if flaw is None and at == 0 and tail > SCHEMA_MAX:
            flaw = f"no valid sync mark for {tail} bytes, a healthy file marks its schema first"
        elif flaw is None and tail > limit:
            flaw = f"unsynced tail of {tail} bytes exceeds the {limit} bytes one sync interval can write"
        return at, flaw

    @classmethod
    def read_stream(cls, paths):
        name = paths[0].removesuffix(".sealed")
        try:
            with open(name + ".sync", "rb") as f:
                marks = f.read()
        except FileNotFoundError:
            marks = None
        with open(paths[0], "rb") as f:
            data = f.read()
        n, flaw = cls.verified(data, marks or b"")
        if marks is None and data:
            flaw = "no sync file, nothing is verified"
        if flaw:
            LOG.error("%s: %s, only the verified %d bytes are shown", paths[0], flaw, n)
        batches = []
        if n:
            try:
                for b in ipc.open_stream(pa.py_buffer(memoryview(data)[:n])):
                    batches.append(b)
            except (pa.ArrowInvalid, OSError) as e:
                LOG.error("%s: verified bytes do not parse, %d batches shown: %s", paths[0], len(batches), e)
        return batches

    def load_hot(self, table, t0=None, t1=None):
        return {stem: self.read_stream(paths) for stem, paths in self.hot_files(table, t0, t1).items() if not any(os.path.exists(p + ".done") for p in paths)}

    def read_state(self, t0, t1):
        state = {}
        for table in self.cfg["tables"]:
            location = self.location(table)
            snaps = self.snapshots(location)
            committed = {s.get("summary", {}).get(self.cfg["source_key"]) for s in snaps}
            loaded = {stem: bs for stem, bs in self.load_hot(table, t0, t1).items() if stem not in committed}
            state[table] = (location, bool(snaps), [b for bs in loaded.values() for b in bs])
        return state

    def refresh(self, t0=None, t1=None):
        for _ in range(self.cfg["retries"]):
            try:
                state = self.read_state(t0, t1)
                if all(self.location(t) == state[t][0] for t in state):
                    break
            except (FileNotFoundError, PermissionError):
                pass
            time.sleep(self.cfg["retry_wait"])
        else:
            raise LakeError("no consistent listing of the lake after retries")
        self.con.abort()
        for table, spec in self.cfg["tables"].items():
            location, has_snaps, batches = state[table]
            hot = pa.Table.from_batches(batches) if batches else spec["empty"].empty_table()
            self.con.register(f"hot_{table}", hot)
            cols = ", ".join(f"{n}::{t} as {n}" for n, t in spec["columns"].items())
            parts = [f"select {cols} from hot_{table}"]
            if has_snaps:
                parts.insert(0, f"select {', '.join(spec['columns'])} from iceberg_scan('{location}')")
            self.con.execute(f"create or replace view {table} as " + " union all ".join(parts))
        return self

    def execute(self, sql, args=None):
        return self.con.execute(sql, args or [])


class CopyLake(Lake):
    def location(self, table):
        names = sorted(glob.glob(os.path.join(self.dir, "warehouse", self.cfg["namespace"], table, "metadata", "*.metadata.json")))
        return names[-1]

    def refresh(self, t0=None, t1=None):
        pass
        for table, spec in self.cfg["tables"].items():
            if table != "telemetry":
                continue
            snaps = self.snapshots(self.location(table))
            committed = {s.get("summary", {}).get(self.cfg["source_key"]) for s in snaps}
            batches = [b for stem, bs in self.load_hot(table, t0, t1).items() if stem not in committed for b in bs]
            hot = pa.Table.from_batches(batches) if batches else spec["empty"].empty_table()
            self.con.register(f"hot_{table}", hot)
            cols = ", ".join(f"{n}::{t} as {n}" for n, t in spec["columns"].items())
            files = os.path.join(self.dir, "warehouse", self.cfg["namespace"], table, "data", "*.parquet").replace("\\", "/")
            self.con.execute(f"create or replace view {table} as select {', '.join(spec['columns'])} from read_parquet('{files}') union all select {cols} from hot_{table}")
        return self
