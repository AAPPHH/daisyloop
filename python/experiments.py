import json
import os
import pickle
import sys
import time
import numpy as np
from models import CONFIG, FRESH, Dataset, Metrics, RunLog, build_task, physics_task, xgb_task
from nets import nn_task
from features import Spec, CONFIG as FCFG

CRITERIA = {
    "stage3": {
        "T1": "ablation in the final config (shallow trees, LHM-dropout aug): a group earns its place if dropping it raises test RMSE of water d2min or d5min by >=1% with the paired bootstrap interval above 0",
        "T2": "rich (all 354) vs pruned-40 vs small-20 in shallow+aug config: rich is deployed only if it beats pruned-40 by >=2% on val and test at water d2min; else pruned-40 or small, whichever is better, is deployed",
        "T3": "seed stability: x40aug test RMSE across 3 seeds within 2% of each other, otherwise treat differences below that as noise",
        "T4": "NN variants are kept only if they beat x40aug on val and test at water d2min and d5min; otherwise NN is dropped from deployment",
    },
    "stage2": {
        "S1": "pruned (top-40/80 train-gain) or shallow variants are kept only if val RMSE of water d2min and d5min is >=2% below rich and small on val AND not worse on test",
        "S2": "physics-informed residual net counts only if it beats the best XGB on val water d2min and d5min by >=3% and is not worse on test",
        "S3": "LHM-dropout augmentation (aug models) must keep test skill within 2% of its non-aug twin and must not lose to persistence on lhm_missing block by more than 5% at d30s/d2min",
        "S4": "x44aug (x40aug plus fan and pump rpm and duty) is deployed only if its held-out test RMSE of water d30s and d2min is not more than 2% above x40aug from the same run; identified flags stay false regardless",
    },
    "stage1": {
        "C1": "water d15min, test block: rich XGB RMSE <= 0.80 x persistence RMSE and <= 0.90 x best physics RMSE, with the 95% block-bootstrap interval of the skill vs physics above 0",
        "C2": "rich vs small (<=20 features): rich is kept only if RMSE_rich < 0.95 x RMSE_small on val AND test for water d5min and d15min; otherwise small (or pruned) set is deployed",
        "C3": "short horizons (30 s, 2 min): a model must beat persistence by >=10% on test; otherwise the report says persistence wins",
        "C4": "group ablation: a feature group earns its place if dropping it raises test RMSE of water d15min by >=1% (95% interval of the relative change above 0)",
        "C5": "out-of-regime blocks (pump 20%, LHM missing) are reported separately, never pooled with the test block",
    },
}


class Cluster:
    def __init__(self, cfg=CONFIG):
        import ray
        self.ray = ray
        ray.init(address="auto", logging_level=40, runtime_env={"env_vars": cfg["ray_env"]})
        self.cfg = cfg

    def submit(self, fn, arg, **res):
        r = dict(self.cfg["ray_res"])
        r.update(res)
        return self.ray.remote(**r)(fn).remote(arg)

    def resources(self):
        return self.ray.available_resources()


class Stage1:
    def __init__(self, cfg=CONFIG):
        self.cfg = cfg
        self.D = Dataset(cfg).load()
        self.spec = Spec.load(FCFG["spec_path"])
        self.log = RunLog(cfg)
        self.groups = self.spec.groups()
        self.out = os.path.join(cfg["out_dir"], "stage1.pkl")

    def jobs(self):
        D, cfg = self.D, self.cfg
        allt = D.tnames
        water = [f"water_{h}" for h in cfg["horizons"]]
        feats = D.names
        jobs = [{"tag": "rich", "feats": None, "targets": allt, "save": True},
                {"tag": "small", "feats": cfg["small"], "targets": allt, "save": True}]
        gnames = sorted(set(self.groups.values()))
        for g in gnames:
            jobs.append({"tag": f"drop:{g}", "feats": [n for n in feats if self.groups[n] != g], "targets": water})
        for g in gnames:
            jobs.append({"tag": f"only:{g}", "feats": [n for n in feats if self.groups[n] == g], "targets": water})
        return jobs

    def run(self):
        cl = Cluster(self.cfg)
        self.log.write(event="start", run="stage1", resources=self.cfg["ray_res"], criteria=CRITERIA["stage1"])
        t0 = time.time()
        status = "failed"
        try:
            futs = [cl.submit(physics_task, {})] + [cl.submit(xgb_task, j) for j in self.jobs()]
            res = [cl.ray.get(f) for f in futs]
            pickle.dump(res, open(self.out, "wb"))
            status = "ok"
        finally:
            self.log.write(event="stop", run="stage1", status=status, seconds=round(time.time() - t0))


