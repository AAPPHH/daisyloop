import json
import os
import pickle
import sys
import time
import numpy as np
from rawstore import RawStore
from models import CONFIG, FRESH, PINT, VARIANTS, LIN, LIN_SETS, LIN_TARGETS, Dataset, Metrics, RunLog, build_task, linear_task, physics_task, xgb_fit_task, xgb_task
from nets import nn_task
from features import PHYS, PhysSpec, Spec, CONFIG as FCFG

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
        try:
            ray.init(address="auto", logging_level=40, runtime_env={"env_vars": cfg["ray_env"]})
        except Exception:
            ray.init(address="local", num_cpus=cfg.get("local_cpus", 8), object_store_memory=int(1e9), include_dashboard=False, logging_level=40, runtime_env={"env_vars": cfg["ray_env"]})
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


LCRIT = {
    "H2": "water 2 min RMSE <= 1.05 x x44aug, water 30 s <= 1.10 x x44aug, CPU and GPU not worse than persistence; linear sets a44 / phys / rich with ridge, LinearSVR (epsilon 0.005 to 0.02 K, C tuned on val), Huber; test block and fresh blocks, idle and load apart",
    "H3": "higher rpm lowers water at 200 W, stopped fan 1.4 K/min at 200 W and 2.8 K/min at 400 W within factor 1.5, forecast differs across 0 to 735 rpm, small pump effect, 240 s rollout stable",
    "H4": "load steps, water above 40 C, GPU heavy, missing LHM: error not above persistence by more than 10 percent, no blown-up forecasts",
    "H5": "a horizon is usable to choose the fan only if the forecast difference between 0 rpm and full speed at 200 W exceeds that horizon's RMSE",
}


class LinearStage:
    def __init__(self, cfg=FRESH):
        self.cfg = cfg
        self.D = Dataset(cfg).load()
        self.dir = os.path.dirname(cfg["cache"])
        self.log = RunLog(cfg)
        dep = json.load(open(os.path.join(CONFIG["out_dir"], "model_deploy.json")))
        self.a44 = dep["features"]
        ps = PhysSpec(Spec.load(FCFG["spec_path"]))
        ps.build()
        self.phys = [n for n in dict.fromkeys(ps.names()) if n in self.D.col]
        added = {n.name for n in ps.new}
        self.rich = [n for n in self.D.names if n not in added]
        self.sets = {"a44": self.a44, "phys": self.phys, "rich": self.rich}
        self.shallow = {"max_depth": 3, "min_child_weight": 100, "eta": 0.03}

    def old_jobs(self):
        j = [{"tag": f"{fam}|{st}|{'nolhm' if dl else 'lhm'}", "family": fam, "feats": self.sets[st], "targets": LIN_TARGETS, "fit": ["train"], "val": ["val"], "drop_lhm": dl, "cfg": self.cfg}
             for st in LIN_SETS for fam in LIN for dl in (False, True)]
        return j

    def fresh_jobs(self, old):
        j = []
        for r in old:
            if r["tag"].endswith("|lhm") or r["tag"].endswith("|nolhm"):
                fam, st, dl = r["tag"].split("|")
                j.append({"tag": r["tag"], "family": fam, "feats": self.sets[st], "targets": LIN_TARGETS, "fit": FIT_FRESH, "drop_lhm": dl == "nolhm", "cfg": self.cfg,
                          "params": {t: r["res"]["targets"][t]["params"] for t in LIN_TARGETS}})
        return j

    def run(self, which):
        cl = Cluster(self.cfg)
        self.log.write(event="start", run=f"linear_{which}", resources={"cpus": 8, "memory_gb": 12}, criteria=LCRIT)
        t0 = time.time()
        status = "failed"
        try:
            pth = lambda n: os.path.join(self.dir, n)
            if which == "old":
                futs = [cl.submit(linear_task, j, num_cpus=2, memory=2e9) for j in self.old_jobs()]
                futs.append(cl.submit(xgb_task, {"tag": "lx44", "feats": self.a44, "targets": LIN_TARGETS, "lhm_aug": 0.3, "params": self.shallow, "save": True, "cfg": self.cfg}, num_cpus=2, memory=2e9))
                done, pend = [], list(futs)
                while pend:
                    rd, pend = cl.ray.wait(pend, num_returns=1)
                    done += cl.ray.get(rd)
                    pickle.dump(done, open(pth("lin_old.part"), "wb"))
                pickle.dump(done, open(pth("lin_clip_old.pkl"), "wb"))
            else:
                old = pickle.load(open(pth("lin_clip_old.pkl"), "rb"))
                futs = [cl.submit(linear_task, j, num_cpus=2, memory=2e9) for j in self.fresh_jobs(old)]
                rounds = {r_t: [r for r in old if r["tag"] == "lx44"][0]["res"][r_t]["best_iter"] + 1 for r_t in LIN_TARGETS}
                futs.append(cl.submit(xgb_fit_task, {"tag": "lx44f", "cfg": self.cfg, "feats": self.a44, "targets": LIN_TARGETS, "fit": FIT_FRESH, "lhm_aug": 0.3, "params": self.shallow, "rounds": rounds}, num_cpus=2, memory=2e9))
                pickle.dump([cl.ray.get(f) for f in futs], open(pth("lin_clip_fresh.pkl"), "wb"))
            status = "ok"
        finally:
            self.log.write(event="stop", run=f"linear_{which}", status=status, seconds=round(time.time() - t0))


