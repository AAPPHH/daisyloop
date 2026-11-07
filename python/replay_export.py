import json
import os
import sys

import numpy as np

import metastore

CFG = {"spec": os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "models", "feature_spec_deploy.json"), "phase": "daisyloop/step/phase", "cache": None}


class ReplayExport:
    def __init__(self, cfg=CFG):
        self.cfg = cfg

    def channels(self):
        spec = json.load(open(self.cfg["spec"]))
        raw = [n["p"]["channel"] for n in spec["nodes"] if n["op"] == "raw"]
        return raw, raw + [self.cfg["phase"]]

    def run(self, out):
        raw, want = self.channels()
        store_cfg = dict(metastore.CONFIG)
        store_cfg["cache_dir"] = self.cfg["cache"] or os.path.join(out, "cache")
        meta, X = metastore.HistoryStore(store_cfg).load(want)
        ix = {c["channel"]: i for i, c in enumerate(meta["channels"])}
        have = [c for c in want if c in ix]
        os.makedirs(out, exist_ok=True)
        data = np.empty((X.shape[0], len(have)), dtype=np.float32)
        for lo in range(0, X.shape[0], 50000):
            data[lo:lo + 50000] = np.asarray(X[lo:lo + 50000][:, [ix[c] for c in have]])
        data.tofile(os.path.join(out, "data.f32"))
        json.dump({"channels": have, "k0": meta["k0"], "rows": int(data.shape[0]), "raw": raw}, open(os.path.join(out, "meta.json"), "w"))
        return {"rows": int(data.shape[0]), "channels": len(have), "missing": [c for c in want if c not in ix]}


if __name__ == "__main__":
    print(ReplayExport().run(sys.argv[1]))