class Stage2(Stage1):
    def __init__(self, cfg=CONFIG):
        super().__init__(cfg)
        self.out = os.path.join(cfg["out_dir"], "stage2.pkl")

    def pruned(self, k):
        res = pickle.load(open(os.path.join(self.cfg["out_dir"], "stage1.pkl"), "rb"))
        rich = [r for r in res if isinstance(r, dict) and r.get("tag") == "rich"][0]["res"]
        tot = {}
        for tg, r in rich.items():
            z = sum(r["gain"].values()) or 1.0
            for n, v in r["gain"].items():
                tot[n] = tot.get(n, 0.0) + v / z
        return [n for n, _ in sorted(tot.items(), key=lambda kv: -kv[1])[:k]], res[0]["water"]["theta"]

    def jobs(self):
        allt = self.D.tnames
        p40, th = self.pruned(40)
        p80, _ = self.pruned(80)
        pi = [th[0], th[1], th[3], th[4], th[5], th[6]]
        shallow = {"max_depth": 3, "min_child_weight": 100, "eta": 0.03}
        j = [{"tag": "x40", "feats": p40, "targets": allt}, {"tag": "x80", "feats": p80, "targets": allt},
             {"tag": "x80s", "feats": p80, "targets": allt, "params": shallow},
             {"tag": "x40s", "feats": p40, "targets": allt, "params": shallow},
             {"tag": "xsmall_s", "feats": self.cfg["small"], "targets": allt, "params": shallow},
             {"tag": "x40aug", "feats": p40, "targets": allt, "lhm_aug": 0.3, "params": shallow, "save": True},
             {"tag": "x44aug", "feats": p40 + [n for n in self.cfg["forced"] if n not in p40], "targets": allt, "lhm_aug": 0.3, "params": shallow, "save": True},
             {"tag": "x80aug", "feats": p80, "targets": allt, "lhm_aug": 0.3, "params": shallow},
             {"tag": "xsmall_aug", "feats": self.cfg["small"], "targets": allt, "lhm_aug": 0.3, "params": shallow, "save": True}]
        n = [{"tag": "nn_phys", "feats": p80, "phys_init": pi, "phys": True, "kind": "nn", "save": True},
             {"tag": "nn_plain", "feats": p80, "phys_init": pi, "phys": False, "kind": "nn"},
             {"tag": "nn_phys_aug", "feats": p80, "phys_init": pi, "phys": True, "lhm_aug": 0.3, "kind": "nn", "save": True},
             {"tag": "nn_phys_p40", "feats": p40, "phys_init": pi, "phys": True, "kind": "nn"}]
        return j + n

    def run(self):
        cl = Cluster(self.cfg)
        self.log.write(event="start", run="stage2", resources=self.cfg["ray_res"], criteria=CRITERIA["stage2"])
        t0 = time.time()
        status = "failed"
        try:
            futs = [cl.submit(nn_task if j.get("kind") == "nn" else xgb_task, j) for j in self.jobs()]
            res = [cl.ray.get(f) for f in futs]
            pickle.dump(res, open(self.out, "wb"))
            status = "ok"
        finally:
            self.log.write(event="stop", run="stage2", status=status, seconds=round(time.time() - t0))


class Stage3(Stage2):
    def __init__(self, cfg=CONFIG):
        super().__init__(cfg)
        self.out = os.path.join(cfg["out_dir"], "stage3.pkl")

    def jobs(self):
        D = self.D
        allt = D.tnames
        water = [f"water_{h}" for h in self.cfg["horizons"]]
        p40, th = self.pruned(40)
        pi = [th[0], th[1], th[3], th[4], th[5], th[6]]
        shallow = {"max_depth": 3, "min_child_weight": 100, "eta": 0.03}
        base = {"params": shallow, "lhm_aug": 0.3}
        j = [{"tag": "rs", "feats": None, "targets": allt, "params": shallow},
             {"tag": "rs_aug", "feats": None, "targets": allt, **base}]
        for sd in (1, 2):
            j.append({"tag": f"x40aug_s{sd}", "feats": p40, "targets": allt, "seed": sd, **base})
        gnames = sorted(set(self.groups.values()))
        for g in gnames:
            j.append({"tag": f"dropS:{g}", "feats": [n for n in D.names if self.groups[n] != g], "targets": water, **base})
            j.append({"tag": f"onlyS:{g}", "feats": [n for n in D.names if self.groups[n] == g], "targets": water, **base})
        small = self.cfg["small"]
        for tag, f, kw in [("nn_small", small, {"phys": False}), ("nn_small_phys", small, {"phys": True}),
                           ("nn_p40_reg", p40, {"phys": False, "hidden": 32, "drop": 0.4, "wd": 0.1, "epochs": 60, "lr": 1e-3})]:
            j.append({"tag": tag, "feats": f, "phys_init": pi, "kind": "nn", "hidden": 32, "drop": 0.3, "wd": 0.05, "epochs": 60, "lhm_aug": 0.3, **kw})
        return j

    def run(self):
        cl = Cluster(self.cfg)
        self.log.write(event="start", run="stage3", resources=self.cfg["ray_res"], criteria=CRITERIA["stage3"])
        t0 = time.time()
        status = "failed"
        try:
            futs = [cl.submit(nn_task if j.get("kind") == "nn" else xgb_task, j) for j in self.jobs()]
            res = [cl.ray.get(f) for f in futs]
            pickle.dump(res, open(self.out, "wb"))
            status = "ok"
        finally:
            self.log.write(event="stop", run="stage3", status=status, seconds=round(time.time() - t0))


