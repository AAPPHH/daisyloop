import copy
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import numpy as np
import steptrain as S
from export import LinearExporter
from models import Prep
from stepsim import Plant, SIM


def test_prep_fits_on_the_given_rows_and_exempts_free_columns():
    rng = np.random.default_rng(0)
    F = np.column_stack([rng.normal(size=1000), np.where(rng.random(1000) < 0.02, 1.0, 0.0)])
    rows = np.arange(1000)
    prep = Prep([0, 1]).fit(F, rows)
    assert abs(prep.mean[0]) < 0.1 and abs(prep.std[0] - 1.0) < 0.1
    free = np.array([False, True])
    assert prep.clipped(F, rows, 5.0, free) == 0.0
    assert prep.clipped(F, rows, 5.0) > 0.0
    assert prep.transform(F, rows, 5.0, free)[:, 1].max() > 5.0
    assert prep.transform(F, rows, 5.0)[:, 1].max() == 5.0


def test_plant_bound_lifts_a_low_fan_duty_and_rpm_is_never_negative():
    n = 3000
    fan, pump = np.full(n, 27.0), np.full(n, np.nan)
    sim = Plant().run(fan, pump, np.full(n, 150.0), np.full(n, 120.0), np.random.default_rng(0))
    assert sim["fan_duty"].max() > 40.0 and sim["water"].max() > 35.0
    assert (sim["fan_rpm"] >= 0).all()
    cold = Plant().run(np.full(300, 0.0), np.full(300, np.nan), np.full(300, 40.0), np.full(300, 30.0), np.random.default_rng(0))
    assert (cold["fan_duty"] == 0.0).all() and np.isfinite(cold["water"]).all()


def test_grey_box_recovers_the_fan_exponent_and_rejects_none():
    n = 2400
    fan = np.concatenate([np.full(600, 100.0), np.full(600, 50.0), np.full(600, 30.0), np.full(600, 100.0)])
    pump = np.full(n, 19.8)
    load = np.concatenate([np.full(600, 200.0), np.full(600, 120.0), np.full(600, 80.0), np.full(600, 200.0)])
    gb = S.GreyBox()
    for m, expect in ((1.61, True), (0.0, False)):
        sim = Plant(SIM["plant"], expect).run(fan, pump, load * 0.55, load * 0.45, np.random.default_rng(1))
        seg = {"w": sim["water"], "q": 0.96 * load * 0.55 + load * 0.45 + sim["pump_pow"], "f": sim["fan_rpm"], "p": sim["pump_rpm"], "t": np.arange(n) / 3600.0, "fit": np.ones(n, bool)}
        th, sse, k = gb.fit([seg])
        _, sse0, _ = gb.fit([seg], {"m": 0.0}, th)
        lr = k * np.log(max(sse0, sse) / sse) / 100.0
        assert (lr > 3.84 and th[2] > 0.5) == expect, (m, th[2], lr)


def test_response_term_matches_between_trainer_and_exporter():
    r = S.Response({"feat": "step.fx", "pump_feat": "pump_rpm", "k": 0.003, "m": 1.6, "gamma": 0.03, "ta": 25.0, "pump": [2570.0, 4800.0], "dead": [31.0, 11.0], "gain": {"water": 1.0, "cpu": 0.5, "gpu": 0.5}})
    model = {"features": ["pump_rpm", "step.fx"], "response": r.block()}
    for prpm in (1800.0, 2570.0, 3400.0, 4800.0):
        for tg in ("water_30", "water_120", "cpu_60"):
            a = float(r.term(np.array([prpm]), np.array([3.0]), tg)[0])
            b = LinearExporter.term(model, [prpm, 3.0], tg)
            assert abs(a - b) < 1e-12, (prpm, tg, a, b)


def run_case(case, seed, threads):
    root = tempfile.mkdtemp(prefix="steptest-")
    cfg = dict(S.CFG, store_dir=S.FRESH["store_dir"], threads=threads)
    try:
        return S.dry_task({"cfg": cfg, "case": case, "seed": seed, "root": root})
    finally:
        shutil.rmtree(root, ignore_errors=True)


def test_gate_ships_with_a_fan_response_refuses_without_and_repeats():
    if not os.path.exists(os.path.join(S.FRESH["store_dir"], "raw.npy")):
        return
    a, b = run_case("fan", 2, 1), run_case("fan", 2, 4)
    assert a["ship"] and a["identified"]["fan"] and 1 / 1.5 < a["ratio"] < 1.5
    assert a["ship"] == b["ship"] and a["ratio"] == b["ratio"] and a["fit_rmse"] == b["fit_rmse"]
    for case in ("nofan", "drift_nofan"):
        assert not run_case(case, 2, 1)["ship"]