CF = {
    "levels": {"idle": [30.0, 100.0], "w200": [170.0, 240.0], "w300": [280.0, 400.0]},
    "blocks": ["test", "fresh0", "fresh1", "fresh2"],
    "per_level": 36,
    "spacing_s": 300,
    "window_s": 4000,
    "fan_rpm": [0.0, 300.0, 441.0, 735.0, 1100.0, 1465.0],
    "pump_rpm": [1835.0, 2570.0, 4800.0],
    "rollouts": [120, 240],
    "fill_s": 10,
    "targets": ["water_30", "water_60", "water_90", "water_120"],
    "ref_k_per_min_per_100w": 0.70,
    "models": {
        "svr_phys_old": "svr|phys|lhm", "svr_phys_new": "svr|phys@new", "huber_phys_new": "huber+|phys@new", "svr_pint_new": "svr+|pint@new", "huber_pint_new": "huber+|pint@new",
        "huber_nofan_G05": "huber|nofan+G0.5@new", "huber_nofan_G1": "huber|nofan+G1.0@new", "huber_nofan_G2": "huber|nofan+G2.0@new",
        "huber_m07": "huber+|phys~m07@new", "huber_m25": "huber+|phys~m25@new", "huber_gn20": "huber+|phys~gn20@new",
    },
    "g_full": 23.8, "m": 1.61, "ta": 25.0, "c_j_per_k": 11700.0,
}


OFFSET = {"feat": "ph.fx", "k": 23.8 / 11700.0, "pump_rpm": 3500.0, "dead_hi": 11.0, "dead_lo": 33.0, "scale": 1.0}


def lin_apply(r, x, clip=None):
    z = np.where(np.isfinite(x), (x - r["mean"]) / r["std"], 0.0)
    z = np.clip(z, -clip, clip) if clip else z
    return float(z @ r["w"] + r["b"])


