import json
import os
import pickle
import sys
import time
import numpy as np
from models import CONFIG, Dataset, Metrics, RunLog, physics_task, xgb_task
from features import Spec, CONFIG as FCFG

CRITERIA = {
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
        futs = [cl.submit(physics_task, {})] + [cl.submit(xgb_task, j) for j in self.jobs()]
        res = [cl.ray.get(f) for f in futs]
        pickle.dump(res, open(self.out, "wb"))
        self.log.write(event="stop", run="stage1", seconds=round(time.time() - t0))


if __name__ == "__main__":
    mode = sys.argv[1] if len(sys.argv) > 1 else "build"
    if mode == "build":
        Dataset().build()
    elif mode == "stage1":
        Stage1().run()