NAMES = ["model_deploy.json", "feature_spec_deploy.json"]
ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
STORE = os.path.exists(os.path.join(S.FRESH["store_dir"], "raw.npy"))


def doc(name):
    return open(os.path.join(ROOT, "docs", name), encoding="utf8").read()


def test_dryrun_entry_point_runs_as_documented_and_writes_the_pair():
    if not STORE:
        return
    root = tempfile.mkdtemp(prefix="steptest-entry-")
    try:
        r = subprocess.run([sys.executable, os.path.join(ROOT, "python", "steptrain.py"), "dryrun", root, "fan", "1"], capture_output=True, text=True, timeout=900)
        assert r.returncode == 0, r.stderr[-2000:]
        out = json.loads(r.stdout)
        assert out["summary"]["fan"]["n"] == 1
        d = os.path.join(root, "fan-0", "out")
        assert out["summary"]["fan"]["ship"] == 1
        assert {*NAMES, "report.json"} <= set(os.listdir(d))
        assert not (set(os.listdir(d)) & {"golden_deploy.json", "candidate_golden_deploy.json"})
        assert copy_steps(d, os.path.join(root, "models"), os.path.join(root, "install"))
        cargo = subprocess.run(["cargo", "test", "--manifest-path", os.path.join(ROOT, "tools", "model_infer", "Cargo.toml"), "--test", "load_dir", "--", "--ignored"], env=dict(os.environ, STEPTRAIN_OUT=os.path.join(root, "install")), capture_output=True, text=True, timeout=900)
        assert cargo.returncode == 0 and "1 passed" in cargo.stdout, cargo.stdout[-2000:] + cargo.stderr[-2000:]
    finally:
        shutil.rmtree(root, ignore_errors=True)


def copy_steps(out, models, install):
    where = {r"c:\clones\daisyloop\models\steptrain": out, r"c:\clones\daisyloop\models": models, r"c:\program files\daisyloop\models": install}
    for d in (models, install):
        os.makedirs(d, exist_ok=True)
        for n in NAMES:
            shutil.copy(os.path.join(ROOT, "models", n), os.path.join(d, n))
    cmds = re.findall(r"Copy-Item .*", doc("steptest.md").split("## 7.")[1].split("## 8.")[0])
    assert len(cmds) == 2
    for c in cmds:
        names = re.findall(r"(\w+\.json)", c)
        src = re.search(r"([A-Za-z]:[^,'\s]*)\\" + names[0], c).group(1).lower()
        dest = re.sub(r"(?i)\s*-Force.*$", "", c[c.rindex(".json") + 5:]).strip(" '\"").lower().rstrip("\\")
        assert sorted(names) == sorted(NAMES), c
        assert src in where and dest in where, (src, dest)
        for n in names:
            shutil.copy(os.path.join(where[src], n), os.path.join(where[dest], n))
    return all(open(os.path.join(install, n), "rb").read() == open(os.path.join(out, n), "rb").read() for n in NAMES)



class FakeStore:
    def __init__(self, T, wins, cols):
        self.T, self.wins, self.cols, self.cfg = T, wins, cols, S.CFG

    def col(self, c):
        return self.cols[c]

    def labels(self):
        return ["fan"] * len(self.wins)

    def spans(self):
        return S.Store.spans(self)


def fake_cols(T, bad):
    names = ["d5next/water/temp", "d5next/fan/rpm", "d5next/pump/rpm", "/intelcpu/0/power/0", "nvml/0/power", "d5next/pump/power", "d5next/fan/duty", "d5next/pump/duty", S.CFG["phase_channel"]]
    cols = {c: np.full(T, v) for c, v in zip(names, [30.0, 700.0, 2500.0, 40.0, 30.0, 4.3, 50.0, np.nan, np.nan])}
    cols["d5next/water/temp"] += np.arange(T) * 0.001
    cols[S.CFG["phase_channel"]][400:1200] = 1.0 + np.arange(800) // 100
    for a, b in bad:
        for c in names[:6]:
            cols[c][a:b] = np.nan
    return cols


def test_gap_rows_leave_the_fit_and_segments_start_after_the_gap():
    T, gap = 1600, (200, 330)
    cols = fake_cols(T, [gap, (700, 703)])
    segs = S.Identifier(S.CFG, FakeStore(T, [(400, 1200)], cols)).segments()
    assert len(segs) == 1 and segs[0]["lo"] == gap[1]
    s = segs[0]
    for k in "wqfp":
        assert np.isfinite(s[k]).all()
    assert s["f"].min() == 700.0 and s["p"].min() == 2500.0
    bad = np.arange(s["lo"], s["hi"])
    bad = (bad >= 700) & (bad < 703)
    assert not (s["fit"] & bad).any() and not (s["hold"] & bad).any() and not (s["all"] & bad).any() and s["all"].sum() == len(bad) - 3
    cols = fake_cols(T, [(500, 700)])
    segs = S.Identifier(S.CFG, FakeStore(T, [(400, 1200)], cols)).segments()
    assert [(s["lo"], s["hi"]) for s in segs] == [(100, 500), (700, 1600)]
    assert all(np.isfinite(s["w"]).all() for s in segs)