def cf_task(job):
    from features import BatchEngine
    cfg, cf = job["cfg"], job["cf"]
    D = Dataset(cfg).load()
    meta, X = RawStore({"cache_dir": cfg["store_dir"]}).load()
    ix = {c["channel"]: i for i, c in enumerate(meta["channels"])}
    need = set(job["a44"]) | set(job["phys"]) | {"ph.q"}
    rich = Spec.load(FCFG["spec_path"])
    specs = {}
    for m in job["lin"].values():
        key = json.dumps(m.get("phys_cfg") or {}, sort_keys=True)
        if key not in specs:
            specs[key] = PhysSpec(rich, dict(PHYS, **json.loads(key))).build().subset(need)
    col = {n: i for i, n in enumerate(D.names)}
    lo, hi = cf["levels"][job["level"]]
    pw = D.F[:, col["p_total.ewm30"]]
    feats_ok = np.isfinite(D.F[:, [col[n] for n in job["a44"] + job["phys"]]]).all(1)
    rows = []
    for b in cf["blocks"]:
        sel = np.where((D.block == D.bnames.index(b)) & feats_ok & (pw >= lo) & (pw <= hi))[0]
        last = -10 ** 9
        for r in sel:
            t = int(D.t[r])
            if t - last >= cf["spacing_s"] and t >= cf["window_s"]:
                rows.append(t)
                last = t
    rng = np.random.default_rng(0)
    rows = sorted(rng.choice(rows, min(cf["per_level"], len(rows)), replace=False).tolist())
    fan_ch = {k: ix[f"d5next/fan/{k}"] for k in ("duty", "rpm", "power", "current")}
    pump_ch = {k: ix[f"d5next/pump/{k}"] for k in ("duty", "rpm", "power", "current")}
    maps = {}
    for nm, chs in (("fan", fan_ch), ("pump", pump_ch)):
        rp, du = np.asarray(X[:, chs["rpm"]], dtype=np.float64), np.asarray(X[:, chs["duty"]], dtype=np.float64)
        ok = np.isfinite(rp) & np.isfinite(du)
        edges = np.arange(0, 105, 5.0)
        xs, ys = [], []
        for a in edges:
            m = ok & (du >= a) & (du < a + 5)
            if m.sum() > 30:
                xs.append(float(np.median(rp[m]))), ys.append(float(np.median(du[m])))
        o = np.argsort(xs)
        maps[nm] = (np.array(xs)[o], np.array(ys)[o])
    dep = job["xgb_paths"]
    import xgboost as xgb
    boosters = {tg: xgb.Booster() for tg in cf["targets"]}
    for tg, b in boosters.items():
        b.load_model(dep.format(tg=tg))
    out = {"rows": rows, "base": [], "pred": {}}
    scen = [("fan", r, h) for h in cf["rollouts"] for r in cf["fan_rpm"]] + [("pump", r, 120) for r in cf["pump_rpm"]]
    for g in rows:
        w0 = g - cf["window_s"] + 1
        base = np.asarray(X[w0: g + 1], dtype=np.float64)
        filled = base[-1].copy()
        for j in range(base.shape[1]):
            v = base[-cf["fill_s"]:, j]
            v = v[np.isfinite(v)]
            filled[j] = v[-1] if len(v) else np.nan
        op = {nm: {k: filled[ch[k]] for k in ch} for nm, ch in (("fan", fan_ch), ("pump", pump_ch))}
        out["base"].append({"fan_rpm": op["fan"]["rpm"], "pump_rpm": op["pump"]["rpm"], "t": g})
        for key in scen:
            axis, rpm, ro = key
            row = filled.copy()
            chs = fan_ch if axis == "fan" else pump_ch
            sc = 1.0 if op[axis]["rpm"] < 100 else min(max(rpm / op[axis]["rpm"], 0.0), 20.0)
            row[chs["rpm"]] = rpm
            row[chs["duty"]] = float(np.interp(rpm, *maps[axis]))
            row[chs["power"]] = op[axis]["power"] * sc ** 3
            row[chs["current"]] = op[axis]["current"] * sc ** 2
            Xs = np.vstack([base, np.tile(row, (ro, 1))]).astype(np.float32)
            fs = {}
            for key, sp in specs.items():
                v = BatchEngine(sp).run(Xs, ix, meta["k0"] + w0)
                fs[key] = {n: float(v[n][-1]) for n in need}
            pump_now = rpm if axis == "pump" else op["pump"]["rpm"]
            pl = 1.0 if pump_now < 3500.0 else 0.0
            rec = {"ph.q": fs[json.dumps({}, sort_keys=True)]["ph.q"] if json.dumps({}, sort_keys=True) in fs else 0.0}
            for name, m in job["lin"].items():
                f = dict(fs[json.dumps(m.get("phys_cfg") or {}, sort_keys=True)])
                f["ph.pl"] = pl
                for n in m["feats"]:
                    if n.endswith("*pl"):
                        f[n] = f[n[:-3]] * pl
                x = np.array([f[n] for n in m["feats"]])
                for tg in cf["targets"]:
                    v0 = lin_apply(m["targets"][tg], x, m.get("clip"))
                    o = m.get("offset")
                    if o:
                        td = o["dead_hi"] if pump_now > o["pump_rpm"] else o["dead_lo"]
                        v0 -= o["scale"] * o["k"] * max(0.0, int(tg.split("_")[1]) - td) * f[o["feat"]]
                    rec[f"{name}|{tg}"] = v0
            f0 = fs[json.dumps({}, sort_keys=True)]
            xa = np.array([[f0[n] for n in job["a44"]]], dtype=np.float32)
            for tg in cf["targets"]:
                rec[f"xgb|{tg}"] = float(boosters[tg].predict(xgb.DMatrix(xa))[0])
            out["pred"].setdefault(f"{axis}|{int(rpm)}|{ro}", []).append(rec)
    return {"level": job["level"], "res": out}


