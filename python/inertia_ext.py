import datetime
import numpy as np
from inertia import CONFIG, Levenberg, Plant, WaterModel, Windows

def slope(y):
    t = np.arange(len(y), dtype=float)
    t -= t.mean()
    return float(t @ (y - y.mean()) / (t @ t))


class Lump2:
    def __init__(self, x, model, config=CONFIG):
        self.x, self.m, self.cfg = np.array(x, float), model, config["ext"]["lump2"]
        self.lm = config["lm"]

    def simulate(self, p, w):
        C1, C2, K = p
        G, Q = self.m.conductance(self.x, w.fan, w.pump), self.m.heat(self.x, w)
        ns = self.cfg["substeps"]
        h = w.dt / ns
        T1 = w.tw_full[:, 0].copy()
        T2 = T1.copy()
        out = np.empty((len(T1), G.shape[1] + 1))
        out[:, 0] = T1
        for j in range(G.shape[1]):
            if j == w.lead:
                T1 = w.tw_full[:, j].copy()
                out[:, j] = T1
            g, q = G[:, j], Q[:, j]
            for _ in range(ns):
                ex = K * (T1 - T2)
                T1 = T1 + h * (q - g * (T1 - self.x[7]) - ex) / C1
                T2 = T2 + h * ex / C2
            out[:, j + 1] = T1
        return out[:, w.lead:]

    def residual(self, p, w):
        return (self.simulate(p, w) - w.tw)[:, 1:].ravel()

    def fit(self, w, two=True):
        c = self.cfg
        lo, hi, x0 = np.array(c["lo"]), np.array(c["hi"]), np.array(c["x0"])
        if not two:
            lo, hi, x0 = np.array([lo[0] + 2000, 1e9, 0.0]), np.array([hi[0] * 3, 1e9, 0.0]), np.array([11700.0, 1e9, 0.0])
        lm = Levenberg(lambda p: self.residual(p, w), lo, hi, self.lm)
        p, cost = lm.run(x0, free=[0, 1, 2] if two else [0])
        return p, float(cost)

    def variants(self, store, w):
        c = self.cfg
        pm = w.pump.mean(axis=1)
        p100 = pm > c["p100_min"]
        out = []
        for d in c["delays"]:
            shift = np.where(p100, d, d + c["pump20_extra_s"])
            ws = w.shifted(store, shift)
            for sub in c["subsets"]:
                mask = np.ones(len(w), bool) if sub == "all" else (p100 if sub == "p100" else ~p100)
                wm = ws.subset(mask)
                p2, c2 = self.fit(wm, True)
                p1, c1 = self.fit(wm, False)
                C1, C2, K = p2
                out.append({"delay_p100_s": d, "delay_p20_s": d + c["pump20_extra_s"], "subset": sub, "n": int(mask.sum()), "C1": float(C1), "C2": float(C2), "K": float(K), "C_total": float(C1 + C2), "tau_fast_s": float(C1 * C2 / (C1 + C2) / K), "sse": c2, "C_1lump": float(p1[0]), "sse_1lump": c1})
        allv = [v for v in out if v["subset"] == "all"]
        best = min(allv, key=lambda v: v["sse"])
        keys = ("C1", "C2", "K", "C_total", "tau_fast_s")
        return {"variants": out, "best": best, "range": {k: [min(v[k] for v in out), max(v[k] for v in out)] for k in keys}}