class Analysis:
    def __init__(self, cfg=CONFIG):
        self.cfg = cfg
        self.D = Dataset(cfg).load()
        self.spec = Spec.load(FCFG["spec_path"])
        self.groups = self.spec.groups()
        res = pickle.load(open(os.path.join(cfg["out_dir"], "stage1.pkl"), "rb"))
        self.phys = res[0]
        self.xgb = {r["tag"]: r["res"] for r in res[1:]}
        for st in ("stage2", "stage3"):
            p2 = os.path.join(cfg["out_dir"], f"{st}.pkl")
            if os.path.exists(p2):
                self.xgb.update({r["tag"]: r["res"] for r in pickle.load(open(p2, "rb"))})
        self.m = Metrics(cfg)

    def preds(self, tg, block, models):
        D = self.D
        ix = D.sel(block, tg)
        out = {}
        for name in models:
            if name.startswith("phys"):
                key = tg.split("_")[0] + ("_fan" if name == "phys_fan" else "")
                if key not in self.phys:
                    continue
                out[name] = self.phys[key]["preds"][tg][block]
            else:
                out[name] = self.xgb[name][tg]["preds"][block]
        out = {k: v for k, v in out.items() if len(v) == 0 or np.isfinite(v).mean() > 0.5}
        ok = np.ones(len(ix), bool)
        for v in out.values():
            ok &= np.isfinite(v)
        return ix[ok], {k: v[ok] for k, v in out.items()}

    def compare(self, tg, block, models, extra_mask=None):
        D = self.D
        ix, p = self.preds(tg, block, models)
        if extra_mask is not None:
            keep = extra_mask(ix)
            ix, p = ix[keep], {k: v[keep] for k, v in p.items()}
        if len(ix) < 50:
            return None
        return self.m.compare(D.t[ix], D.Y[ix, D.tnames.index(tg)].astype(np.float64), p)

    def group_gain(self, tag, tg):
        g = {}
        for n, v in self.xgb[tag][tg]["gain"].items():
            g[self.groups[n]] = g.get(self.groups[n], 0.0) + v
        tot = sum(g.values()) or 1.0
        return {k: v / tot for k, v in sorted(g.items(), key=lambda kv: -kv[1])}

    def main_table(self):
        rows = {}
        for tg in self.D.tnames:
            for b in ("val", "test", "lhm_missing", "oor_pump20"):
                models = ["phys", "phys_fan", "small", "rich"]
                r = self.compare(tg, b, models)
                if r is not None:
                    rows[f"{tg}|{b}"] = r
        return rows

    def ablation(self, targets=("water_30", "water_120", "water_300", "water_900"), block="test", ref="rs_aug", prefix="dropS:"):
        out = {}
        for tg in targets:
            tags = [ref] + [t for t in self.xgb if t.startswith(prefix)]
            r = self.compare(tg, block, tags)
            for t in tags[1:]:
                k = r["skill_vs"][f"{ref}|{t}"]
                out[f"{tg}|{t}"] = k
        return out


if __name__ == "__main__":
    mode = sys.argv[1] if len(sys.argv) > 1 else "build"
    if mode == "build":
        Dataset().build()
    elif mode == "fresh":
        cl = Cluster(FRESH)
        print(cl.ray.get(cl.submit(build_task, FRESH, num_cpus=2, memory=6e9)))
    elif mode == "stage1":
        Stage1().run()
    elif mode == "stage3":
        Stage3().run()
    elif mode == "stage2":
        Stage2().run()
    elif mode == "report1":
        a = Analysis()
        json.dump({"main": a.main_table(), "ablation": a.ablation(), "ablation_val": a.ablation(block="val"),
                   "gain": {tg: a.group_gain("rich", tg) for tg in a.D.tnames}}, open(os.path.join(CONFIG["out_dir"], "stage1_report.json"), "w"), indent=1)
