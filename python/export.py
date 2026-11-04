import json
import math
import os
import pickle
import numpy as np
from experiments import Stage2
from features import BatchEngine, Spec, StreamEngine, CONFIG as FCFG
from models import CONFIG, Dataset, local_idx
from rawstore import RawStore

CFG = {
    "tag": "x40aug",
    "stage": "stage2.pkl",
    "spec_out": os.path.join(CONFIG["out_dir"], "feature_spec_deploy.json"),
    "model_out": os.path.join(CONFIG["out_dir"], "model_deploy.json"),
    "golden_out": os.path.join(CONFIG["out_dir"], "golden.json"),
    "golden_span": ("2026-11-04 07:00", "2026-11-04 09:00"),
    "golden_every": 10,
    "feature_count": 40,
}


class Exporter:
    def __init__(self, cfg=CFG):
        self.cfg = cfg
        self.D = Dataset(CONFIG).load()
        self.rich = Spec.load(FCFG["spec_path"])
        self.feats, _ = Stage2().pruned(cfg["feature_count"])
        self.stage = {r["tag"]: r["res"] for r in pickle.load(open(os.path.join(CONFIG["out_dir"], cfg["stage"]), "rb"))}[cfg["tag"]]

    def spec(self):
        sub = self.rich.subset(set(self.feats))
        sub.meta["deployed_outputs"] = self.feats
        sub.save(self.cfg["spec_out"])
        return sub

    @staticmethod
    def flat(tree):
        f, t, l, r, m, v = [], [], [], [], [], []

        def walk(n):
            i = len(f)
            f.append(-1), t.append(0.0), l.append(-1), r.append(-1), m.append(0), v.append(0.0)
            if "leaf" in n:
                v[i] = n["leaf"]
                return i
            f[i], t[i] = int(n["split"][1:]), n["split_condition"]
            ch = {c["nodeid"]: c for c in n["children"]}
            l[i] = walk(ch[n["yes"]])
            r[i] = walk(ch[n["no"]])
            m[i] = 1 if n["missing"] == n["yes"] else 0
            return i

        walk(tree)
        return {"f": f, "t": t, "l": l, "r": r, "m": m, "v": v}

    def model(self):
        import xgboost as xgb
        out = {"features": self.feats, "identified": {"fan": False, "pump": False}, "targets": {}}
        for tg, r in self.stage.items():
            b = xgb.Booster()
            b.load_model(os.path.join(CONFIG["out_dir"], f"{self.cfg['tag']}_{tg}.json"))
            n = r["best_iter"] + 1
            base = float(json.loads(b.save_config())["learner"]["learner_model_param"]["base_score"].strip("[]"))
            trees = [self.flat(json.loads(s)) for s in b.get_dump(dump_format="json")[:n]]
            out["targets"][tg] = {"base": base, "trees": trees}
        with open(self.cfg["model_out"], "w") as f:
            json.dump(out, f, separators=(",", ":"))
        return out

    @staticmethod
    def predict(model, x):
        res = {}
        xf = np.asarray(x, dtype=np.float32)
        for tg, m in model["targets"].items():
            s = m["base"]
            for t in m["trees"]:
                i = 0
                while t["f"][i] >= 0:
                    v = xf[t["f"][i]]
                    left = t["m"][i] == 1 if np.isnan(v) else v < np.float32(t["t"][i])
                    i = t["l"][i] if left else t["r"][i]
                s += t["v"][i]
            res[tg] = float(s)
        return res

    def golden(self, spec, model):
        meta, X = RawStore().load()
        a, b = (local_idx(meta, s) for s in self.cfg["golden_span"])
        chans = spec.raw_channels()
        ix = {c["channel"]: i for i, c in enumerate(meta["channels"])}
        sub = np.asarray(X[a:b])
        cols = [ix[c] for c in chans]
        rows = []
        for t in range(sub.shape[0]):
            rows.append([None if math.isnan(float(sub[t, c])) else float(str(np.float32(sub[t, c]))) for c in cols])
        eng = StreamEngine(spec)
        k0 = meta["k0"] + a
        every = self.cfg["golden_every"]
        ticks, feats, preds = [], [], []
        for t, row in enumerate(rows):
            v = eng.step({c: (math.nan if x is None else x) for c, x in zip(chans, row)}, k0 + t)
            if t % every == 0:
                x = [v[n] for n in self.feats]
                ticks.append(t)
                feats.append([None if math.isnan(y) else y for y in x])
                preds.append(self.predict(model, x))
        g = {"channels": chans, "k0": k0, "rows": rows, "ticks": ticks, "features": self.feats, "expected_features": feats, "expected_predictions": preds}
        with open(self.cfg["golden_out"], "w") as f:
            json.dump(g, f, separators=(",", ":"))
        return g

    def booster_check(self, model):
        import xgboost as xgb
        D = self.D
        ci = [D.col[n] for n in self.feats]
        worst = 0.0
        for tg, m in model["targets"].items():
            b = xgb.Booster()
            b.load_model(os.path.join(CONFIG["out_dir"], f"{self.cfg['tag']}_{tg}.json"))
            n = self.stage[tg]["best_iter"] + 1
            ix = D.sel("test", tg)[::40]
            ref = b.predict(xgb.DMatrix(D.F[ix][:, ci]), iteration_range=(0, n))
            mine = np.array([self.predict({"targets": {tg: m}}, D.F[i, ci])[tg] for i in ix])
            worst = max(worst, float(np.abs(ref - mine).max()))
        return worst

    def run(self):
        spec = self.spec()
        model = self.model()
        worst = self.booster_check(model)
        assert worst < 1e-4, worst
        g = self.golden(spec, model)
        return {"nodes": len(spec.nodes), "raw": len(spec.raw_channels()), "trees": {k: len(v["trees"]) for k, v in model["targets"].items()},
                "booster_max_abs_diff": worst, "golden_ticks": len(g["ticks"])}


if __name__ == "__main__":
    print(Exporter().run())