class Events:
    def __init__(self, store, x, dead, model, config=CONFIG):
        self.s, self.x, self.dead, self.m, self.cfg = store, np.array(x, float), dead, model, config["ext"]["events"]
        v = store.v
        self.q = v["cpu_w"] + v["gpu_w"] + v["pump_w"]
        self.t, self.r, self.pr = v["tw"], v["fan_rpm"], v["pump_rpm"]
        self.pump_full = config["pump_full_rpm"]

    def find(self):
        c, q = self.cfg, self.q
        ok = np.isfinite(q) & np.isfinite(self.t) & (self.r > c["full_rpm"])
        nb = np.concatenate([[0], np.cumsum(~ok)])
        cq = np.concatenate([[0.0], np.cumsum(np.where(ok, q, 0.0))])
        ev, i, n = [], 800, len(q)
        while i < n - 800:
            if nb[i + 700] - nb[i - 400] == 0:
                ts = np.arange(i - 60, i + 120)
                d = (cq[ts + 20] - cq[ts]) / 20.0 - (cq[ts] - cq[ts - 20]) / 20.0
                k = int(np.argmax(np.abs(d)))
                t = int(ts[k])
                pre, post = q[t - 150:t - 10], q[t + 10:t + 150]
                dq = post.mean() - pre.mean()
                if abs(dq) >= c["min_dq"] and pre.std() < c["calm"] * abs(dq) and post.std() < c["calm"] * abs(dq) and abs(d[k]) > 0.6 * abs(dq):
                    cross = np.where((q[t - 15:t + 16] - (pre.mean() + dq / 2)) * np.sign(dq) > 0)[0]
                    j = t - 15 + int(cross[0]) if len(cross) else t
                    ev.append((j, dq, pre.mean()))
                    i = j + 400
                    continue
            i += 5
        return ev

    def pclass(self, j):
        pr = self.pr[j]
        return "p100" if pr > self.cfg["p100_rpm"] else ("p20" if self.cfg["p20_rpm"][0] < pr < self.cfg["p20_rpm"][1] else "other")

    def responses(self, ev):
        n = 600
        out = {"p100": [], "p20": []}
        for j, dq, pm in ev:
            pc = self.pclass(j)
            if pc not in out:
                continue
            s0 = slope(self.t[j - 150:j - 10])
            y = (self.t[j:j + n] - self.t[j] - s0 * np.arange(n)) / dq * 100.0
            L = int(round(self.dead[pc]))
            h = np.cumsum(self.q[j - L:j + n - L] - pm) / dq * 100.0
            out[pc].append((y, h, dq, int(np.sign(dq))))
        return out

    def step(self, G, C1, C2, K, n=700):
        T1 = T2 = 0.0
        out = np.empty(n)
        for t in range(n):
            out[t] = T1
            ex = K * (T1 - T2)
            T1 += (100.0 - G * T1 - ex) / C1
            if C2:
                T2 += ex / C2
        return out

    def model(self, tt, L, G, C1, C2, K):
        r = self.step(G, C1, C2, K)
        return np.where(tt >= L, np.interp(tt - L, np.arange(len(r)), r), 0.0)

    def gof(self, pc):
        return float(self.m.conductance(self.x, 1.0, 1.0 if pc == "p100" else 2570.0 / self.pump_full))

    def fit_pooled(self, means, mode, ses=None):
        tt = np.arange(600.0)
        gs = {pc: self.gof(pc) for pc in means}
        pcs = list(means)
        k = len(pcs)
        ct = self.x[0]
        def lumps(p):
            if mode == "one":
                return p[k], 0.0, 0.0
            if mode == "fixed":
                return p[k], ct - p[k], p[k + 1]
            return p[k], p[k + 1], p[k + 2]
        def res(p):
            C1, C2, K = lumps(p)
            return np.concatenate([((self.model(tt, p[i], gs[pc], C1, C2, K) - means[pc]) / (1.0 if ses is None else np.maximum(ses[pc], 0.01)))[10:] for i, pc in enumerate(pcs)])
        tail = {"one": ([3000.0], [40000.0], [11700.0]), "fixed": ([3000.0, 5.0], [ct - 500.0, 600.0], [7000.0, 50.0]), "free": ([3000.0, 500.0, 5.0], [20000.0, 40000.0, 600.0], [7000.0, 4700.0, 50.0])}[mode]
        lo, hi = np.r_[np.zeros(k), tail[0]], np.r_[np.full(k, 120.0), tail[1]]
        x0 = np.r_[[self.dead[pc] for pc in pcs], tail[2]]
        p, cost = Levenberg(res, lo, hi, {"iters": 40, "lam": 1.0, "tol": 1e-8}).run(x0)
        C1, C2, K = lumps(p)
        return {"delay_s": dict(zip(pcs, p[:k].tolist())), "C1": float(C1), "C2": float(C2), "K": float(K), "C_total": float(C1 + C2), "tau_fast_s": float(C1 * C2 / (C1 + C2) / K) if K > 0 and C2 > 0 else None, "sse": float(cost)}

    def pooled(self, resp, idx=None):
        means, ses = {}, {}
        for pc, rows in resp.items():
            sel = rows if idx is None else [rows[i] for i in idx[pc]]
            Y = np.array([r[0] for r in sel])
            means[pc], ses[pc] = Y.mean(0), Y.std(0, ddof=1) / np.sqrt(len(Y))
        out = {m: self.fit_pooled(means, m) for m in ("one", "fixed", "free")}
        out["weighted"] = self.fit_pooled(means, "fixed", ses)
        for pc in means:
            out[pc] = self.fit_pooled({pc: means[pc]}, "fixed")
        return out

    def run(self, boot=60):
        ev = self.find()
        resp = self.responses(ev)
        c = self.cfg
        out = {"n_found": len(ev), "n": {pc: len(r) for pc, r in resp.items()}}
        allfit = self.pooled(resp)
        fit = allfit["weighted"]
        rng = np.random.default_rng(5)
        bs = []
        for _ in range(boot):
            idx = {pc: rng.integers(0, len(r), len(r)) for pc, r in resp.items()}
            bs.append(self.pooled(resp, idx)["weighted"])
        keys = ("C1", "C2", "K", "C_total", "tau_fast_s")
        fit["boot_ci"] = {k: [float(np.percentile([b[k] for b in bs], 2.5)), float(np.percentile([b[k] for b in bs], 97.5))] for k in keys}
        fit["boot_n"] = boot
        out["fit"] = fit
        out["fit_variants"] = {k: allfit[k] for k in ("fixed", "p100", "p20")}
        out["fit_one"] = allfit["one"]
        out["fit_free_total"] = allfit["free"]
        for pc, rows in resp.items():
            Y = np.array([r[0] for r in rows])
            H = np.array([r[1] for r in rows])
            M, S = Y.mean(0), Y.std(0, ddof=1) / np.sqrt(len(Y))
            G = self.gof(pc)
            L = self.dead[pc]
            tt = np.arange(600.0)
            m1 = self.model(tt, L, G, self.x[0], 0.0, 0.0)
            m2 = self.model(tt, L, G, fit["C1"], fit["C2"], fit["K"])
            blk = {"n": len(rows), "up": int(sum(r[3] > 0 for r in rows)), "mean_abs_dq_w": float(np.mean([abs(r[2]) for r in rows])), "lags": {str(s): {"obs": float(M[s]), "se": float(S[s]), "lump1": float(m1[s]), "lump2": float(m2[s])} for s in c["lags"]}}
            wins = {}
            for a, b in c["windows"]:
                row = {}
                for tag, MM in (("obs", M), ("lump1", m1), ("lump2", m2)):
                    sl = (MM[b] - MM[a]) / (b - a)
                    heat = (H[:, b - 1] - H[:, a - 1]).mean() / (b - a)
                    row[f"k_min_100w_{tag}"] = float(sl * 60)
                    row[f"c_eff_{tag}_kj"] = float((heat - G * MM[a:b].mean()) / sl / 1000) if sl > 0 else None
                wins[f"{a}-{b}"] = row
            blk["windows"] = wins
            on = [s for s in c["lags"] if M[s] > 3 * S[s]]
            blk["onset_s"] = float(on[0]) if on else None
            out[pc] = blk
        return out


