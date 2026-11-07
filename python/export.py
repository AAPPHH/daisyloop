import json
import math
import os
import pickle
import numpy as np
from experiments import Stage2
from features import BatchEngine, PHYS, PhysSpec, Spec, StreamEngine, CONFIG as FCFG
from models import CONFIG, FRESH, LIN_TARGETS, Dataset, local_idx
from rawstore import CONFIG as RawCFG, RawStore

EXPORT_TARGETS = [t for t in LIN_TARGETS if t.startswith("water_")]

CFG = {
    "tag": "x44aug",
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
        from experiments import Stage2
        pruned, _ = Stage2().pruned(cfg["feature_count"])
        self.feats = pruned + [n for n in CONFIG["forced"] if n not in pruned]
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
            if not tg.startswith("water_"):
                continue
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


LCFG = {
    "pickles": [os.path.join(os.path.dirname(FRESH["cache"]), f) for f in ("lin_v3.pkl", "lin_v5.pkl")],
    "tag": "svr+|pint@new",
    "tag_fallback": "svr+|pint~nolhm@new",
    "spec_out": os.path.join(CONFIG["out_dir"], "feature_spec_linear.json"),
    "model_out": os.path.join(CONFIG["out_dir"], "model_linear.json"),
    "golden_out": os.path.join(CONFIG["out_dir"], "golden_linear.json"),
    "golden_span": CFG["golden_span"],
    "golden_every": 10,
}


class LinearExporter:
    def __init__(self, cfg=LCFG):
        self.cfg = cfg
        res = {r["tag"]: r["res"] for f in cfg["pickles"] for r in pickle.load(open(f, "rb"))}
        self.full, self.fb = res[cfg["tag"]], res[cfg["tag_fallback"]]

    @staticmethod
    def block(res, targets):
        return {tg: {"base": float(res["targets"][tg]["b"]), "lin": {"w": res["targets"][tg]["w"].tolist(), "mean": res["targets"][tg]["mean"].tolist(), "std": res["targets"][tg]["std"].tolist()}} for tg in targets}

    def spec(self):
        ps = PhysSpec(Spec.load(FCFG["spec_path"]), dict(PHYS, pint=True))
        sub = ps.build().subset(set(self.full["feats"]) | set(self.fb["feats"]))
        sub.meta["deployed_outputs"] = self.full["feats"]
        sub.save(self.cfg["spec_out"])
        return sub

    def model(self):
        out = {"kind": "linear", "features": self.full["feats"], "identified": {"fan": False, "pump": False}, "clip": self.full["clip"], "targets": self.block(self.full, EXPORT_TARGETS),
               "fallback": {"features": self.fb["feats"], "targets": self.block(self.fb, EXPORT_TARGETS)}}
        with open(self.cfg["model_out"], "w") as f:
            json.dump(out, f, separators=(",", ":"))
        return out

    @staticmethod
    def term(model, x, tg):
        r = model.get("response")
        series, h = tg.split("_")
        if not r or series not in r["gain"]:
            return 0.0
        f = model["features"]
        u = min(max((x[f.index(r["pump_feat"])] - r["pump"][0]) / (r["pump"][1] - r["pump"][0]), 0.0), 1.0)
        dead = r["dead"][0] + (r["dead"][1] - r["dead"][0]) * u
        return r["gain"][series] * r["k"] * max(float(h) - dead, 0.0) * x[f.index(r["feat"])]

    @staticmethod
    def predict(model, x):
        x = np.asarray(x, dtype=np.float64)
        free = np.array([n in model.get("free", []) for n in model["features"]])
        def dot(t, v, clip, fr):
            z = np.where(np.isfinite(v), (v - np.array(t["lin"]["mean"])) / np.array(t["lin"]["std"]), 0.0)
            return float(t["base"] + np.array(t["lin"]["w"]) @ np.where(fr, z, np.clip(z, -clip, clip)))
        primary = all(w == 0.0 or np.isfinite(x[j]) for t in model["targets"].values() for j, w in enumerate(t["lin"]["w"]))
        if primary or not model.get("fallback"):
            return {tg: dot(t, x, model["clip"], free) - LinearExporter.term(model, x, tg) for tg, t in model["targets"].items()}
        idx = [model["features"].index(n) if n in model["features"] else -1 for n in model["fallback"]["features"]]
        v = np.array([x[i] if i >= 0 else np.nan for i in idx])
        return {tg: dot(t, v, model["clip"], free[idx]) - LinearExporter.term(model, x, tg) for tg, t in model["fallback"]["targets"].items()}

    def golden(self, spec, model):
        meta, X = RawStore(self.cfg.get("store", RawCFG)).load()
        a, b = (local_idx(meta, s) for s in self.cfg["golden_span"])
        chans = spec.raw_channels()
        ix = {c["channel"]: i for i, c in enumerate(meta["channels"])}
        sub = np.asarray(X[a:b])
        cols = [ix[c] for c in chans]
        rows = [[None if math.isnan(float(sub[t, c])) else float(str(np.float32(sub[t, c]))) for c in cols] for t in range(sub.shape[0])]
        eng = StreamEngine(spec)
        k0 = meta["k0"] + a
        ticks, feats, preds, fb = [], [], [], 0
        for t, row in enumerate(rows):
            v = eng.step({c: (math.nan if x is None else x) for c, x in zip(chans, row)}, k0 + t)
            if t % self.cfg["golden_every"] == 0:
                x = [v[n] for n in model["features"]]
                ticks.append(t)
                feats.append([None if math.isnan(y) else y for y in x])
                preds.append({k: (v if math.isfinite(v) else None) for k, v in self.predict(model, x).items()})
                fb += not all(math.isfinite(y) for y in x)
        g = {"channels": chans, "k0": k0, "rows": rows, "ticks": ticks, "features": model["features"], "expected_features": feats, "expected_predictions": preds}
        with open(self.cfg["golden_out"], "w") as f:
            json.dump(g, f, separators=(",", ":"))
        return {"ticks": len(ticks), "fallback_ticks": fb}

    def run(self):
        spec = self.spec()
        model = self.model()
        return {"nodes": len(spec.nodes), "raw": len(spec.raw_channels()), **self.golden(spec, model)}


if __name__ == "__main__":
    import sys
    print(LinearExporter().run() if len(sys.argv) > 1 and sys.argv[1] == "linear" else Exporter().run())