EVALS = {"test": ["test"], "fresh_old": ["fresh0", "fresh1", "fresh2"], "fresh12": ["fresh1", "fresh2"], "lhm_missing": ["lhm_missing"], "oor_pump20": ["oor_pump20"]}
LOAD_W = 150.0


class LinReport:
    def __init__(self, cfg=FRESH):
        self.cfg = cfg
        self.D = Dataset(cfg).load()
        d = os.path.dirname(cfg["cache"])
        self.old = {r["tag"]: r["res"] for r in pickle.load(open(os.path.join(d, "lin_clip_old.pkl"), "rb"))}
        p = os.path.join(d, "lin_clip_fresh.pkl")
        self.fresh = {r["tag"]: r["res"] for r in pickle.load(open(p, "rb"))} if os.path.exists(p) else {}
        self.m = Metrics(cfg)

    def get(self, src, tag, tg, blocks):
        r = src[tag]
        r = r["targets"][tg] if "targets" in r else r[tg]
        return np.concatenate([r["preds"][b] for b in blocks])

    def idx(self, tg, blocks):
        return np.concatenate([self.D.sel(b, tg) for b in blocks])

    def cmp(self, src, tags, tg, blocks, sub="all"):
        D = self.D
        ix = self.idx(tg, blocks)
        P = {t: self.get(src, t, tg, blocks) for t in tags}
        keep = np.ones(len(ix), bool)
        if sub != "all":
            p = D.F[ix, D.col["p_total.ewm5"]]
            keep = (p > LOAD_W) if sub == "load" else (p <= LOAD_W)
        for v in P.values():
            keep &= np.isfinite(v)
        ix = ix[keep]
        if len(ix) < 100:
            return None
        return self.m.compare(D.t[ix], D.Y[ix, D.tnames.index(tg)].astype(np.float64), {k: v[keep] for k, v in P.items()})

    def lin_tags(self, nolhm=False):
        return [t for t in self.old if "|" in t and t.endswith("|nolhm" if nolhm else "|lhm")]

    def rmse_table(self, src, tags, blocks, sub="all", targets=LIN_TARGETS):
        rows = {}
        for tg in targets:
            r = self.cmp(src, tags, tg, blocks, sub)
            if r is None:
                continue
            for k, v in r["rmse"].items():
                rows.setdefault(k, {})[tg] = v
            for k, v in r["skill_vs"].items():
                if k.endswith("|lx44"):
                    rows.setdefault(k.split("|")[0], {})[tg + "|vs_xgb"] = v
            rows.setdefault("n", {})[tg] = r["n"]
        return rows

    def verdict(self, tab, tag):
        r = tab[tag]
        x, p = tab["lx44"], tab["persist"]
        c = {"water_120": r["water_120"] <= 1.05 * x["water_120"], "water_30": r["water_30"] <= 1.10 * x["water_30"],
             "cpu_30": r["cpu_30"] <= p["cpu_30"], "cpu_120": r["cpu_120"] <= p["cpu_120"], "gpu_30": r["gpu_30"] <= p["gpu_30"], "gpu_120": r["gpu_120"] <= p["gpu_120"]}
        return c

    def h2(self):
        out = {}
        tags = self.lin_tags() + self.lin_tags(True) + ["lx44"]
        for name, bl in EVALS.items():
            out[name] = {"rmse": self.rmse_table(self.old, tags, bl)}
            if name in ("test", "fresh_old"):
                for sub in ("load", "idle"):
                    out[name][sub] = self.rmse_table(self.old, tags, bl, sub)
            tab = out[name]["rmse"]
            if all(t in tab.get("lx44", {}) for t in LIN_TARGETS):
                out[name]["verdict"] = {t: self.verdict(tab, t) for t in self.lin_tags()}
        if self.fresh:
            tf = [t for t in self.fresh if t != "lx44f"] + ["lx44f"]
            src = {k: v for k, v in self.fresh.items()}
            for name, bl in (("fresh12_trained_through_fresh0", ["fresh1", "fresh2"]),):
                tab = self.rmse_table(src, tf, bl, "all")
                out[name] = {"rmse": tab}
                for sub in ("load", "idle"):
                    out[name][sub] = self.rmse_table(src, tf, bl, sub)
                out[name]["verdict"] = {t: self.verdict_f(tab, t) for t in tf if "|" in t}
        return out

    @staticmethod
    def verdict_f(tab, tag):
        r, x, p = tab[tag], tab["lx44f"], tab["persist"]
        return {"water_120": r["water_120"] <= 1.05 * x["water_120"], "water_30": r["water_30"] <= 1.10 * x["water_30"],
                "cpu_30": r["cpu_30"] <= p["cpu_30"], "cpu_120": r["cpu_120"] <= p["cpu_120"], "gpu_30": r["gpu_30"] <= p["gpu_30"], "gpu_120": r["gpu_120"] <= p["gpu_120"]}

    def h5(self, models, src=None, ref="lx44"):
        src = src or self.old
        out = {}
        for name, bl in (("test", ["test"]), ("fresh_old", ["fresh0", "fresh1", "fresh2"]), ("fresh12", ["fresh1", "fresh2"])):
            for sub in ("load", "idle"):
                for tg in ("water_30", "water_60", "water_90", "water_120"):
                    r = self.cmp(src, models, tg, bl, sub)
                    if r:
                        out[f"{name}|{sub}|{tg}"] = {"n": r["n"], "persist": r["rmse"]["persist"], **{m: {"rmse": r["rmse"][m], "skill": r["skill_vs_persist"][m], "rmse_ci": r["rmse_ci"][m]} for m in models}}
        return out

    def h4(self, models, src=None, blocks=("val", "test", "oor_pump20", "fresh0", "fresh1", "fresh2"), nolhm=None):
        src = src or self.old
        D = self.D
        col = D.col
        conds = {
            "load_step": lambda i: np.abs(D.F[i, col["p_total.ewm5.d30"]]) > 100.0,
            "water_gt_38": lambda i: D.F[i, col["water"]] > 38.0,
            "water_gt_40": lambda i: D.F[i, col["water"]] > 40.0,
            "gpu_heavy": lambda i: D.F[i, col["gpu_w.ewm5"]] > 150.0,
            "cpu_heavy": lambda i: D.F[i, col["cpu_pkg_w.ewm5"]] > 150.0,
            "water_hot_load": lambda i: (D.F[i, col["water"]] > 36.0) & (D.F[i, col["p_total.ewm5"]] > 200.0),
        }
        out = {}
        for tg in ("water_30", "water_120", "gpu_120", "cpu_120"):
            for cname, cf in conds.items():
                ix = self.idx(tg, list(blocks))
                P = {m: self.get(src, m, tg, list(blocks)) for m in models}
                keep = cf(ix) & np.all([np.isfinite(v) for v in P.values()], axis=0)
                if keep.sum() < 30:
                    out[f"{tg}|{cname}"] = {"n": int(keep.sum())}
                    continue
                y = D.Y[ix[keep], D.tnames.index(tg)].astype(np.float64)
                rec = {"n": int(keep.sum()), "persist": float(np.sqrt(np.mean(y ** 2))), "max_abs_y": float(np.abs(y).max())}
                for m, v in P.items():
                    e = v[keep] - y
                    rec[m] = {"rmse": float(np.sqrt(np.mean(e ** 2))), "max_abs_pred": float(np.abs(v[keep]).max()), "max_abs_err": float(np.abs(e).max()), "bias": float(e.mean())}
                out[f"{tg}|{cname}"] = rec
        if nolhm:
            for tg in ("water_30", "water_120"):
                P = {m: self.get(src, m, tg, ["lhm_missing"]) for m in nolhm}
                y = D.Y[self.idx(tg, ["lhm_missing"]), D.tnames.index(tg)].astype(np.float64)
                ok = np.all([np.isfinite(v) for v in P.values()], axis=0)
                out[f"{tg}|lhm_missing"] = {"n": int(ok.sum()), "persist": float(np.sqrt(np.mean(y[ok] ** 2))), **{m: {"rmse": float(np.sqrt(np.mean((v[ok] - y[ok]) ** 2))), "max_abs_pred": float(np.abs(v[ok]).max())} for m, v in P.items()}}
        return out

    def best_by_val(self, tg="water_120"):
        out = {}
        for t in self.lin_tags():
            fam, st, _ = t.split("|")
            r = self.old[t]["targets"][tg]["params"], min(g[0] for g in self.old[t]["targets"][tg]["grid"])
            if st not in out or r[1] < out[st][1]:
                out[st] = (t, r[1])
        return {k: v[0] for k, v in out.items()}


