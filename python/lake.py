import glob
import json
import os
import pathlib
import sqlite3
import duckdb
import pyarrow as pa
import pyarrow.ipc as ipc

CONFIG = {
    "data_dir": os.environ.get("DAISYLOOP_DATA_DIR") or os.path.join(os.environ.get("LOCALAPPDATA", "."), "daisyloop"),
    "catalog": "daisyloop",
    "namespace": "daisyloop",
    "source_key": "daisyloop.source",
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
            return None
        db = sqlite3.connect(pathlib.Path(path).as_uri() + "?mode=ro", uri=True)
        try:
            row = db.execute(
                "select metadata_location from iceberg_tables where catalog_name=? and table_namespace=? and table_name=?",
                (self.cfg["catalog"], self.cfg["namespace"], table),
            ).fetchone()
        except sqlite3.OperationalError:
            row = None
        finally:
            db.close()
        return None if row is None else row[0]

    def snapshots(self, location):
        with open(location.removeprefix("file:///").removeprefix("file://"), encoding="utf-8") as f:
            return json.load(f).get("snapshots") or []

    def hot_files(self, table):
        found = {}
        for ext in (".arrows", ".arrows.sealed"):
            for p in glob.glob(os.path.join(self.dir, "hot", f"{table}-*{ext}")):
                found.setdefault(os.path.basename(p).removesuffix(".sealed"), []).append(p)
        return dict(sorted(found.items()))

    @staticmethod
    def read_stream(paths):
        for p in paths:
            try:
                with open(p, "rb") as f:
                    data = f.read()
            except FileNotFoundError:
                continue
            batches = []
            try:
                for b in ipc.open_stream(pa.py_buffer(data)):
                    batches.append(b)
            except (pa.ArrowInvalid, OSError):
                pass
            return batches
        return []

    def load_hot(self, table):
        return {stem: self.read_stream(paths) for stem, paths in self.hot_files(table).items() if not any(os.path.exists(p + ".done") for p in paths)}

    def refresh(self):
        for table, spec in self.cfg["tables"].items():
            loaded = self.load_hot(table)
            location = self.location(table)
            snaps = self.snapshots(location) if location else []
            committed = {s.get("summary", {}).get(self.cfg["source_key"]) for s in snaps}
            batches = [b for stem, bs in loaded.items() if stem not in committed for b in bs]
            hot = pa.Table.from_batches(batches) if batches else spec["empty"].empty_table()
            self.con.register(f"hot_{table}", hot)
            cols = ", ".join(f"{n}::{t} as {n}" for n, t in spec["columns"].items())
            parts = [f"select {cols} from hot_{table}"]
            if snaps:
                parts.insert(0, f"select {', '.join(spec['columns'])} from iceberg_scan('{location}')")
            self.con.execute(f"create or replace view {table} as " + " union all ".join(parts))
        return self

    def execute(self, sql, args=None):
        return self.con.execute(sql, args or [])