class Frozen:
    def __init__(self, windows, model, folds, x_global, config=CONFIG):
        self.w, self.m, self.folds, self.xg, self.cfg = windows, model, folds, np.array(x_global), config["ext"]["frozen"]

    def freeze(self, w):
        L = w.lead
        out = w.subset(np.ones(len(w), bool))
        for key in ("p_cpu", "p_gpu", "p_pump"):
            a = getattr(out, key).copy()
            a[:, L:] = a[:, L - 1:L]
            setattr(out, key, a)
        return out

    def collect(self, parts):
        out = {}
        for h in self.cfg["horizons"]:
            j = h // int(self.w.dt)
            ek = np.concatenate([p[0][:, j] - p[3][:, j] for p in parts])
            ef = np.concatenate([p[1][:, j] - p[3][:, j] for p in parts])
            ep = np.concatenate([p[3][:, 0] - p[3][:, j] for p in parts])
            u = -ef
            out[str(h)] = {"n": int(len(ef)), "rmse_known": float(np.sqrt((ek ** 2).mean())), "rmse_frozen": float(np.sqrt((ef ** 2).mean())), "rmse_persist": float(np.sqrt((ep ** 2).mean())), "skill_frozen": float(1 - (ef ** 2).sum() / (ep ** 2).sum()), "skill_known": float(1 - (ek ** 2).sum() / (ep ** 2).sum()), "bias_frozen": float(ef.mean()), "p95_under": float(np.percentile(u, 95)), "p99_under": float(np.percentile(u, 99)), "max_under": float(u.max()), "two_sigma": float(2 * np.sqrt((ef ** 2).mean()))}
        return out

    def run(self):
        parts = []
        for f in self.folds:
            sub = self.w.subset(self.w.day == f["day"])
            x = np.array(f["x"])
            parts.append((self.m.simulate(x, sub), self.m.simulate(x, self.freeze(sub)), None, sub.tw))
        glob = [(self.m.simulate(self.xg, self.w), self.m.simulate(self.xg, self.freeze(self.w)), None, self.w.tw)]
        return {"cv": self.collect(parts), "global_fit": self.collect(glob)}