class V2Stage(LinearStage):
    NOFAN = ("ph.fx", "ph.fx.e30", "ph.fx.e120", "ph.fxl", "ph.fxd", "ph.px", "ph.gf.l22", "ph.gf.d30")

    def jobs(self):
        fresh_fit, fresh_val = ["train", "val", "test", "oor_pump20"], ["fresh0"]
        nofan = [n for n in self.phys if n not in self.NOFAN]
        pint = self.phys + ["ph.pl"] + [f"{n}*pl" for n in PINT]
        resid = {"feats": self.a44 + [n for n in self.phys if n not in self.a44], "params": dict(self.shallow, max_depth=2, eta=0.05, min_child_weight=100), "rounds": 60}
        spec = []
        for fam in ("ridge", "svr", "huber"):
            spec.append((f"{fam}|phys", fam, self.cfg, self.phys, {}))
            spec.append((f"{fam}|pint", fam, VARIANTS["pint"], pint, {}))
        spec.append(("huber|phys+resid", "huber", self.cfg, self.phys, {"resid": resid}))
        spec.append(("svr|phys+resid", "svr", self.cfg, self.phys, {"resid": resid}))
        for sc in (0.5, 1.0, 2.0):
            for fam in (("svr", "huber") if sc == 1.0 else ("huber",)):
                spec.append((f"{fam}|nofan+G{sc}", fam, self.cfg, nofan, {"offset": dict(OFFSET, scale=sc)}))
        j = []
        for tag, fam, cfg, feats, kw in spec:
            j.append({"tag": tag + "@old", "family": fam, "feats": feats, "targets": LIN_TARGETS, "fit": ["train"], "val": ["val"], "cfg": cfg, **kw})
            j.append({"tag": tag + "@new", "family": fam, "feats": feats, "targets": LIN_TARGETS, "fit": fresh_fit, "val": fresh_val, "refit_val": True, "cfg": cfg, **kw})
        return j

    def run(self, which="v2"):
        cl = Cluster(self.cfg)
        self.log.write(event="start", run="linear_v2", resources={"cpus": 8, "memory_gb": 12}, criteria=LCRIT)
        done, pend = [], [cl.submit(linear_task, j, num_cpus=2, memory=2e9) for j in self.jobs()]
        while pend:
            rd, pend = cl.ray.wait(pend, num_returns=1)
            done += cl.ray.get(rd)
            pickle.dump(done, open(os.path.join(self.dir, "lin_v2.pkl"), "wb"))
        self.log.write(event="stop", run="linear_v2", status="ok")


