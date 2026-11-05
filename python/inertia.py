import json
import os
import sys
import numpy as np
import pyarrow as pa
import pyarrow.parquet as pq

CONFIG = {
    "work": os.environ.get("INERTIA_DIR", "inertia"),
    "tz_offset": 3600,
    "channels": {
        "tw": "d5next/water/temp",
        "fan_rpm": "d5next/fan/rpm",
        "fan_duty": "d5next/fan/duty",
        "fan_w": "d5next/fan/power",
        "pump_rpm": "d5next/pump/rpm",
        "pump_duty": "d5next/pump/duty",
        "pump_w": "d5next/pump/power",
        "cpu_w": "/intelcpu/0/power/0",
        "cpu_max": "/intelcpu/0/temperature/0",
        "cpu_avg": "/intelcpu/0/temperature/1",
        "cpu_load": "/intelcpu/0/load/0",
        "gpu_w": "nvml/0/power",
        "gpu_t": "nvml/0/temp",
        "gpu_core": "/gpu-nvidia/0/temperature/0",
        "gpu_hot": "/gpu-nvidia/0/temperature/2",
        "gpu_mem": "/gpu-nvidia/0/temperature/3",
        "tw_lhm": "/usbhid/0C70/F00E/00000-00000/temperature/0",
        "sys_t": "/lpc/nct6687dr/0/temperature/1",
    },
    "extract": {
        "names": ["d5next/water/temp", "d5next/fan/rpm", "d5next/fan/duty", "d5next/fan/power", "d5next/pump/rpm", "d5next/pump/duty", "d5next/pump/power", "nvml/0/power", "nvml/0/temp", "nvml/0/util_gpu"],
        "like": ["/intelcpu/0/power/%", "/intelcpu/0/temperature/%", "/intelcpu/0/load/0", "/gpu-nvidia/0/temperature/%", "/gpu-nvidia/0/power/0", "/lpc/nct6687dr/0/temperature/%", "/memory/dimm/%/temperature/%", "/nvme/%/temperature/%", "/ssd/0/temperature/0", "/usbhid/%/temperature/%"],
    },
    "grid": 30,
    "horizons": [300, 900, 1800],
    "window_steps": 60,
    "lead_steps": 8,
    "window_stride": 5,
    "gap_max": 3,
    "fan_full_rpm": 1460.0,
    "pump_full_rpm": 4800.0,
    "params": {
        "names": ["C", "eta_cpu", "eta_gpu", "Gn", "A", "m", "gamma", "Ta", "P0"],
        "x0": [12000.0, 0.9, 0.9, 1.0, 26.0, 1.0, 0.0, 25.0, 0.0],
        "lo": [3000.0, 0.5, 0.5, 0.0, 3.0, 0.3, -0.6, 16.0, -60.0],
        "hi": [40000.0, 1.0, 1.0, 15.0, 120.0, 2.5, 2.0, 29.0, 120.0],
    },
    "plant": {
        "tick_s": 30,
        "up_rpm": 700.0,
        "down_rpm": 150.0,
        "min_run_duty": 20.0,
        "stop_c": 33.0,
        "restart_c": 35.0,
        "limit_plan_c": 46.0,
        "limit_takeover_c": 48.0,
        "limit_top_c": 50.0,
        "pump_floor_q": 0.5,
        "gs_lo": 0.5,
        "gs_hi": 5.0,
        "horizon_s": 240,
        "heat_w": [100.0, 200.0, 300.0, 400.0],
        "spool_s": 4.5,
    },
    "steptest": {
        "reps": 20,
        "delay_s": 33,
        "modes": {"lump_ignore": {"truth": "lump", "fit_delay": 0}, "lump_fit": {"truth": "lump", "fit_delay": 33}, "two_fit": {"truth": "two", "fit_delay": 33}, "two_fit_off10": {"truth": "two", "fit_delay": 23}},
        "pick_tol": 0.03,
        "q_w": 285.0,
        "q_bias": 1.05,
        "t0": 34.0,
        "pump_floor": 0.5,
        "lead_s": 300,
        "fan_levels": [0.75, 0.55, 0.4, 0.27],
        "low_frac": 0.5,
        "pump_low": 0.5,
        "holds": [120, 180, 240, 360, 480],
        "noise": {"q_sd": 8.0, "t_sd": 0.005},
    },
    "lm": {"iters": 60, "lam": 1.0, "tol": 1e-7},
    "bootstrap": {"draws": 120, "block_h": 3.0, "seed": 7},
    "ext": {
        "lump2": {"x0": [7000.0, 4700.0, 50.0], "lo": [1000.0, 500.0, 5.0], "hi": [20000.0, 40000.0, 600.0], "substeps": 6, "delays": [0, 5, 10], "pump20_extra_s": 20, "subsets": ["all", "p100", "p20"], "p100_min": 0.7},
        "events": {"min_dq": 70.0, "calm": 0.35, "full_rpm": 1400.0, "windows": [[30, 60], [60, 120], [120, 240], [240, 480]], "lags": [5, 10, 15, 20, 30, 45, 60, 90, 120, 180, 240], "pre": [125, 5], "p100_rpm": 4000.0, "p20_rpm": [2300.0, 2900.0]},
        "frozen": {"horizons": [240, 300, 480, 900, 1800]},
        "restart": {"start_c": 33.0, "q_w": [200.0, 300.0, 400.0, 500.0, 600.0], "variants": {"nominal": [1.0, 1.0], "c_half": [0.5, 1.0], "dead_x2": [1.0, 2.0]}, "takeover_ramp_s": 10.0, "phases": [0, 10, 20], "horizon_s": 1500, "daemon_q_w": [200.0, 300.0, 400.0, 600.0], "daemon_horizon_s": 4000, "rule": [[35.0, "start"], [38.0, 800.0], [42.0, "full"]], "gs": [0.5, 5.0], "rates_q_w": [100.0, 200.0, 300.0, 400.0, 600.0], "rate_secs": 240},
        "sinks": {"sink_c": [25.0, 30.0, 32.0], "steady_q_w": 500.0, "peak_cpu_w": 316.0, "peak_gpu_rated_w": 320.0},
        "pump": {"events_local": ["2026-11-04 07:36:00", "2026-11-04 15:07:00", "2026-11-04 15:35:00", "2026-11-04 16:55:00", "2026-11-04 20:59:00"], "search_s": 240, "min_change_rpm": 500.0, "tw_bins": [[30, 33], [33, 35], [35, 35.5], [35.5, 36], [36, 37], [37, 40]]},
        "lowrpm": {"bins": [[250, 500], [500, 650], [650, 800], [800, 1000], [1000, 1150], [1150, 1250], [1250, 1350], [1350, 1400]], "c_j_per_k": [11700.0, 8000.0], "rate_local": ["2026-11-05 04:28:30", "2026-11-05 03:58:00", "2026-11-05 05:10:00", "2026-11-05 05:22:30"], "rate_secs": [100, 100, 200, 80]},
        "ema": {"taus_s": [30, 60, 120], "limit_c": 105.0},
    "spool": {"min_step_rpm": 60.0, "duty_jump": 4.0},
        "spikes": {"durations": [5, 30, 120, 600], "powers": [100.0, 200.0], "delay_s": 10},
    },
    "ray": {"cpus": 6},
    "stages": ["water", "dynamics", "settings", "tables", "ext", "steptest", "physical", "bootstrap", "report"],
}