def test_restart_gap_before_the_fan_test_still_ships_with_the_right_effect():
    if STORE:
        a = run_case("fan_30.0_gap", 0, 1)
        assert a["ship"] and 1 / 1.5 < a["ratio"] < 1.5


def test_step_train_and_hold_do_not_overlap_and_the_fit_stays_clear_of_the_hold_rows():
    T = 6000
    st = FakeStore(T, [(400, 1200), (1500, 2500)], fake_cols(T, []))
    st.cols[S.CFG["phase_channel"]][1500:2300] = 1.0 + np.arange(800) // 100
    co = S.Corpus.__new__(S.Corpus)
    co.cfg = dict(S.CFG, window=dict(S.CFG["window"], embargo_s=100))
    rows = np.arange(T)
    b = co.step_blocks(st, rows)
    assert b["step_hold"].sum() > 400 and b["step_train"].sum() > 500
    assert not (b["step_hold"] & b["step_train"]).any()
    emb = 100
    first = np.where(b["step_hold"])[0].min()
    assert not b["step_train"][max(0, first - emb):first].any()
    for (lo, hold, hi) in st.spans():
        assert hold < hi and not b["step_train"][hold - emb:hi].any() and b["step_hold"][hold:hi].all()
    segs = S.Identifier(S.CFG, st).segments()
    assert segs and not any((s["fit"] & b["step_hold"][s["lo"]:s["hi"]]).any() for s in segs)


def plans(text):
    return [[[float(v) for v in step.split(",")] for step in re.findall(r"\[([\d., ]+)\]", m)] for m in re.findall(r"(?:fan|pump) = (\[\[.*\]\])", text)]


def test_runbook_table_and_plans_in_the_doc_equal_the_simulated_protocol():
    proto = [(b[0], b[1]) for b in SIM["inject"]["protocols"]["runbook"] if b[0] in ("fan", "pump")]
    duty_fan, duty_pump, main_fan, main_pump = (steps for _, steps in proto)
    text = doc("steptest.md")
    table = [(float(d), float(m) * 60.0, float(w)) for d, m, w in re.findall(r"^\| \d[^|]*\| (\d+) % \| (\d+) min \| (\d+) W", text, re.M)]
    assert table == [tuple(float(v) for v in step) for step in main_fan]
    step2, step4 = text.split("## 3.")[0].split("## 2.")[1], text.split("## 4.")[1].split("### 4a")[0]
    assert plans(step2) == [[s[:2] for s in duty_fan], [s[:2] for s in duty_pump]] == [[[0.0, 120.0]], [[15.0, 120.0]]]
    assert plans(step4) == [[list(map(float, s[:2])) for s in main_fan], [list(map(float, s[:2])) for s in main_pump]]
    loads = {s[2] for s in main_pump}
    assert loads == {200} and re.search(r"Load (\d+) W constant", text.split("### 4b")[1]).group(1) == "200"


def test_conductance_check_uses_the_lowest_held_running_level_and_can_fail():
    n = 700
    seg = {"who": "fan", "phase": np.ones(n), "duty": np.r_[np.zeros(120), np.full(580, 27.0)], "f": np.r_[np.zeros(120), np.full(580, 395.0)], "hold": np.zeros(n, bool)}
    ident = S.Identifier(S.CFG, None)
    ident.segs = [seg]
    ex = ident.excitation("fan")
    assert ex["levels_held"] == [0.0, 25.0] and abs(ex["level_rpm"]["25.0"] - 395.0) < 1e-9
    low = min(r for l, r in ex["level_rpm"].items() if float(l) > 0.0) / ident.gb.full
    for m, passes in ((1.6, True), (0.1, False)):
        o = {"fit_rmse_k": 0.01, "lr_m": 50.0, "lr_threshold": 3.84, "g_ratio_at_lowest": low ** m, "hold_fan": {"ahead": None}}
        th = np.zeros(11)
        th[0], th[2] = 12.0, m
        why = ident.fan(o, th, [], ex)["reasons"]
        assert ("conductance drop at the lowest level too small" in why) == (not passes), (m, why)


def test_gate_criteria_are_the_pre_registered_ones_and_the_text_says_so():
    g = S.CFG["gate"]["grey"]
    assert g["fit_rmse_max"] == 0.12 and g["c_range"] == [8.0, 16.0] and g["ahead"] == {"h": 120, "step": 10, "gain": 1.5} and "band" not in g and "hold_rmse_max" not in g
    text = " ".join(S.CFG["gate"]["text"])
    assert "0.12 K" in text and "8 to 16 kJ/K" in text and "1.5 times better" in text and "persistence" in text and "q0" in text