class V3Stage(V2Stage):
    def jobs(self):
        fresh_fit, fresh_val = ["train", "val", "test", "oor_pump20"], ["fresh0"]
        pint = self.phys + ["ph.pl"] + [f"{n}*pl" for n in PINT]
        wt = {"feat": "ph.gf.d30", "above": 0.01, "w": 5.0}
        j = []
        for fam in ("ridge+", "svr+", "huber+"):
            for st, cfg, feats in (("phys", self.cfg, self.phys), ("pint", VARIANTS["pint"], pint)):
                for wn, kw in (("", {}), ("w5", {"weight": wt})):
                    tag = f"{fam}|{st}{wn}"
                    j.append({"tag": tag + "@old", "family": fam, "feats": feats, "targets": LIN_TARGETS, "fit": ["train"], "val": ["val"], "cfg": cfg, **kw})
                    j.append({"tag": tag + "@new", "family": fam, "feats": feats, "targets": LIN_TARGETS, "fit": fresh_fit, "val": fresh_val, "refit_val": True, "cfg": cfg, **kw})
        return j

    def run(self, which="v3"):
        cl = Cluster(self.cfg)
        done, pend = [], [cl.submit(linear_task, j, num_cpus=2, memory=2e9) for j in self.jobs()]
        while pend:
            rd, pend = cl.ray.wait(pend, num_returns=1)
            done += cl.ray.get(rd)
            pickle.dump(done, open(os.path.join(self.dir, "lin_v3.pkl"), "wb"))