class Store:
    def __init__(self, config=CONFIG):
        self.cfg = config
        self.path = os.path.join(config["work"], "wide.parquet")

    def extract(self):
        from lake import Lake
        e = self.cfg["extract"]
        cond = " or ".join(["channel=?"] * len(e["names"]) + ["channel like ?"] * len(e["like"]))
        q = f"select cast(floor(epoch(ts)) as bigint) k, channel c, avg(value) v from telemetry where {cond} group by 1,2"
        d = Lake().execute(q, e["names"] + e["like"]).fetchnumpy()
        k0, k1 = int(d["k"].min()), int(d["k"].max())
        cs = sorted(set(d["c"]))
        ci = {c: i for i, c in enumerate(cs)}
        m = np.full((k1 - k0 + 1, len(cs)), np.nan)
        m[d["k"] - k0, [ci[c] for c in d["c"]]] = d["v"]
        os.makedirs(self.cfg["work"], exist_ok=True)
        pq.write_table(pa.table({"k": np.arange(k0, k1 + 1), **{c: m[:, i] for i, c in enumerate(cs)}}), self.path)

    def load(self):
        if not os.path.exists(self.path):
            self.extract()
        have = set(pq.read_schema(self.path).names)
        t = pq.read_table(self.path, columns=["k"] + [c for c in self.cfg["channels"].values() if c in have])
        self.k = t["k"].to_numpy()
        self.v = {n: self.fill(t[c].to_numpy()) for n, c in self.cfg["channels"].items() if c in have}
        self.h = (self.k - self.k[0]) / 3600.0
        self.day = ((self.k + self.cfg["tz_offset"]) // 86400).astype(int)
        return self

    def fill(self, x):
        ok = np.isfinite(x)
        idx = np.where(ok)[0]
        if len(idx) < 2:
            return x
        out = x.copy()
        gap = np.where((np.diff(idx) > 1) & (np.diff(idx) <= self.cfg["gap_max"] + 1))[0]
        for g in gap:
            a, b = idx[g], idx[g + 1]
            out[a + 1:b] = np.interp(np.arange(a + 1, b), [a, b], [x[a], x[b]])
        return out

    def segments(self, names, min_len=600):
        ok = np.ones(len(self.k), bool)
        for n in names:
            ok &= np.isfinite(self.v[n])
        idx = np.where(ok)[0]
        cut = np.where(np.diff(idx) > self.cfg["gap_max"])[0]
        s = np.r_[idx[0], idx[cut + 1]]
        e = np.r_[idx[cut], idx[-1]]
        return [(a, b) for a, b in zip(s, e) if b - a >= min_len]


class Levenberg:
    def __init__(self, fun, lo, hi, config=CONFIG["lm"]):
        self.fun, self.lo, self.hi, self.cfg = fun, np.asarray(lo, float), np.asarray(hi, float), config

    def jac(self, x, r0):
        J = np.empty((len(r0), len(x)))
        for i in range(len(x)):
            h = 1e-4 * max(abs(x[i]), 1e-3)
            xi = x.copy()
            xi[i] += h if x[i] + h <= self.hi[i] else -h
            J[:, i] = (self.fun(xi) - r0) / (xi[i] - x[i])
        return J

    def run(self, x0, free=None):
        x = np.clip(np.asarray(x0, float), self.lo, self.hi)
        free = np.arange(len(x)) if free is None else np.asarray(free)
        r = self.fun(x)
        cost = r @ r
        lam = self.cfg["lam"]
        for _ in range(self.cfg["iters"]):
            J = self.jac(x, r)[:, free]
            g = J.T @ r
            H = J.T @ J
            sc = np.sqrt(np.diag(H)) + 1e-12
            improved = done = False
            for _ in range(12):
                step = np.linalg.solve(H + lam * np.diag(sc ** 2), -g)
                xn = x.copy()
                xn[free] = np.clip(x[free] + step, self.lo[free], self.hi[free])
                rn = self.fun(xn)
                cn = rn @ rn
                if cn < cost:
                    done = cost - cn < self.cfg["tol"] * cost
                    x, r, cost, lam, improved = xn, rn, cn, lam / 3, True
                    break
                lam *= 4
            if not improved or done:
                break
        return x, cost


class Windows:
    def __init__(self, store, config=CONFIG):
        self.cfg = config
        s, g = store, config["grid"]
        need = ["tw", "fan_rpm", "pump_rpm", "pump_w", "cpu_w", "gpu_w"]
        n = config["window_steps"]
        lead = config["lead_steps"]
        starts = []
        for a, b in store.segments(need, min_len=(n + lead) * g):
            starts.extend(range(a + lead * g, b - n * g, g * config["window_stride"]))
        self.i0 = np.array(starts)
        self.lead = lead
        idx = self.i0[:, None] + (np.arange(-lead, n + 1) * g)[None, :]
        def edge(name):
            v = s.v[name]
            c = np.concatenate([[0.0], np.cumsum(np.where(np.isfinite(v), v, 0.0))])
            return (c[np.clip(idx + 6, 0, len(v))] - c[np.clip(idx - 5, 0, len(v))]) / 11.0
        def block(name):
            v = s.v[name]
            c = np.concatenate([[0.0], np.cumsum(np.where(np.isfinite(v), v, 0.0))])
            return (c[idx[:, 1:]] - c[idx[:, :-1]]) / g
        self.idx = idx
        self.tw_full = edge("tw")
        self.tw = self.tw_full[:, lead:]
        self.p_cpu, self.p_gpu, self.p_pump = block("cpu_w"), block("gpu_w"), block("pump_w")
        self.fan = block("fan_rpm") / config["fan_full_rpm"]
        self.pump = block("pump_rpm") / config["pump_full_rpm"]
        self.fan_duty = block("fan_duty")
        self.t0 = s.k[self.i0]
        self.h0 = s.h[self.i0]
        self.day = s.day[self.i0]
        self.dt = float(g)
        self.span = n * g

    def shifted(self, store, shift):
        out = self.subset(np.ones(len(self.i0), bool))
        sh = np.asarray(shift, int)[:, None]
        for key, name in (("p_cpu", "cpu_w"), ("p_gpu", "gpu_w"), ("p_pump", "pump_w")):
            v = store.v[name]
            c = np.concatenate([[0.0], np.cumsum(np.where(np.isfinite(v), v, 0.0))])
            out.__dict__[key] = (c[np.clip(self.idx[:, 1:] - sh, 0, len(v))] - c[np.clip(self.idx[:, :-1] - sh, 0, len(v))]) / self.dt
        return out

    def subset(self, mask):
        out = object.__new__(Windows)
        out.__dict__ = {k: (v[mask] if isinstance(v, np.ndarray) and len(v) == len(self.i0) else v) for k, v in self.__dict__.items()}
        return out

    def __len__(self):
        return len(self.i0)


class WaterModel:
    def __init__(self, config=CONFIG):
        self.cfg = config
        p = config["params"]
        self.names, self.x0, self.lo, self.hi = p["names"], np.array(p["x0"]), np.array(p["lo"]), np.array(p["hi"])

    def conductance(self, x, fan, pump):
        return x[3] + x[4] * np.power(np.clip(fan, 0, 2), x[5]) * (1.0 + x[6] * (pump - 1.0))

    def heat(self, x, w):
        return x[1] * w.p_cpu + x[2] * w.p_gpu + w.p_pump + x[8]

    def simulate(self, x, w):
        C, Ta, L = x[0], x[7], w.lead
        G = self.conductance(x, w.fan, w.pump)[:, L:]
        Q = self.heat(x, w)[:, L:]
        T = np.empty_like(w.tw)
        T[:, 0] = w.tw[:, 0]
        for j in range(Q.shape[1]):
            g = np.maximum(G[:, j], 1e-3)
            inf = Ta + Q[:, j] / g
            T[:, j + 1] = inf + (T[:, j] - inf) * np.exp(-g * w.dt / C)
        return T

    def residual(self, x, w):
        return (self.simulate(x, w) - w.tw)[:, 1:].ravel()

    def bounds(self, fixed=None, free_p0=False):
        lo, hi = self.lo.copy(), self.hi.copy()
        pins = dict(fixed or {})
        if not free_p0:
            pins.setdefault("P0", 0.0)
        for n, v in pins.items():
            lo[self.names.index(n)] = hi[self.names.index(n)] = v
        return lo, hi

    def fit(self, w, x0=None, fixed=None, free_p0=False):
        lo, hi = self.bounds(fixed, free_p0)
        x0 = np.clip(self.x0 if x0 is None else x0, lo, hi)
        return Levenberg(lambda x: self.residual(x, w), lo, hi).run(x0)

    def skill(self, x, w):
        T = self.simulate(x, w)
        out = {}
        for h in self.cfg["horizons"]:
            j = h // self.cfg["grid"]
            e_m = T[:, j] - w.tw[:, j]
            e_p = w.tw[:, 0] - w.tw[:, j]
            out[h] = {"sse_model": float(np.sum(e_m ** 2)), "sse_persist": float(np.sum(e_p ** 2)), "n": int(len(e_m))}
        return out


class Evaluation:
    def __init__(self, store, windows, model):
        self.s, self.w, self.m = store, windows, model

    def folds(self):
        out = []
        for d in np.unique(self.w.day):
            test = self.w.day == d
            if test.sum() < 20:
                continue
            lo, hi = self.w.t0[test].min() - 1800, self.w.t0[test].max() + 1800 + self.w.span
            train = (self.w.t0 + self.w.span < lo) | (self.w.t0 > hi)
            out.append((int(d), train, test))
        return out

    def cv(self, x0=None, fixed=None, free_p0=False):
        res = []
        for d, tr, te in self.folds():
            x, cost = self.m.fit(self.w.subset(tr), x0, fixed, free_p0)
            res.append({"day": d, "n_train": int(tr.sum()), "n_test": int(te.sum()), "x": x.tolist(), "skill": self.m.skill(x, self.w.subset(te))})
        return res

    @staticmethod
    def pool(res, horizons):
        out = {}
        for h in horizons:
            sm = sum(r["skill"][h]["sse_model"] for r in res)
            sp = sum(r["skill"][h]["sse_persist"] for r in res)
            n = sum(r["skill"][h]["n"] for r in res)
            out[h] = {"rmse_model": float(np.sqrt(sm / n)), "rmse_persist": float(np.sqrt(sp / n)), "skill": float(1 - sm / sp), "n": int(n)}
        return out


class Bootstrap:
    def __init__(self, windows, model, config=CONFIG, fixed=None, free_p0=False):
        self.w, self.m, self.cfg, self.fixed, self.free_p0 = windows, model, config["bootstrap"], fixed, free_p0

    def blocks(self):
        b = np.floor(self.w.t0 / (self.cfg["block_h"] * 3600)).astype(int)
        return b, np.unique(b)

    def draw(self, seed, x0):
        rng = np.random.default_rng(seed)
        b, ub = self.blocks()
        pick = rng.choice(ub, len(ub), replace=True)
        idx = np.concatenate([np.where(b == k)[0] for k in pick])
        sub = self.w.subset(np.zeros(len(self.w), bool))
        sub.__dict__.update({k: v[idx] for k, v in self.w.__dict__.items() if isinstance(v, np.ndarray) and len(v) == len(self.w.i0)})
        x, c = self.m.fit(sub, x0, self.fixed, self.free_p0)
        return x

    def run(self, x0, draws=None):
        n = draws or self.cfg["draws"]
        return np.array([self.draw(self.cfg["seed"] + i, x0) for i in range(n)])

    def run_ray(self, x0, cluster, draws=None):
        n = draws or self.cfg["draws"]
        k = cluster.cpus
        seeds = [list(range(self.cfg["seed"] + j, self.cfg["seed"] + n, k)) for j in range(k)]
        parts = cluster.map(boot_chunk, [(sd, list(x0), self.fixed, self.free_p0) for sd in seeds])
        return np.concatenate([np.array(p) for p in parts if len(p)])


class Cluster:
    def __init__(self, config=CONFIG):
        self.cpus = config["ray"]["cpus"]
        self.own = False

    def __enter__(self):
        import ray
        self.ray = ray
        env = {"env_vars": {"PYTHONPATH": os.path.dirname(os.path.abspath(__file__)), "INERTIA_DIR": os.path.abspath(CONFIG["work"])}}
        try:
            ray.init(address="auto", logging_level="ERROR", runtime_env=env)
        except ConnectionError:
            ray.init(num_cpus=self.cpus, include_dashboard=False, logging_level="ERROR", runtime_env=env)
            self.own = True
        return self

    def __exit__(self, *exc):
        self.ray.shutdown()

    def map(self, fn, items):
        task = self.ray.remote(num_cpus=1)(fn)
        return self.ray.get([task.remote(*it) for it in items])


def boot_chunk(seeds, x0, fixed, free_p0):
    st = Store().load()
    w = Windows(st)
    m = WaterModel()
    b = Bootstrap(w, m, CONFIG, fixed, free_p0)
    return [b.draw(sd, np.array(x0)).tolist() for sd in seeds]


class Binned:
    def __init__(self, store, config=CONFIG):
        self.s, self.cfg = store, config
        self.edges = [0.0, 0.25, 0.4, 0.55, 0.7, 0.85, 0.97, 9.0]
        self.pumps = {"pump100": (4000, 9000), "pump20": (2300, 2900), "pump_mid": (2900, 4000)}

    def blocks(self, secs=300):
        v = self.s.v
        names = ["tw", "fan_rpm", "pump_rpm", "pump_w", "cpu_w", "gpu_w"]
        out = {k: [] for k in ["T", "slope", "pc", "pg", "pp", "fan", "fan_sd", "pump", "t0"]}
        tt = np.arange(secs, dtype=float) - (secs - 1) / 2
        for a, b in self.s.segments(names, min_len=secs):
            for i in range(a, b - secs + 1, secs):
                sl = slice(i, i + secs)
                tw = v["tw"][sl]
                out["T"].append(tw.mean())
                out["slope"].append(float(tt @ (tw - tw.mean()) / (tt @ tt)))
                out["pc"].append(v["cpu_w"][sl].mean())
                out["pg"].append(v["gpu_w"][sl].mean())
                out["pp"].append(v["pump_w"][sl].mean())
                out["fan"].append(v["fan_rpm"][sl].mean() / self.cfg["fan_full_rpm"])
                out["fan_sd"].append(v["fan_rpm"][sl].std() / self.cfg["fan_full_rpm"])
                out["pump"].append(v["pump_rpm"][sl].mean())
                out["t0"].append(self.s.k[i])
        return {k: np.array(x) for k, x in out.items()}

    def estimate(self, B, x, secs=300):
        C, ec, eg, Ta, P0 = x[0], x[1], x[2], x[7], x[8]
        Q = ec * B["pc"] + eg * B["pg"] + B["pp"] + P0 - C * B["slope"]
        d = B["T"] - Ta
        return Q, d

    def table(self, B, x, draws=400, seed=3):
        Q, d = self.estimate(B, x)
        rng = np.random.default_rng(seed)
        hb = (B["t0"] // 10800).astype(int)
        uh = np.unique(hb)
        rows = []
        calm = B["fan_sd"] < 0.05
        for pn, (plo, phi) in self.pumps.items():
            pm = (B["pump"] >= plo) & (B["pump"] < phi)
            for lo, hi in zip(self.edges[:-1], self.edges[1:]):
                m = calm & pm & (B["fan"] >= lo) & (B["fan"] < hi)
                if m.sum() < 3:
                    continue
                g = (Q[m] * d[m]).sum() / (d[m] ** 2).sum()
                idx = np.where(m)[0]
                bh = hb[idx]
                gs = []
                ub = np.unique(bh)
                for _ in range(draws):
                    pick = rng.choice(ub, len(ub), replace=True)
                    sel = np.concatenate([idx[bh == k] for k in pick])
                    gs.append((Q[sel] * d[sel]).sum() / (d[sel] ** 2).sum())
                rows.append({"pump": pn, "fan_lo": lo, "fan_hi": min(hi, 1.1), "fan_mean_rpm": float(B["fan"][m].mean() * self.cfg["fan_full_rpm"]), "blocks": int(m.sum()), "hours": float(m.sum() * 300 / 3600), "tw_p5": float(np.percentile(B["T"][m], 5)), "tw_p95": float(np.percentile(B["T"][m], 95)), "G": float(g), "G_lo": float(np.percentile(gs, 2.5)), "G_hi": float(np.percentile(gs, 97.5)), "q_mean_w": float(Q[m].mean())})
        return rows


class Natural:
    def __init__(self, store, config=CONFIG):
        self.s, self.cfg = store, config

    def at(self, local):
        import datetime
        t = datetime.datetime.strptime(local, "%Y-%m-%d %H:%M:%S").replace(tzinfo=datetime.timezone.utc).timestamp() - self.cfg["tz_offset"]
        return int(t - self.s.k[0])

    def window(self, x, a, b, ta=None):
        v = self.s.v
        sl = slice(a, b)
        tw = v["tw"][sl]
        tt = np.arange(len(tw), dtype=float)
        slope = float((tt - tt.mean()) @ (tw - tw.mean()) / ((tt - tt.mean()) @ (tt - tt.mean())))
        q = x[1] * v["cpu_w"][sl].mean() + x[2] * v["gpu_w"][sl].mean() + v["pump_w"][sl].mean() + x[8]
        qe = q - x[0] * slope
        ta = x[7] if ta is None else ta
        return {"tw": float(tw.mean()), "slope_mk_s": slope * 1e3, "q_w": float(q), "q_eff_w": float(qe), "G": float(qe / (tw.mean() - ta)), "fan_rpm": float(v["fan_rpm"][sl].mean()), "pump_rpm": float(v["pump_rpm"][sl].mean()), "fan_duty": float(v["fan_duty"][sl].mean()), "n": int(b - a)}

    def events(self):
        return {
            "pump_100_to_20": {"t": "2026-11-04 07:35:31", "pre": (-600, -60), "post": (120, 720)},
            "fan_setpoint_test_1": {"t": "2026-11-04 14:56:39", "pre": (-150, -5), "post": (25, 115)},
            "fan_setpoint_test_2_aquasuite_stopped": {"t": "2026-11-04 15:15:15", "pre": (-150, -5), "post": (25, 115)},
        }

    def run(self, x, tas=(22.0, 25.0, 27.0)):
        out = {}
        for name, e in self.events().items():
            t = self.at(e["t"])
            r = {}
            for ph in ("pre", "post"):
                a, b = t + e[ph][0], t + e[ph][1]
                r[ph] = self.window(x, a, b)
                r[ph + "_by_ta"] = {str(ta): self.window(x, a, b, ta)["G"] for ta in tas}
            r["G_ratio_by_ta"] = {str(ta): r["post_by_ta"][str(ta)] / r["pre_by_ta"][str(ta)] for ta in tas}
            out[name] = r
        return out


class Response:
    def __init__(self, store, model, config=CONFIG):
        self.s, self.m, self.cfg = store, model, config

    def water_kernel(self, pump_lo, pump_hi, bin_s=5, lags=100, smooth=20.0):
        v = self.s.v
        n = len(v["tw"]) // bin_s
        b = {k: np.nanmean(v[k][:n * bin_s].reshape(n, bin_s), axis=1) for k in ["tw", "cpu_w", "gpu_w", "fan_rpm", "pump_rpm"]}
        kb = self.s.k[:n * bin_s].reshape(n, bin_s)[:, 0]
        ok = np.isfinite(b["tw"]) & np.isfinite(b["cpu_w"]) & np.isfinite(b["gpu_w"]) & np.isfinite(b["fan_rpm"]) & (b["pump_rpm"] >= pump_lo) & (b["pump_rpm"] < pump_hi)
        cs = np.concatenate([[0], np.cumsum(~ok)])
        rows = np.array([i for i in range(lags, n - 3) if cs[i + 3] - cs[i - lags] == 0 and kb[i + 2] - kb[i - lags] <= (lags + 2) * bin_s + 1])
        y = b["tw"][rows + 2] - b["tw"][rows]
        Xc = np.stack([b["cpu_w"][rows - l] for l in range(lags)], axis=1)
        Xg = np.stack([b["gpu_w"][rows - l] for l in range(lags)], axis=1)
        fr = b["fan_rpm"][rows] / self.cfg["fan_full_rpm"]
        extra = np.stack([b["tw"][rows] - 25, (b["tw"][rows] - 25) * fr ** 1.6, np.ones(len(rows))], axis=1)
        A = np.hstack([Xc, Xg, extra])
        D = np.zeros((2 * (lags - 2), A.shape[1]))
        for j in range(2):
            for i in range(lags - 2):
                r = j * (lags - 2) + i
                D[r, j * lags + i], D[r, j * lags + i + 1], D[r, j * lags + i + 2] = 1, -2, 1
        sc = np.sqrt(smooth * np.trace(A.T @ A) / A.shape[1] / 1e3)
        coef = np.linalg.lstsq(np.vstack([A, sc * D]), np.concatenate([y, np.zeros(len(D))]), rcond=None)[0]
        out = {"n": int(len(rows)), "r2": float(1 - np.var(y - A @ coef) / np.var(y))}
        for name, h in (("cpu", coef[:lags]), ("gpu", coef[lags:2 * lags])):
            k = h / (2 * bin_s)
            pos = np.clip(k[:12], 0, None)
            plateau = k[:30].sum()
            cum = np.cumsum(k[:30])
            t = (np.arange(lags) + 0.5) * bin_s
            out[name] = {"kernel_1e3": (k[:24] * 1e3).round(3).tolist(), "peak_lag_s": float(t[int(np.argmax(k[:20]))]), "centroid_s": float((pos * t[:12]).sum() / pos.sum()), "onset_s": float(t[int(np.argmax(cum >= 0.1 * plateau))]), "area_1_over_c": float(plateau), "C_over_eta_kj_per_k": float(1 / plateau / 1000)}
        return out

    def spikes(self, x, durations=(5, 30, 120, 600), powers=(100.0, 200.0), delay_s=10, horizon=1800):
        C, Ta = x[0], x[7]
        G = float(self.m.conductance(x, 1.0, 1.0))
        out = []
        for p in powers:
            for d in durations:
                n = horizon
                Q = np.zeros(n)
                Q[delay_s:delay_s + d] = x[1] * p
                T = np.zeros(n)
                for j in range(1, n):
                    T[j] = T[j - 1] + (Q[j] - G * T[j - 1]) / C
                out.append({"extra_w": p, "duration_s": d, "peak_rise_k": float(T.max()), "t_peak_s": int(T.argmax()), "lsb": float(T.max() / 0.01), "rise_at_end_k": float(T[min(delay_s + d, n - 1)]), "energy_kj": float(x[1] * p * d / 1000), "frac_of_steady": float(T.max() / (x[1] * p / G))})
        return out

    def soak(self, x, hi=240.0, lo=140.0, win=300, keep=900):
        v = self.s.v
        tot = v["cpu_w"] + v["gpu_w"]
        c = np.concatenate([[0], np.cumsum(np.where(np.isfinite(tot), tot, 0))])
        bad = np.concatenate([[0], np.cumsum(~np.isfinite(tot) | ~np.isfinite(v["tw"]) | ~np.isfinite(v["fan_rpm"]))])
        mean = lambda a, b: (c[b] - c[a]) / (b - a)
        ev, i = [], win
        while i < len(tot) - keep:
            if bad[i + keep] - bad[i - win] == 0 and mean(i - win, i) > hi and mean(i, i + 60) < lo:
                ev.append(i)
                i += keep
            else:
                i += 1
        res = []
        for i in ev:
            a, b = i, i + keep
            T = np.empty(keep)
            T[0] = v["tw"][a]
            G = self.m.conductance(x, v["fan_rpm"][a:b] / self.cfg["fan_full_rpm"], v["pump_rpm"][a:b] / self.cfg["pump_full_rpm"])
            Q = x[1] * v["cpu_w"][a:b] + x[2] * v["gpu_w"][a:b] + v["pump_w"][a:b] + x[8]
            for j in range(1, keep):
                T[j] = T[j - 1] + (Q[j - 1] - G[j - 1] * (T[j - 1] - x[7])) / x[0]
            res.append((v["tw"][a:b] - v["tw"][a], T - T[0], v["tw"][a:b] - T))
        if not res:
            return {"n": 0}
        obs, mod, dif = (np.array([r[k] for r in res]) for k in range(3))
        ts = [0, 15, 30, 60, 120, 300, 600, 899]
        return {"n": len(res), "obs_rise": {t: float(obs[:, t].mean()) for t in ts}, "model_rise": {t: float(mod[:, t].mean()) for t in ts}, "obs_minus_model": {t: float(dif[:, t].mean()) for t in ts}, "obs_minus_model_sd": {t: float(dif[:, t].std()) for t in ts}, "max_obs_rise_after_drop": float(obs.max(axis=1).mean()), "p_still_rising_60s": float((obs[:, 60] > obs[:, 15] + 0.02).mean()), "pre_load_w": float(np.mean([mean(i - win, i) for i in ev])), "post_load_w": float(np.mean([mean(i, i + 60) for i in ev]))}


class Sensor:
    def __init__(self, store, config=CONFIG):
        self.s, self.cfg = store, config

    def run(self):
        v = self.s.v
        tw, tl = v["tw"], v["tw_lhm"]
        ok = np.isfinite(tw)
        d = np.diff(tw)
        d = d[np.isfinite(d)]
        nz = np.abs(d[np.abs(d) > 1e-9])
        lsb = float(np.round(np.median(nz[nz <= np.percentile(nz, 90)]), 3))
        out = {"lsb_k": 0.01, "median_nonzero_step_k": lsb, "zero_step_fraction": float((np.abs(d) < 1e-9).mean()), "one_lsb_fraction": float((np.abs(np.abs(d) - 0.01) < 1e-6).mean()), "samples": int(ok.sum())}
        blocks = []
        for a, b in self.s.segments(["tw"], min_len=600):
            for i in range(a, b - 60, 60):
                y = tw[i:i + 60]
                t = np.arange(60.0)
                p = np.polyfit(t, y, 1)
                if abs(p[0]) < 3e-4:
                    blocks.append((np.std(y - np.polyval(p, t)), np.sqrt(np.mean(np.diff(y) ** 2) / 2), p[0]))
        bl = np.array(blocks)
        out["quiet_blocks"] = int(len(bl))
        out["noise_sd_k"] = {"median": float(np.median(bl[:, 0])), "p90": float(np.percentile(bl[:, 0], 90)), "diff_based_median": float(np.median(bl[:, 1]))}
        out["quantization_sd_k"] = 0.01 / np.sqrt(12)
        m = np.isfinite(tw) & np.isfinite(tl)
        dd = (tl - tw)[m]
        out["lhm_vs_d5"] = {"mean_k": float(dd.mean()), "sd_k": float(dd.std()), "n": int(m.sum())}
        cc = {}
        a, b = tw[m] - tw[m].mean(), tl[m] - tl[m].mean()
        dm = np.diff(a), np.diff(b)
        for lag in range(-3, 4):
            x1, x2 = (dm[0][max(lag, 0):len(dm[0]) + min(lag, 0)], dm[1][max(-lag, 0):len(dm[1]) + min(-lag, 0)])
            cc[lag] = float(np.corrcoef(x1, x2)[0, 1])
        out["lhm_vs_d5_xcorr_of_steps"] = cc
        out["min_resolvable_rate_k_per_min_over_30s"] = 0.01 / 30 * 60
        out["min_resolvable_rate_k_per_min_over_120s"] = 0.01 / 120 * 60
        return out


class FanSpool:
    def __init__(self, store, config=CONFIG):
        self.s, self.cfg = store, config

    def static_map(self):
        v = self.s.v
        d, r = v["fan_duty"], v["fan_rpm"]
        ok = np.isfinite(d) & np.isfinite(r)
        steady = np.zeros(len(d), bool)
        steady[30:] = np.abs(d[30:] - d[:-30]) < 0.3
        bins = np.arange(0, 101, 5.0)
        out = []
        for lo, hi in zip(bins[:-1], bins[1:]):
            m = ok & steady & (d >= lo) & (d < hi + (0.2 if hi == 100 else 0))
            if m.sum() >= 30:
                out.append((float(d[m].mean()), float(np.median(r[m])), int(m.sum())))
        return out

    def spool(self):
        v = self.s.v
        sm = np.array(self.static_map())
        d, r = v["fan_duty"], v["fan_rpm"]
        ok = np.isfinite(d) & np.isfinite(r)
        a, b = int(np.argmax(ok)), len(d)
        target = np.interp(np.nan_to_num(d), sm[:, 0], sm[:, 1])
        res = {}
        for tau in (0.5, 1, 2, 3, 5, 8, 12, 20, 30):
            y = np.empty(len(d))
            y[0] = target[0]
            al = 1 - np.exp(-1.0 / tau)
            for i in range(1, len(d)):
                y[i] = y[i - 1] + al * (target[i] - y[i - 1])
            m = ok.copy()
            m[:200] = False
            res[tau] = float(np.sqrt(np.mean((y[m] - r[m]) ** 2)))
        best = min(res, key=res.get)
        return {"static_map": sm.round(1).tolist(), "rmse_by_tau": res, "tau_s": best}


class Plant:
    def __init__(self, x, model, config=CONFIG):
        self.x, self.m, self.p, self.cfg = np.array(x, float), model, config["plant"], config

    def G(self, rpm, q, gs=None):
        x = self.x.copy()
        if gs is not None and rpm <= 0.0:
            x[3] = gs
        return float(self.m.conductance(x, rpm / self.cfg["fan_full_rpm"], q))

    def min_run_rpm(self, fan_map):
        sm = np.array(fan_map)
        return float(np.interp(self.p["min_run_duty"], sm[:, 0], sm[:, 1]))

    def heating_rate(self, q_w, tw, rpm, q_pump, gs=None):
        q_w = q_w + self.x[8]
        return (q_w - self.G(rpm, q_pump, gs) * (tw - self.x[7])) / self.x[0] * 60.0

    def run(self, q_w, t0, gs, fan_map, limit=True, q_pump=None, dead_s=31.0, horizon=3600, tick_phase=0, target=None, state="stopped"):
        p, full = self.p, self.cfg["fan_full_rpm"]
        q_pump = p["pump_floor_q"] if q_pump is None else q_pump
        target = full if target is None else target
        mn = self.min_run_rpm(fan_map)
        n = horizon
        q_w = q_w + self.x[8]
        T = np.empty(n + 1)
        T[0] = t0
        rpm = 0.0 if state == "stopped" else mn
        cmd = rpm
        running = state != "stopped"
        trace = np.zeros(n)
        t_cmd = None
        al = 1 - np.exp(-1.0 / p["spool_s"])
        for t in range(n):
            if t % p["tick_s"] == tick_phase:
                meas = T[max(t - int(dead_s), 0)]
                if not running and meas >= p["restart_c"]:
                    running = True
                    t_cmd = t
                    cmd = max(mn, min(target, p["up_rpm"])) if limit else target
                elif running:
                    cmd = min(target, cmd + p["up_rpm"]) if limit else target
            trace[t] = rpm
            rpm += al * (cmd - rpm)
            g = self.G(rpm, q_pump, gs)
            T[t + 1] = T[t] + (q_w - g * (T[t] - self.x[7])) / self.x[0]
        out = {"t_cmd_s": t_cmd, "peak_k": float(T.max()), "t_peak_s": int(T.argmax()), "final_k": float(T[-1])}
        for lim in (p["restart_c"], p["limit_plan_c"], p["limit_takeover_c"], p["limit_top_c"]):
            hit = np.where(T >= lim)[0]
            out[f"t_{lim:g}_s"] = int(hit[0]) if len(hit) else None
        full_at = np.where(trace >= 0.98 * full)[0]
        out["t_full_s"] = int(full_at[0]) if len(full_at) else None
        out["tw_at_full_k"] = float(T[full_at[0]]) if len(full_at) else None
        out["rpm_at_cmd_plus"] = {s: float(trace[min(t_cmd + s, n - 1)]) for s in (0, 30, 60, 90)} if t_cmd is not None else {}
        return out

    def steady(self, q_w, rpm, q_pump, gs=None):
        return float(self.x[7] + (q_w + self.x[8]) / self.G(rpm, q_pump, gs))

    def capacity(self, t_c, rpm, q_pump):
        return float(self.G(rpm, q_pump) * (t_c - self.x[7]) - self.x[8])

    def visibility(self, q_w, rpm0, gs, fan_map, q_pump=None, dead_s=31.0, horizon=None):
        p = self.p
        q_pump = p["pump_floor_q"] if q_pump is None else q_pump
        lim = p["limit_plan_c"]
        seen, pnr = None, None
        for t0 in np.arange(30.0, 46.0, 0.1):
            if seen is None:
                T, n = t0, horizon or p["horizon_s"]
                for _ in range(n):
                    T = T + (q_w + self.x[8] - self.G(rpm0, q_pump, gs) * (T - self.x[7])) / self.x[0]
                    if T >= lim:
                        seen = float(t0)
                        break
        for t0 in np.arange(30.0, 52.0, 0.1):
            if self.steady(q_w, self.cfg["fan_full_rpm"], q_pump, gs) > lim:
                break
            r = self.run(q_w, t0 + (q_w + self.x[8]) * dead_s / self.x[0], gs, fan_map, True, q_pump, 0.0, 1500, 0, None, "stopped" if rpm0 == 0 else "running")
            if r["peak_k"] > lim:
                pnr = float(t0)
                break
        rate = (q_w + self.x[8]) / self.x[0]
        return {"q_w": q_w, "rpm0": rpm0, "horizon_s": horizon or p["horizon_s"], "seen_from_c": seen, "no_return_c": pnr, "feasible": bool(self.steady(q_w, self.cfg["fan_full_rpm"], q_pump, gs) <= lim), "margin_k": None if seen is None or pnr is None else pnr - seen, "margin_s": None if seen is None or pnr is None else (pnr - seen) / rate}

    def cycle(self, q_idle, band, rpm_run, q_pump, gs):
        lo, hi = band
        mid = 0.5 * (lo + hi)
        q_idle = q_idle + self.x[8]
        up = (q_idle - self.G(0.0, q_pump, gs) * (mid - self.x[7])) / self.x[0]
        dn = (self.G(rpm_run, q_pump) * (mid - self.x[7]) - q_idle) / self.x[0]
        t_up = (hi - lo) / up if up > 0 else None
        t_dn = (hi - lo) / dn if dn > 0 else None
        return {"q_idle_w": q_idle, "band": [lo, hi], "rpm_run": rpm_run, "t_stopped_s": t_up, "t_running_s": t_dn, "period_s": None if t_up is None or t_dn is None else t_up + t_dn}


class Fir:
    def __init__(self, lags, smooth=30.0, ridge=1e-6):
        self.lags, self.smooth, self.ridge = lags, smooth, ridge

    def design(self, x, rows):
        L = self.lags
        return np.stack([x[rows - l] for l in range(L)], axis=1)

    def solve(self, X, y, extra):
        n, L = X.shape
        A = np.hstack([X, extra])
        D = np.zeros((L - 2, A.shape[1]))
        for i in range(L - 2):
            D[i, i], D[i, i + 1], D[i, i + 2] = 1, -2, 1
        sc = np.sqrt(self.smooth * np.trace(A.T @ A) / A.shape[1] / 1e3)
        A2 = np.vstack([A, sc * D, np.sqrt(self.ridge * np.trace(A.T @ A) / A.shape[1]) * np.eye(A.shape[1])[:L]])
        y2 = np.concatenate([y, np.zeros(D.shape[0] + L)])
        coef = np.linalg.lstsq(A2, y2, rcond=None)[0]
        return coef[:L], coef[L:], 1 - np.var(y - A @ coef) / np.var(y)


class Dynamics:
    def __init__(self, store, config=CONFIG):
        self.s, self.cfg = store, config

    def rows(self, names, lags, stride):
        out = []
        for a, b in self.s.segments(names, min_len=lags + 60):
            out.append(np.arange(a + lags, b, stride))
        return np.concatenate(out)

    def step_curve(self, y_name, p_name, lags=150, stride=2, with_water=True):
        v = self.s.v
        rows = self.rows([y_name, p_name, "tw"], lags, stride)
        fir = Fir(lags)
        X = fir.design(v[p_name], rows)
        extra = np.stack([v["tw"][rows], np.ones(len(rows))], axis=1) if with_water else np.ones((len(rows), 1))
        h, e, r2 = fir.solve(X, v[y_name][rows], extra)
        return np.cumsum(h), h, r2, int(len(rows))

    @staticmethod
    def two_exp(x, t):
        d, r1, t1, r2, t2 = x
        u = np.maximum(t - d, 0.0)
        return r1 * (1 - np.exp(-u / t1)) + r2 * (1 - np.exp(-u / t2))

    def fit_step(self, S):
        t = np.arange(len(S), dtype=float)
        lo, hi = [0.0, 0.0, 0.3, -0.1, 3.0], [20.0, 1.0, 40.0, 1.0, 400.0]
        best = None
        for d0 in (1.0, 3.0, 6.0):
            for t2 in (15.0, 60.0):
                x0 = [d0, max(S[-1] * 0.6, 1e-3), 3.0, max(S[-1] * 0.4, 1e-3), t2]
                x, c = Levenberg(lambda x: self.two_exp(x, t) - S, lo, hi, {"iters": 80, "lam": 1.0, "tol": 1e-9}).run(x0)
                if best is None or c < best[1]:
                    best = (x, c)
        x = best[0]
        return {"dead_s": float(x[0]), "R1": float(x[1]), "tau1": float(x[2]), "R2": float(x[3]), "tau2": float(x[4]), "R_total": float(x[1] + x[3]), "rmse": float(np.sqrt(best[1] / len(S)))}

    def t_frac(self, S, f=0.632):
        tgt = f * S[-1]
        i = int(np.argmax(S >= tgt))
        return float(i)

    def die_responses(self):
        out = {}
        for name, y, p in [("cpu_core_max", "cpu_max", "cpu_w"), ("cpu_core_avg", "cpu_avg", "cpu_w"), ("gpu_core", "gpu_core", "gpu_w"), ("gpu_hot_spot", "gpu_hot", "gpu_w")]:
            S, h, r2, n = self.step_curve(y, p)
            f = self.fit_step(S)
            f.update({"r2": float(r2), "n": n, "t63_s": self.t_frac(S), "t90_s": self.t_frac(S, 0.9), "step": S[::10].round(4).tolist()})
            out[name] = f
        return out




class StepTest:
    def __init__(self, x, model, config=CONFIG, lump=None):
        self.x, self.m, self.cfg = np.array(x, float), model, config
        self.cfg_t = config["steptest"]
        self.lump = lump

    def protocol(self, kind, hold):
        t = self.cfg_t
        g = lambda f, q: float(self.m.conductance(self.x, f, q))
        qp = t["pump_floor"]
        lead = [(1.0, qp, t["lead_s"], 1.0)]
        if kind == "fan":
            return lead + [(f, qp, hold, g(f, qp) / g(1.0, qp)) for f in t["fan_levels"]] + [(1.0, qp, t["lead_s"], 1.0), (1.0, qp, t["lead_s"], t["low_frac"])]
        if kind == "fan_const_load":
            return lead + [(f, qp, hold, 1.0) for f in t["fan_levels"]] + [(1.0, qp, t["lead_s"], 1.0)]
        return lead + [(1.0, 1.0, hold, 1.0), (1.0, qp, hold, 1.0), (1.0, 1.0, hold, 1.0), (1.0, qp, hold, 1.0)]

    def simulate(self, seq, q, c, ta, gfun, noise, rng, q_bias=1.0, delay=0, two=False):
        rep = lambda i: np.concatenate([np.full(int(s[2]), s[i]) for s in seq])
        fan, pump, qf = rep(0), rep(1), rep(3)
        n = len(fan)
        qn = q * qf + noise["q_sd"] * rng.standard_normal(n)
        qd = np.r_[np.full(delay, qn[0]), qn[:n - delay]] if delay else qn
        c1 = self.lump["C1"] * c / (self.lump["C1"] + self.lump["C2"]) if two else c
        c2, kk = c - c1, (self.lump["K"] if two else 0.0)
        T = np.empty(n + 1)
        T[0] = t2 = noise["t0"]
        for j in range(n):
            ex = kk * (T[j] - t2)
            T[j + 1] = T[j] + (qd[j] * q_bias - gfun(fan[j], pump[j]) * (T[j] - ta) - ex) / c1
            if two:
                t2 += ex / c2
        return fan, pump, qn, T + noise["t_sd"] * rng.standard_normal(n + 1)

    def fit(self, fan, pump, qn, obs, levels, x0, free_scale, dt=5, delay=0):
        n = len(qn) // dt * dt
        qd = qn[:n].reshape(-1, dt).mean(axis=1)
        sh = int(delay // dt)
        if sh:
            qd = np.r_[np.full(sh, qd[0]), qd[:-sh]]
        idx = np.array([levels.index((f, q)) for f, q in zip(fan[:n:dt], pump[:n:dt])])
        od = obs[:n + 1:dt]
        t0 = obs[:30].mean()
        k = len(levels)

        def res(p):
            c, ta, sc, g = p[0], p[1], p[2], p[3:]
            gt = g[idx]
            T = np.empty(len(qd) + 1)
            T[0] = t0
            for j in range(len(qd)):
                T[j + 1] = T[j] + dt * (sc * qd[j] - gt[j] * (T[j] - ta)) / c
            return T[1:] - od[1:]
        lo = np.r_[3000.0, 15.0, 0.8 if free_scale else 1.0, np.full(k, 0.3)]
        hi = np.r_[40000.0, 30.0, 1.2 if free_scale else 1.0, np.full(k, 80.0)]
        return Levenberg(res, lo, hi, {"iters": 40, "lam": 1.0, "tol": 1e-8}).run(x0)[0]

    def study(self, kind, hold, reps, mode, seed=11):
        t = self.cfg_t
        md = t["modes"][mode]
        rng = np.random.default_rng(seed)
        seq = self.protocol(kind, hold)
        levels = sorted({(s[0], s[1]) for s in seq})
        pf = lambda f, q: float(self.m.conductance(self.x, f, q))
        noise = {**t["noise"], "t0": t["t0"]}
        truth = np.array([pf(f, q) for f, q in levels])
        free = kind == "fan"
        two = md["truth"] == "two"
        est, est_b = [], []
        for _ in range(reps):
            x0 = np.r_[self.x[0] * 1.1, self.x[7] + 1.5, 1.0, truth * 1.2]
            est.append(self.fit(*self.simulate(seq, t["q_w"], self.x[0], self.x[7], pf, noise, rng, 1.0, t["delay_s"], two), levels, x0, free, delay=md["fit_delay"]))
            est_b.append(self.fit(*self.simulate(seq, t["q_w"], self.x[0], self.x[7], pf, noise, rng, t["q_bias"], t["delay_s"], two), levels, x0, free, delay=md["fit_delay"]))
        est, est_b = np.array(est), np.array(est_b)
        ref = levels.index((1.0, t["pump_floor"] if kind != "pump" else 1.0))
        g, gb = est[:, 3:], est_b[:, 3:]
        tr = truth / truth[ref]
        return {"kind": kind, "mode": mode, "hold_s": hold, "total_s": int(sum(s[2] for s in seq)), "levels": [list(l) for l in levels], "G_true": truth, "reps": reps, "G_rel_sd": g.std(axis=0) / truth, "G_rel_bias": g.mean(axis=0) / truth - 1, "G_ratio_rel_sd": (g / g[:, [ref]]).std(axis=0) / tr, "G_ratio_rel_bias": (g / g[:, [ref]]).mean(axis=0) / tr - 1, "C_rel_sd": float(est[:, 0].std() / self.x[0]), "C_rel_bias": float(est[:, 0].mean() / self.x[0] - 1), "Ta_sd": float(est[:, 1].std()), "Ta_bias_with_q_bias": float((est_b[:, 1] - self.x[7]).mean()), "G_rel_bias_with_q_bias": (gb / truth).mean(axis=0) - 1, "G_ratio_rel_bias_with_q_bias": (gb / gb[:, [ref]]).mean(axis=0) / tr - 1}

    def jobs(self):
        t = self.cfg_t
        out = [(k, h, m) for k in ("fan", "pump") for h in t["holds"] for m in t["modes"]]
        return out + [("fan_const_load", t["holds"][3], "lump_fit")]

    def design(self, cluster):
        x, lump = list(self.x), self.lump
        return cluster.map(step_task, [(x, lump, k, h, self.cfg_t["reps"], m) for k, h, m in self.jobs()])


def step_task(x, lump, kind, hold, reps, mode):
    return StepTest(np.array(x), WaterModel(), CONFIG, lump).study(kind, hold, reps, mode)


class Pipeline:
    def __init__(self, config=CONFIG):
        self.cfg = config
        self.work = config["work"]
        os.makedirs(self.work, exist_ok=True)
        self.st = Store(config).load()
        self.w = Windows(self.st, config)
        self.m = WaterModel(config)

    def path(self, name):
        return os.path.join(self.work, f"res_{name}.json")

    def save(self, name, obj):
        with open(self.path(name), "w", encoding="utf-8") as f:
            json.dump(obj, f, indent=1, default=lambda o: o.tolist() if hasattr(o, "tolist") else str(o))
        return obj

    def load(self, name):
        with open(self.path(name), encoding="utf-8") as f:
            return json.load(f)

    def water(self):
        m, w, st = self.m, self.w, self.st
        x, cost = m.fit(w)
        ev = Evaluation(st, w, m)
        res = ev.cv(x)
        pooled = ev.pool(res, self.cfg["horizons"])
        out = {"names": m.names, "x": x, "cost": cost, "windows": len(w), "window_hours": len(w) * self.cfg["window_stride"] * self.cfg["grid"] / 3600, "span_h": [float(w.h0.min()), float(w.h0.max())]}
        folds = []
        for r in res:
            sk = r["skill"]
            folds.append({"day": r["day"], "n_test": r["n_test"], "n_train": r["n_train"], "skill": {h: 1 - sk[h]["sse_model"] / sk[h]["sse_persist"] for h in sk}, "rmse_model": {h: float(np.sqrt(sk[h]["sse_model"] / sk[h]["n"])) for h in sk}, "rmse_persist": {h: float(np.sqrt(sk[h]["sse_persist"] / sk[h]["n"])) for h in sk}, "x": r["x"]})
        out["cv"] = {"folds": folds, "pooled": pooled, "pass_pooled": bool(all(pooled[h]["rmse_model"] < pooled[h]["rmse_persist"] for h in pooled)), "folds_beating_persistence": {h: int(sum(r["skill"][h]["sse_model"] < r["skill"][h]["sse_persist"] for r in res)) for h in self.cfg["horizons"]}}
        sens = []

        def add(tag, fixed, free_p0=False):
            xx, cc = m.fit(w, None, fixed, free_p0)
            p = ev.pool(ev.cv(xx, fixed, free_p0), self.cfg["horizons"])
            sens.append({"tag": tag, "x": xx, "cost": cc, "skill": {h: p[h]["skill"] for h in p}})
        add("base: Ta free, P0 = 0", None)
        for ta in (20, 22, 24, 26, 27, 28):
            add(f"Ta fixed {ta} C, P0 free", {"Ta": float(ta)}, True)
        for mm in (0.5, 0.7, 1.0, 1.3):
            add(f"fan exponent m fixed {mm}", {"m": mm})
        for g in (0.8, 0.9):
            add(f"eta_gpu fixed {g}", {"eta_gpu": g})
        add("pump effect gamma = 0", {"gamma": 0.0})
        out["sensitivity"] = sens
        out["alt_curve"] = {"x": sens[2]["x"], "cost": sens[2]["cost"], "tag": sens[2]["tag"]}
        return self.save("water", out)

    def bootstrap(self, draws=None):
        base = self.load("water")
        x = np.array(base["x"])
        b = Bootstrap(self.w, self.m)
        if self.cfg["ray"]["cpus"] > 0:
            with Cluster(self.cfg) as cl:
                X = b.run_ray(x, cl, draws)
                how = f"ray, {'local cluster started by this run and shut down afterwards' if cl.own else 'attached to the running cluster'}, {cl.cpus} tasks of 1 CPU"
        else:
            X = b.run(x, draws)
            how = "serial"
        stats = lambda a: {"p2_5": float(np.percentile(a, 2.5)), "p50": float(np.percentile(a, 50)), "p97_5": float(np.percentile(a, 97.5))}
        cond = lambda f, q: np.array([float(f(r, q)) for r in X])
        g100 = cond(lambda r, q: self.m.conductance(r, 1.0, 1.0), 0)
        g20 = cond(lambda r, q: self.m.conductance(r, 1.0, 2570.0 / self.cfg["pump_full_rpm"]), 0)
        ratio = lambda f: stats(cond(lambda r, q: self.m.conductance(r, f, 1.0) / self.m.conductance(r, 1.0, 1.0), 0))
        out = {"how": how, "draws": int(len(X)), "block_h": self.cfg["bootstrap"]["block_h"], "params": {n: stats(X[:, i]) for i, n in enumerate(self.m.names)}, "G_full_pump100": stats(g100), "G_full_pump20": stats(g20), "tau_full_pump100_s": stats(X[:, 0] / g100), "tau_full_pump20_s": stats(X[:, 0] / g20), "G_ratio_pump20_over_100": stats(g20 / g100), "G_ratio_fan_0_8": ratio(0.8), "G_ratio_fan_0_5": ratio(0.5), "G_ratio_fan_0_27": ratio(0.27)}
        C = out["params"]["C"]
        out["C_half_width_rel"] = (C["p97_5"] - C["p2_5"]) / 2 / base["x"][0]
        out["pass_C_within_30pct"] = bool(out["C_half_width_rel"] <= 0.30)
        out["samples"] = X
        return self.save("boot", out)

    def tables(self):
        wat = self.load("water")
        x = np.array(wat["x"])
        st = self.st
        out = {}
        b = Binned(st)
        B = b.blocks()
        out["binned"] = {"base": b.table(B, x), "Ta20_P0free": b.table(B, np.array(wat["sensitivity"][1]["x"])), "Ta27_P0free": b.table(B, np.array(wat["sensitivity"][5]["x"]))}
        out["natural"] = Natural(st).run(x)
        v = st.v
        ok = np.isfinite(v["tw"]) & np.isfinite(v["fan_rpm"]) & np.isfinite(v["pump_rpm"])
        r = v["fan_rpm"][ok] / self.cfg["fan_full_rpm"]
        pq_ = v["pump_rpm"][ok]
        tw = v["tw"][ok]
        out["exposure"] = {"hours": float(ok.sum() / 3600), "tw_min": float(tw.min()), "tw_p1": float(np.percentile(tw, 1)), "tw_p99": float(np.percentile(tw, 99)), "tw_max": float(tw.max()), "fan_full_fraction": float((r > 0.97).mean()), "fan_below_0_7_hours": float((r < 0.7).sum() / 3600), "fan_below_0_4_hours": float((r < 0.4).sum() / 3600), "fan_min_rpm": float(v["fan_rpm"][ok].min()), "pump_hours": {"100": float((pq_ > 4000).sum() / 3600), "20": float(((pq_ > 2300) & (pq_ < 2900)).sum() / 3600), "other": float((((pq_ >= 2900) & (pq_ <= 4000)) | (pq_ <= 2300)).sum() / 3600)}, "sys_temp_range": [float(np.nanmin(v["sys_t"])), float(np.nanmax(v["sys_t"]))], "cpu_power_hours": float(np.isfinite(v["cpu_w"]).sum() / 3600), "span_h": float((st.k[-1] - st.k[0]) / 3600)}
        return self.save("tables", out)

    def dynamics(self):
        x = np.array(self.load("water")["x"])
        d = Dynamics(self.st)
        r = Response(self.st, self.m)
        out = {"die": d.die_responses(), "kernel_pump100": r.water_kernel(4000, 9000), "kernel_pump20": r.water_kernel(2300, 2900)}
        out["spikes"] = r.spikes(x)
        out["soak"] = r.soak(x)
        out["sensor"] = Sensor(self.st).run()
        out["fan"] = FanSpool(self.st).spool()
        return self.save("dynamics", out)

    def rpm_to_cool(self, pl, q, t_c):
        for rpm in range(0, 1461, 10):
            if pl.G(float(rpm), 0.5) * (t_c - pl.x[7]) >= q:
                return rpm
        return None

    def settings(self):
        wat = self.load("water")
        dyn = self.load("dynamics")
        x = np.array(wat["x"])
        xa = np.array(wat["alt_curve"]["x"])
        P = self.cfg["plant"]
        fm = dyn["fan"]["static_map"]
        out = {"curves": {"base": x, "alt": xa}}
        dead = {"100": dyn["kernel_pump100"]["cpu"]["centroid_s"], "20": dyn["kernel_pump20"]["cpu"]["centroid_s"]}
        dead["floor_est"] = dead["20"] * (2570.0 / 2400.0)
        out["dead_s"] = dead
        dd = dead["floor_est"]
        for tag, xx in (("base", x), ("alt", xa)):
            pl = Plant(xx, self.m, self.cfg)
            mn = pl.min_run_rpm(fm)
            sec = {"min_run_rpm": mn}
            sec["G"] = {"full_pump100": pl.G(1460, 1.0), "full_pump_floor": pl.G(1460, 0.5), "min_run_pump_floor": pl.G(mn, 0.5), "rpm_700_pump_floor": pl.G(700, 0.5), "rpm_1000_pump_floor": pl.G(1000, 0.5)}
            sec["tau_s"] = {k: float(xx[0] / v) for k, v in sec["G"].items()}
            sec["capacity_w"] = {str(t): pl.capacity(t, 1460, 0.5) for t in (P["limit_plan_c"], P["limit_takeover_c"], P["limit_top_c"])}
            sec["steady_full_pump_floor"] = {str(q): pl.steady(q, 1460, 0.5) for q in (200, 300, 400, 500, 600)}
            sec["heating"] = [{"q_w": q, "tw": tw, "stopped_gs_lo": pl.heating_rate(q, tw, 0, 0.5, P["gs_lo"]), "stopped_gs_hi": pl.heating_rate(q, tw, 0, 0.5, P["gs_hi"]), "full_fan_pump_floor": pl.heating_rate(q, tw, 1460, 0.5), "initial_q_over_c": (q + xx[8]) / xx[0] * 60} for q in P["heat_w"] for tw in (33.0, 40.0)]
            sec["worst"] = []
            for q in (400.0, 500.0, 550.0, 600.0):
                for gs in (P["gs_lo"], P["gs_hi"]):
                    for lim in (True, False):
                        r = pl.run(q, P["stop_c"], gs, fm, lim, None, dd, 3600)
                        sec["worst"].append({"q_w": q, "gs": gs, "rate_limit": lim, **{k: r[k] for k in ("t_cmd_s", "t_35_s", "t_full_s", "tw_at_full_k", "t_46_s", "t_48_s", "t_50_s", "peak_k", "final_k")}})
            sec["visibility"] = [pl.visibility(q, r0, gs, fm, None, dd, hz) for q in (200.0, 300.0, 400.0, 500.0) for r0 in (0.0, mn) for gs in (P["gs_lo"], P["gs_hi"]) for hz in (120, 240)]
            sec["cycle"] = [{**pl.cycle(q, band, rpm, 0.5, gs), "gs": gs} for q in (40.0, 60.0, 80.0, 120.0) for band in ((33.0, 35.0), (32.0, 36.0), (31.0, 37.0)) for rpm in (mn, 600.0, 800.0) for gs in (P["gs_lo"], P["gs_hi"])]
            sec["rpm_to_cool_at_34c"] = {str(q): self.rpm_to_cool(pl, q, 34.0) for q in (40.0, 60.0, 80.0, 120.0)}
            out[tag] = sec
        out["smoothing"] = [{"tau_s": ts, "durations": {str(d): float(1 - np.exp(-d / ts)) for d in (5, 30, 120, 600)}} for ts in (10, 30, 60, 120)]
        out["ramp_s"] = {"up_stop_to_full": float(np.ceil(1460.0 / P["up_rpm"]) * P["tick_s"]), "down_full_to_min_run": float(np.ceil((1460.0 - out["base"]["min_run_rpm"]) / P["down_rpm"]) * P["tick_s"])}
        return self.save("settings", out)

    def ext(self):
        from inertia_ext import Ext
        return Ext(self).run()

    def steptest(self):
        x = np.array(self.load("water")["x"])
        lump = {k: self.load("ext")["lump"][k] for k in ("C1", "C2", "K")}
        with Cluster(self.cfg) as cl:
            return self.save("steptest", {"rows": StepTest(x, self.m, self.cfg, lump).design(cl), "cluster_own": cl.own, "cpus": cl.cpus})

    def report(self):
        from inertia_report import Report
        res = {k: self.load(k) for k in ("water", "boot", "tables", "dynamics", "settings", "steptest", "physical", "ext")}
        out = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "docs", "thermal.md")
        with open(out, "w", encoding="utf-8") as fh:
            fh.write(Report(res, self.cfg).build() + chr(10))

    def physical(self):
        C = self.load("water")["x"][0]
        ph = {"coolant_l": [1.5, 2.5], "coolant_kj_per_l_k": [4.0, 4.18], "rad_metal_kj_per_k_each": [0.4, 1.0], "rads": 3, "blocks_pump_fittings_kj_per_k": [0.4, 0.8]}
        lo = ph["coolant_l"][0] * ph["coolant_kj_per_l_k"][0] + ph["rads"] * ph["rad_metal_kj_per_k_each"][0] + ph["blocks_pump_fittings_kj_per_k"][0]
        hi = ph["coolant_l"][1] * ph["coolant_kj_per_l_k"][1] + ph["rads"] * ph["rad_metal_kj_per_k_each"][1] + ph["blocks_pump_fittings_kj_per_k"][1]
        metal = ph["rads"] * np.mean(ph["rad_metal_kj_per_k_each"]) + np.mean(ph["blocks_pump_fittings_kj_per_k"])
        return self.save("physical", {"assumptions": ph, "estimate_kj_per_k": [lo, hi], "fitted_kj_per_k": C / 1000, "metal_mid_kj_per_k": metal, "implied_coolant_l_at_mid_metal": (C / 1000 - metal) / np.mean(ph["coolant_kj_per_l_k"])})

    def run(self, stages):
        for s in stages:
            getattr(self, s)()


if __name__ == "__main__":
    import inertia
    inertia.Pipeline().run(sys.argv[1:] or inertia.CONFIG["stages"])