def test_q0_is_a_parameter_free_in_the_null_model_and_not_a_response_input():
    gb = S.GreyBox()
    i = gb.NAMES.index("q0")
    assert (gb.LO[i], gb.HI[i]) == (0.0, 120.0)
    n = 1800
    seg = {"w": np.full(n, 30.0), "q": np.full(n, 100.0), "f": np.full(n, 700.0), "p": np.full(n, 3000.0), "t": np.arange(n) / 3600.0}
    th = np.array(gb.X0)
    base = gb.simulate(th, seg)
    th2 = th.copy()
    th2[i] += 20.0
    assert gb.simulate(th2, seg)[-1] > base[-1]
    resp = S.Identifier(S.CFG, None).response(dict(zip(gb.NAMES, th.tolist())))
    assert "q0" not in resp
    assert {n.name for n in S.Response(resp).nodes()} == {"step.fr", "step.fm", "step.pr", "step.pf", "step.wt", "step.fp", "step.fx"}


def forecast_seg(m, q0, seed):
    n = 3000
    fan = np.concatenate([np.full(1200, 100.0), np.full(600, 50.0), np.full(600, 100.0), np.full(600, 60.0)])
    load = np.concatenate([np.full(1200, 200.0), np.full(600, 70.0), np.full(600, 200.0), np.full(600, 120.0)])
    c = dict(SIM["plant"], q0=q0, m=m)
    sim = Plant(c, True).run(fan, np.full(n, 19.8), load * 0.55, load * 0.45, np.random.default_rng(seed))
    hold = np.arange(n) >= 1800
    return {"w": sim["water"], "q": 0.96 * load * 0.55 + load * 0.45 + sim["pump_pow"], "f": sim["fan_rpm"], "p": sim["pump_rpm"], "t": np.arange(n) / 3600.0, "fit": ~hold, "hold": hold}


def test_grey_box_recovers_a_constant_unmodelled_heat_and_the_forecast_beats_the_references():
    gb = S.GreyBox()
    seg = forecast_seg(1.61, 65.0, 3)
    th, sse, n = gb.fit([seg])
    th0, _, _ = gb.fit([seg], {"m": 0.0}, th)
    d = dict(zip(gb.NAMES, th))
    assert abs(d["q0"] - 65.0) < 30.0 and 8.0 <= d["C"] <= 16.0, d
    full, per = gb.ahead(th, seg, 120, 10)
    null, _ = gb.ahead(th0, seg, 120, 10)
    assert len(full) == len(per) > 50
    rm = lambda v: float(np.sqrt(np.mean(v ** 2)))
    assert rm(full) < rm(per) and rm(null) >= 1.5 * rm(full), (rm(full), rm(null), rm(per))


def test_forecast_gate_and_heat_capacity_gate_refuse_what_they_should():
    ident = S.Identifier(S.CFG, None)
    ex = {"levels_held": [0.0, 25.0, 50.0, 100.0], "span": 100.0}
    th = np.zeros(11)
    th[0], th[2] = 12.0, 1.5
    base = {"fit_rmse_k": 0.05, "lr_m": 50.0, "lr_threshold": 3.84, "g_ratio_at_lowest": 0.3}
    ok = dict(base, hold_fan={"ahead": {"full": 0.3, "null": 0.6, "persist": 0.62, "n": 100}})
    assert ident.fan(ok, th, [], ex)["reasons"] == []
    for ahead, text in (({"full": 0.3, "null": 0.44, "persist": 0.62}, "times better than the no-fan refit"), ({"full": 0.3, "null": 0.6, "persist": 0.3}, "not better than persistence"), (None, "no hold-out")):
        assert any(text in r for r in ident.fan(dict(base, hold_fan={"ahead": ahead}), th, [], ex)["reasons"]), ahead
    for c in (7.9, 16.1):
        t = th.copy()
        t[0] = c
        assert any("heat capacity" in r for r in ident.fan(ok, t, [], ex)["reasons"])
    assert any("fit RMSE" in r for r in ident.fan(dict(ok, fit_rmse_k=0.121), th, [], ex)["reasons"])


if __name__ == "__main__":
    for f in (test_prep_fits_on_the_given_rows_and_exempts_free_columns, test_plant_bound_lifts_a_low_fan_duty_and_rpm_is_never_negative, test_grey_box_recovers_the_fan_exponent_and_rejects_none,
              test_response_term_matches_between_trainer_and_exporter, test_gate_ships_with_a_fan_response_refuses_without_and_repeats):
        f()
        print("ok", f.__name__)