class V4Stage(V2Stage):
    def jobs(self):
        fresh_fit, fresh_val = ["train", "val", "test", "oor_pump20"], ["fresh0"]
        j = []
        for v in ("m07", "m25", "gn20"):
            for fam in ("huber+", "svr+"):
                j.append((f"{fam}|phys~{v}", fam, VARIANTS[v], self.phys))
        for fam in ("huber+", "svr+"):
            j.append((f"{fam}|ff", fam, VARIANTS["ff"], self.phys + ["ph.fxf15", "ph.fxf30"]))
        out = []
        for tag, fam, cfg, feats in j:
            out.append({"tag": tag + "@old", "family": fam, "feats": feats, "targets": LIN_TARGETS, "fit": ["train"], "val": ["val"], "cfg": cfg})
            out.append({"tag": tag + "@new", "family": fam, "feats": feats, "targets": LIN_TARGETS, "fit": fresh_fit, "val": fresh_val, "refit_val": True, "cfg": cfg})
        return out

    def run(self, which="v4"):
        cl = Cluster(self.cfg)
        done, pend = [], [cl.submit(linear_task, j, num_cpus=2, memory=2e9) for j in self.jobs()]
        while pend:
            rd, pend = cl.ray.wait(pend, num_returns=1)
            done += cl.ray.get(rd)
            pickle.dump(done, open(os.path.join(self.dir, "lin_v4.pkl"), "wb"))