class Restart:
    def __init__(self, x, model, fan_map, dead, lump, config=CONFIG):
        self.x, self.pl, self.lump, self.dead = np.array(x, float), Plant(x, model, config), lump, dead
        self.cfg, self.p, self.full = config["ext"]["restart"], config["plant"], config["fan_full_rpm"]
        self.mn = self.pl.min_run_rpm(fan_map)

    def caps(self, model, cs):
        if model == "lump1":
            return self.x[0] * cs, None, 0.0
        return self.lump["C1"] * cs, self.lump["C2"] * cs, self.lump["K"]

    def floor(self, meas):
        out = 0.0
        for thr, tgt in self.cfg["rule"]:
            if meas >= thr:
                out = self.mn if tgt == "start" else (self.full if tgt == "full" else tgt)
        return out

    def run(self, q_w, model, policy, cs=1.0, ds=1.0, phase=0, gs=0.5, horizon=None, rpm_fixed=None):
        p, c = self.p, self.cfg
        C1, C2, K = self.caps(model, cs)
        dead = int(round(self.dead * ds))
        qh = q_w + self.x[8]
        n = horizon or c["horizon_s"]
        qp = p["pump_floor_q"]
        T = np.empty(n + 1)
        T[0] = t2 = c["start_c"]
        rpm = cmd = 0.0
        running, t_cmd, t_take, rpm_take = False, None, None, 0.0
        al = 1 - np.exp(-1.0 / p["spool_s"])
        for t in range(n):
            meas = T[max(t - dead, 0)]
            if policy in ("stuck", "rule"):
                if t_take is None and meas >= p["limit_takeover_c"]:
                    t_take, rpm_take = t, rpm
                if t % p["tick_s"] == phase:
                    if not running and meas >= p["restart_c"]:
                        running, t_cmd, cmd = True, t, self.mn
                    if running and policy == "rule":
                        cmd = max(cmd, self.floor(meas))
                tgt = cmd if t_take is None else max(cmd, rpm_take + (self.full - rpm_take) * min(1.0, (t - t_take) / c["takeover_ramp_s"]))
                rpm += al * (tgt - rpm)
            elif rpm_fixed is not None:
                rpm = rpm_fixed
            g = self.pl.G(rpm, qp, gs)
            ex = K * (T[t] - t2)
            T[t + 1] = T[t] + (qh - g * (T[t] - self.x[7]) - ex) / C1
            if C2:
                t2 += ex / C2
        out = {"peak_k": float(T.max()), "t_peak_s": int(T.argmax()), "t_cmd_s": t_cmd, "t_takeover_s": t_take, "final_k": float(T[-1])}
        for lim in (35.0, 46.0, 48.0, 50.0):
            hit = np.where(T >= lim)[0]
            out[f"t_{lim:g}_s"] = int(hit[0]) if len(hit) else None
        out["T"] = T
        return out

    def worst(self, q_w, model, policy, cs, ds):
        runs = [self.run(q_w, model, policy, cs, ds, ph, gs) for ph in self.cfg["phases"] for gs in self.cfg["gs"]]
        keep = ("peak_k", "t_cmd_s", "t_takeover_s")
        out = {"peak_k": max(r["peak_k"] for r in runs), "t_cmd_s": min(r["t_cmd_s"] for r in runs if r["t_cmd_s"] is not None) if any(r["t_cmd_s"] is not None for r in runs) else None, "t_takeover_s": min(r["t_takeover_s"] for r in runs if r["t_takeover_s"] is not None) if any(r["t_takeover_s"] is not None for r in runs) else None}
        for lim in (35, 46, 48, 50):
            ts = [r[f"t_{lim}_s"] for r in runs if r[f"t_{lim}_s"] is not None]
            out[f"t_{lim}_s"] = min(ts) if ts else None
        return out

    def table(self):
        c = self.cfg
        rows = []
        for model in ("lump1", "lump2"):
            for vn, (cs, ds) in c["variants"].items():
                for q in c["q_w"]:
                    for policy in ("stuck", "rule"):
                        rows.append({"model": model, "variant": vn, "q_w": q, "policy": policy, **self.worst(q, model, policy, cs, ds)})
        daemon = []
        for model in ("lump1", "lump2"):
            for vn, (cs, ds) in c["variants"].items():
                if ds != 1.0:
                    continue
                for q in c["daemon_q_w"]:
                    for gs in c["gs"]:
                        r = self.run(q, model, "daemon", cs, ds, 0, gs, c["daemon_horizon_s"], 0.0)
                        daemon.append({"model": model, "variant": vn, "q_w": q, "gs": gs, **{k: r[k] for k in ("t_35_s", "t_46_s", "t_48_s", "t_50_s")}})
        return {"stuck": rows, "daemon": daemon, "min_run_rpm": self.mn}

    def rates(self, model):
        c = self.cfg
        out = []
        for q in c["rates_q_w"]:
            for gs in c["gs"]:
                r = self.run(q, model, "daemon", 1.0, 1.0, 0, gs, c["rate_secs"] + 1, 0.0)
                T = r["T"]
                out.append({"model": model, "q_w": q, "gs": gs, "avg_k_min_4min": float((T[c["rate_secs"]] - T[0]) / c["rate_secs"] * 60), "k_min_at_t0": float((T[10] - T[0]) / 10 * 60), "k_min_at_60s": float((T[90] - T[30]) / 60 * 60), "k_min_at_120s": float((T[150] - T[90]) / 60 * 60)})
        return out


