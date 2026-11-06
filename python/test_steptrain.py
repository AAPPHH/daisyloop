import copy
import os
import shutil
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
    cfg = dict(S.CFG, store_dir=S.FRESH["store_dir"], spec=S.CFG["deployed_spec"], threads=threads)
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


if __name__ == "__main__":
    for f in (test_prep_fits_on_the_given_rows_and_exempts_free_columns, test_plant_bound_lifts_a_low_fan_duty_and_rpm_is_never_negative, test_grey_box_recovers_the_fan_exponent_and_rejects_none,
              test_response_term_matches_between_trainer_and_exporter, test_gate_ships_with_a_fan_response_refuses_without_and_repeats):
        f()
        print("ok", f.__name__)