class V5Stage(V2Stage):
    def jobs(self):
        fresh_fit, fresh_val = ["train", "val", "test", "oor_pump20"], ["fresh0"]
        pint = self.phys + ["ph.pl"] + [f"{n}*pl" for n in PINT]
        out = []
        for fam in ("svr+", "huber+"):
            tag = f"{fam}|pint~nolhm"
            out.append({"tag": tag + "@old", "family": fam, "feats": pint, "targets": LIN_TARGETS, "fit": ["train"], "val": ["val"], "cfg": VARIANTS["pint"], "drop_lhm": True})
            out.append({"tag": tag + "@new", "family": fam, "feats": pint, "targets": LIN_TARGETS, "fit": fresh_fit, "val": fresh_val, "refit_val": True, "cfg": VARIANTS["pint"], "drop_lhm": True})
        return out

    def run(self, which="v5"):
        cl = Cluster(self.cfg)
        done, pend = [], [cl.submit(linear_task, j, num_cpus=2, memory=2e9) for j in self.jobs()]
        while pend:
            rd, pend = cl.ray.wait(pend, num_returns=1)
            done += cl.ray.get(rd)
            pickle.dump(done, open(os.path.join(self.dir, "lin_v5.pkl"), "wb"))


class CfStage(LinearStage):
    CAND = {"xgb_note": None}

    def run(self, which="cf"):
        cl = Cluster(self.cfg)
        d = self.dir
        pool = {}
        for f in ("lin_clip_old.pkl", "lin_v2.pkl", "lin_v3.pkl", "lin_v4.pkl", "lin_v5.pkl"):
            if os.path.exists(os.path.join(d, f)):
                pool.update({r["tag"]: r["res"] for r in pickle.load(open(os.path.join(d, f), "rb"))})
        picks = {k: v for k, v in CF["models"].items() if v in pool}
        lin = {}
        for k, t in picks.items():
            r = pool[t]
            v = VARIANTS_BY_TAG.get(t.split("|")[1].split("@")[0].split("~")[-1] if "~" in t else "", {})
            lin[k] = {"feats": r["feats"], "clip": r["clip"], "offset": r.get("offset"), "phys_cfg": v.get("phys_cfg"), "targets": {tg: {x: r["targets"][tg][x] for x in ("mean", "std", "w", "b")} for tg in CF["targets"]}}
        jobs = [{"cfg": self.cfg, "cf": CF, "level": lv, "a44": self.a44, "phys": self.phys, "lin": lin, "xgb_paths": os.path.join(CONFIG["out_dir"], "lx44_{tg}.json")} for lv in CF["levels"]]
        futs = [cl.submit(cf_task, j, num_cpus=2, memory=2.5e9) for j in jobs]
        pickle.dump({"models": picks, "res": [cl.ray.get(f) for f in futs]}, open(os.path.join(d, "cf.pkl"), "wb"))


VARIANTS_BY_TAG = {k: v for k, v in VARIANTS.items()}


FIT_FRESH = ["train", "val", "test", "oor_pump20", "fresh0"]


if __name__ == "__main__":
    mode = sys.argv[1] if len(sys.argv) > 1 else "build"
    if mode == "build":
        Dataset().build()
    elif mode == "fresh":
        cl = Cluster(FRESH)
        print(cl.ray.get(cl.submit(build_task, FRESH, num_cpus=2, memory=6e9)))
    elif mode == "variants":
        cl = Cluster(FRESH)
        for v in VARIANTS.values():
            print(cl.ray.get(cl.submit(build_task, v, num_cpus=2, memory=3e9)))
    elif mode == "v5":
        V5Stage().run()
    elif mode == "v4":
        V4Stage().run()
    elif mode == "v3":
        V3Stage().run()
    elif mode == "v2":
        V2Stage().run()
    elif mode == "cf":
        CfStage().run()
    elif mode in ("lin_old", "lin_fresh"):
        LinearStage().run(mode.split("_")[1])
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