class Spikes2:
    def __init__(self, x, lump, config=CONFIG):
        self.x, self.lump, self.m, self.cfg = np.array(x, float), lump, WaterModel(config), config["ext"]["spikes"]

    def run(self):
        G = float(self.m.conductance(self.x, 1.0, 1.0))
        out = []
        for p in self.cfg["powers"]:
            for d in self.cfg["durations"]:
                row = {"extra_w": p, "duration_s": d}
                for tag, (C1, C2, K) in (("lump1", (self.x[0], 1.0, 0.0)), ("lump2", (self.lump["C1"], self.lump["C2"], self.lump["K"]))):
                    n = 1800
                    T1 = T2 = 0.0
                    pk = 0.0
                    for j in range(n):
                        q = self.x[1] * p if self.cfg["delay_s"] <= j < self.cfg["delay_s"] + d else 0.0
                        ex = K * (T1 - T2)
                        T1 += (q - G * T1 - ex) / C1
                        T2 += ex / C2
                        pk = max(pk, T1)
                    row[f"peak_{tag}_k"] = pk
                out.append(row)
        return out


class PumpFacts:
    def __init__(self, store, config=CONFIG):
        self.s, self.cfg, self.tz = store, config["ext"]["pump"], config["tz_offset"]

    def local(self, text):
        return int(datetime.datetime.strptime(text, "%Y-%m-%d %H:%M:%S").replace(tzinfo=datetime.timezone.utc).timestamp() - self.tz - self.s.k[0])

    def label(self, k):
        return datetime.datetime.fromtimestamp(int(k) + self.tz, datetime.timezone.utc).strftime("%m-%d %H:%M")

    def run(self):
        v, c = self.s.v, self.cfg
        tw, pr = v["tw"], v["pump_rpm"]
        okp = np.isfinite(tw) & np.isfinite(pr)
        floor = okp & (pr > 2300) & (pr < 2800)
        out = {"floor": {"hours": float(floor.sum() / 3600), "tw_max": float(tw[floor].max()), "hours_above_35": float((floor & (tw > 35)).sum() / 3600), "hours_above_36": float((floor & (tw > 36)).sum() / 3600)}, "pump_rpm_min": float(np.nanmin(pr)), "hours_below_2500": float((okp & (pr < 2500)).sum() / 3600), "hours_2500_2800": float((okp & (pr >= 2500) & (pr < 2800)).sum() / 3600)}
        days = ((self.s.k + self.tz) // 86400).astype(int)
        rows = []
        for d in np.unique(days[okp]):
            m = okp & (days == d)
            rows.append({"day": datetime.datetime.fromtimestamp(int(d) * 86400, datetime.timezone.utc).strftime("%m-%d"), "hours": float(m.sum() / 3600), "pump100_h": float((m & (pr > 4000)).sum() / 3600), "pump20_h": float((m & (pr > 2300) & (pr < 2900)).sum() / 3600), "pump_mid_h": float((m & (pr >= 2900) & (pr <= 4000)).sum() / 3600), "pump_ge_4813_h": float((m & (pr >= 4813)).sum() / 3600)})
        out["days"] = rows
        d4 = okp & (days == days[self.local("2026-11-04 12:00:00")])
        bins = []
        for lo, hi in c["tw_bins"]:
            m = d4 & (tw >= lo) & (tw < hi)
            bins.append({"tw_lo": lo, "tw_hi": hi, "hours": float(m.sum() / 3600), "pump_median_rpm": float(np.median(pr[m])) if m.sum() else None})
        out["day_1104_by_tw"] = bins
        q = v["cpu_w"] + v["gpu_w"]
        evs = []
        for text in c["events_local"]:
            t = self.local(text)
            ss = np.arange(t - c["search_s"], t + c["search_s"])
            d = np.array([np.nanmean(pr[s + 5:s + 15]) - np.nanmean(pr[s - 15:s - 5]) for s in ss])
            s = int(ss[np.nanargmax(np.abs(d))])
            ch = float(d[np.nanargmax(np.abs(d))])
            row = {"asked_local": text, "found": bool(abs(ch) >= c["min_change_rpm"]), "t_local": self.label(self.s.k[s]), "pump_before": float(np.nanmean(pr[s - 60:s - 5])), "pump_after": float(np.nanmean(pr[s + 20:s + 60])), "tw_before": float(np.nanmean(tw[s - 30:s - 5])), "d_tw_30s_k": float(np.nanmean(tw[s + 25:s + 40]) - np.nanmean(tw[s - 20:s - 5])), "q_before_w": float(np.nanmean(q[s - 120:s - 5])) if np.isfinite(q[s - 120:s - 5]).any() else None, "q_after_w": float(np.nanmean(q[s + 5:s + 120])) if np.isfinite(q[s + 5:s + 120]).any() else None}
            evs.append(row)
        out["ramps"] = evs
        return out


class LowRpm:
    def __init__(self, store, model, config=CONFIG):
        self.s, self.m, self.cfg, self.tz, self.fullrpm = store, model, config["ext"]["lowrpm"], config["tz_offset"], config["fan_full_rpm"]

    def blocks(self, secs=60):
        v = self.s.v
        names = ["tw", "fan_rpm", "pump_rpm", "pump_w", "cpu_w", "gpu_w"]
        ok = np.ones(len(self.s.k), bool)
        for n in names:
            ok &= np.isfinite(v[n])
        ok &= v["fan_rpm"] < 1400
        idx = np.where(ok)[0]
        cut = np.where(np.diff(idx) > 1)[0]
        st, en = np.r_[idx[0], idx[cut + 1]], np.r_[idx[cut], idx[-1]]
        rows = []
        for a, b in zip(st, en):
            for i in range(a, b - secs + 2, secs):
                sl = slice(i, i + secs)
                rows.append([v["fan_rpm"][sl].mean(), v["tw"][sl].mean(), slope(v["tw"][sl]), v["cpu_w"][sl].mean(), v["gpu_w"][sl].mean(), v["pump_w"][sl].mean(), v["pump_rpm"][sl].mean()])
        return np.array(rows)

    def run(self, curves):
        B = self.blocks()
        out = []
        for lo, hi in self.cfg["bins"]:
            m = (B[:, 0] >= lo) & (B[:, 0] < hi)
            if not m.any():
                continue
            row = {"rpm_lo": lo, "rpm_hi": hi, "blocks": int(m.sum()), "rpm": float(B[m, 0].mean()), "tw": float(B[m, 1].mean()), "slope_k_min": float(B[m, 2].mean() * 60)}
            for name, x in curves.items():
                x = np.array(x)
                q = x[1] * B[m, 3] + x[2] * B[m, 4] + B[m, 5] + x[8]
                pred = self.m.conductance(x, B[m, 0] / self.fullrpm, B[m, 6] / CONFIG["pump_full_rpm"]) * (B[m, 1] - x[7])
                row[name] = {"pred_removal_w": float(pred.mean()), **{f"gap_w_c{int(c / 1000)}": float((q - c * B[m, 2] - pred).mean()) for c in self.cfg["c_j_per_k"]}}
            out.append(row)
        return out

    def rates(self):
        v = self.s.v
        out = []
        for text, w in zip(self.cfg["rate_local"], self.cfg["rate_secs"]):
            t = int(datetime.datetime.strptime(text, "%Y-%m-%d %H:%M:%S").replace(tzinfo=datetime.timezone.utc).timestamp() - self.tz - self.s.k[0])
            y = v["tw"][t:t + w]
            q = float((v["cpu_w"][t:t + w] + v["gpu_w"][t:t + w] + v["pump_w"][t:t + w]).mean())
            sl = slope(y) if np.isfinite(y).all() else float("nan")
            out.append({"local": text, "rpm": float(np.nanmean(v["fan_rpm"][t:t + w])), "tw": float(np.nanmean(y)), "k_min": sl * 60, "q_w": q, "c_upper_kj_if_no_removal": q / sl / 1000 if sl > 0 else None, "extra_heat_w_if_c11_7": 11700.0 * sl - q if sl > 0 else None})
        return out


class SpoolJumps:
    def __init__(self, store, fan_map, config=CONFIG):
        self.s, self.cfg, self.map = store, config["ext"]["spool"], np.array(fan_map)

    def run(self):
        v = self.s.v
        D, R = v["fan_duty"], v["fan_rpm"]
        dd = np.diff(D)
        ii = np.where(np.abs(dd) > self.cfg["duty_jump"])[0]
        ii = ii[np.r_[True, np.diff(ii) > 30]]
        rows = []
        for i in ii:
            if i < 5 or i + 30 >= len(R) or not np.isfinite(R[i - 3:i + 30]).all() or not np.isfinite(D[i - 3:i + 30]).all():
                continue
            if abs(np.nanmean(D[i + 1:i + 10]) - D[i]) < self.cfg["duty_jump"] or np.nanstd(D[i + 1:i + 20]) > 2:
                continue
            r0, r1 = R[i], np.median(R[i + 15:i + 25])
            if abs(r1 - r0) < self.cfg["min_step_rpm"]:
                continue
            fr = (R[i:i + 25] - r0) / (r1 - r0)
            rows.append((np.sign(r1 - r0), abs(r1 - r0), int(np.argmax(fr >= 0.63)), int(np.argmax(fr >= 0.9)), r0))
        A = np.array(rows)
        out = {"n": int(len(A)), "lowest_rpm_seen": float(np.nanmin(R))}
        for nm, sg in (("up", 1), ("down", -1), ("all", 0)):
            m = np.ones(len(A), bool) if sg == 0 else A[:, 0] == sg
            if m.sum():
                out[nm] = {"n": int(m.sum()), "t63_med_s": float(np.median(A[m, 2])), "t63_p90_s": float(np.percentile(A[m, 2], 90)), "t90_med_s": float(np.median(A[m, 3])), "t90_p90_s": float(np.percentile(A[m, 3], 90)), "min_start_rpm": float(A[m, 4].min())}
        return out


class Ema:
    def __init__(self, store, config=CONFIG):
        self.v, self.cfg = store.v["cpu_max"], config["ext"]["ema"]

    def run(self):
        x = self.v
        out = {"raw_max_c": float(np.nanmax(x)), "raw_seconds_at_limit": int((x >= self.cfg["limit_c"]).sum())}
        for tau in self.cfg["taus_s"]:
            al = 1 - np.exp(-1.0 / tau)
            y, s = np.full(len(x), np.nan), np.nan
            for i, val in enumerate(x):
                if np.isfinite(val):
                    s = val if not np.isfinite(s) else s + al * (val - s)
                    y[i] = s
            out[str(tau)] = {"max_c": float(np.nanmax(y)), "seconds_at_limit": int((y >= self.cfg["limit_c"]).sum())}
        return out


class Sinks:
    def __init__(self, x, model, config=CONFIG):
        self.x, self.m, self.cfg = np.array(x, float), model, config["ext"]["sinks"]
        self.q_pump = config["plant"]["pump_floor_q"]
        self.lim = config["plant"]

    def table(self, g_values):
        c = self.cfg
        rows = []
        for name, g in g_values.items():
            for sink in c["sink_c"]:
                rows.append({"g_label": name, "g": float(g), "sink_c": sink, "cap_46_w": float(g * (self.lim["limit_plan_c"] - sink) - self.x[8]), "cap_48_w": float(g * (self.lim["limit_takeover_c"] - sink) - self.x[8]), "cap_50_w": float(g * (self.lim["limit_top_c"] - sink) - self.x[8]), "steady_500_c": float(sink + (c["steady_q_w"] + self.x[8]) / g), "steady_peak_c": float(sink + (c["peak_cpu_w"] + c["peak_gpu_rated_w"] + 4.0 + self.x[8]) / g)})
        return rows


class Ext:
    def __init__(self, pipeline):
        self.p = pipeline
        self.cfg = pipeline.cfg

    def run(self):
        p = self.p
        wat, dyn = p.load("water"), p.load("dynamics")
        x = np.array(wat["x"])
        fm = dyn["fan"]["static_map"]
        dead = {"p100": dyn["kernel_pump100"]["cpu"]["centroid_s"], "p20": dyn["kernel_pump20"]["cpu"]["centroid_s"]}
        dead_floor = dead["p20"] * (2570.0 / 2400.0)
        out = {"checks": {"alt_curve_matches_sensitivity": bool(np.allclose(wat["alt_curve"]["x"], wat["sensitivity"][2]["x"])), "alt_tag": wat["alt_curve"]["tag"], "alt_p0": wat["alt_curve"]["x"][8], "alt_ta": wat["alt_curve"]["x"][7]}}
        l2 = Lump2(x, p.m, self.cfg)
        out["lump2"] = l2.variants(p.st, p.w)
        out["events"] = Events(p.st, x, dead, p.m, self.cfg).run()
        ef = out["events"]["fit"]
        lump = {"C1": ef["C1"], "C2": x[0] - ef["C1"], "K": ef["K"]}
        out["lump"] = {**lump, "tau_fast_s": lump["C1"] * lump["C2"] / (lump["C1"] + lump["C2"]) / lump["K"], "C_total": x[0]}
        out["frozen"] = Frozen(p.w, p.m, wat["cv"]["folds"], x, self.cfg).run()
        rs = Restart(x, p.m, fm, dead_floor, lump, self.cfg)
        out["restart"] = rs.table()
        out["rates"] = rs.rates("lump1") + rs.rates("lump2")
        out["spikes2"] = Spikes2(x, lump, self.cfg).run()
        out["pump"] = PumpFacts(p.st, self.cfg).run()
        out["lowrpm"] = {"table": LowRpm(p.st, p.m, self.cfg).run({"base": x, "alt22": wat["alt_curve"]["x"], "alt20": wat["sensitivity"][1]["x"]}), "rates": LowRpm(p.st, p.m, self.cfg).rates()}
        out["ema"] = Ema(p.st, self.cfg).run()
        out["spool"] = SpoolJumps(p.st, fm, self.cfg).run()
        out["dead_floor_s"] = dead_floor
        v = p.st.v
        q = v["cpu_w"] + v["gpu_w"] + v["pump_w"]
        out["peaks"] = {"cpu_max_w": float(np.nanmax(v["cpu_w"])), "gpu_max_w": float(np.nanmax(v["gpu_w"])), "q_max_w": float(np.nanmax(q)), "q_p999_w": float(np.nanpercentile(q, 99.9))}
        pl = Plant(x, p.m, self.cfg)
        alt = Plant(wat["alt_curve"]["x"], p.m, self.cfg)
        mn = pl.min_run_rpm(fm)
        alt20 = Plant(wat["sensitivity"][1]["x"], p.m, self.cfg)
        out["near_stopped"] = {"min_run_rpm": mn, "net_w_at_30c": {n: q.capacity(30.0, mn, 0.5) for n, q in (("base", pl), ("alt22", alt), ("alt20", alt20))}, "gross_w_at_30c": {n: q.G(mn, 0.5) * (30.0 - q.x[7]) for n, q in (("base", pl), ("alt22", alt), ("alt20", alt20))}}
        return p.save("ext", out)
