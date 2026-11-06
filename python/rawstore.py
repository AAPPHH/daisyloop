import json
import os
import numpy as np
from load import Loader

CONFIG = {
    "cache_dir": os.path.join(os.environ.get("LOCALAPPDATA", "."), "daisyloop-ml", "cache"),
    "chunk_s": 86400,
}


class RawStore:
    def __init__(self, config=CONFIG):
        self.cfg = config
        self.dir = config["cache_dir"]
        self.meta_path = os.path.join(self.dir, "meta.json")
        self.data_path = os.path.join(self.dir, "raw.npy")

    def build(self, force=False, lake=None):
        os.makedirs(self.dir, exist_ok=True)
        loader = Loader(lake=lake)
        table = loader.cfg["table"]
        con = loader.con
        rows = con.execute(f"select count(*) from {table}").fetchone()[0]
        if os.path.exists(self.meta_path) and not force:
            with open(self.meta_path) as f:
                known = json.load(f).get("rows", 0)
            if rows <= known:
                raise RuntimeError(f"{table} has {rows} rows, the store was built from {known}: no growth, pass force=True to rebuild")
        t0, t1 = con.execute(f"select min(epoch(ts)), max(epoch(ts)) from {table}").fetchone()
        k0, k1 = int(t0), int(t1) - 120
        chans = con.execute(
            f"select channel, any_value(source), any_value(label), any_value(kind), any_value(unit), any_value(role) from {table} group by channel order by channel"
        ).fetchall()
        col = {c[0]: i for i, c in enumerate(chans)}
        tmp = self.data_path + ".tmp"
        out = np.lib.format.open_memmap(tmp, mode="w+", dtype=np.float32, shape=(k1 - k0 + 1, len(chans)))
        out[:] = np.nan
        step = self.cfg["chunk_s"]
        for a in range(k0, k1 + 1, step):
            b = min(a + step, k1 + 1)
            d = con.execute(
                f"select cast(floor(epoch(ts)) as bigint) k, channel, avg(value) v from {table} where epoch(ts) >= ? and epoch(ts) < ? group by all",
                [float(a), float(b)],
            ).fetchnumpy()
            if len(d["k"]):
                out[d["k"] - k0, [col[c] for c in d["channel"]]] = d["v"]
        out.flush()
        del out
        os.replace(tmp, self.data_path)
        meta = {"rows": rows, "k0": k0, "k1": k1, "channels": [dict(zip(("channel", "source", "label", "kind", "unit", "role"), c)) for c in chans]}
        with open(self.meta_path, "w") as f:
            json.dump(meta, f)

    def load(self):
        with open(self.meta_path) as f:
            meta = json.load(f)
        return meta, np.load(self.data_path, mmap_mode="r")


if __name__ == "__main__":
    RawStore().build()
