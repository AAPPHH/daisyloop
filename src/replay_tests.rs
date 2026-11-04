use super::*;
use crate::model::Golden;

#[derive(Default, Debug)]
struct Stats {
    all: Vec<Dec>,
    timeline: Vec<String>,
    actuator_calls: usize,
}

#[derive(Debug)]
struct Dec {
    warm: bool,
    feasible: bool,
    fan: f64,
    pump: f64,
    err: Option<[[f64; 2]; 3]>,
}

impl Stats {
    fn share(v: &[f64], lo: f64, hi: f64) -> [f64; 3] {
        let n = v.len().max(1) as f64;
        [v.iter().filter(|x| **x <= lo + 1.0).count() as f64 / n, v.iter().filter(|x| **x > lo + 1.0 && **x < hi - 1.0).count() as f64 / n, v.iter().filter(|x| **x >= hi - 1.0).count() as f64 / n]
    }
    fn changes(v: &[f64]) -> usize {
        v.windows(2).filter(|w| (w[1] - w[0]).abs() > 1.0).count()
    }
    fn flips(v: &[f64], lo: f64, hi: f64) -> usize {
        let ext: Vec<bool> = v.iter().filter(|x| **x <= lo + 1.0 || **x >= hi - 1.0).map(|x| *x >= hi - 1.0).collect();
        ext.windows(2).filter(|w| w[0] != w[1]).count()
    }
    fn report(&self, tag: &str, warm_only: bool, fan: (f64, f64), pump: (f64, f64)) -> String {
        let d: Vec<&Dec> = self.all.iter().filter(|d| d.warm || !warm_only).collect();
        let (fv, pv): (Vec<f64>, Vec<f64>) = (d.iter().map(|d| d.fan).collect(), d.iter().map(|d| d.pump).collect());
        let (sf, sp) = (Self::share(&fv, fan.0, fan.1), Self::share(&pv, pump.0, pump.1));
        let scored: Vec<[[f64; 2]; 3]> = d.iter().filter_map(|d| d.err).collect();
        let n = scored.len();
        let rmse = |i: usize, k: usize| (scored.iter().map(|e| e[i][k]).sum::<f64>() / n.max(1) as f64).sqrt();
        format!(
            "{tag}: decisions {}, feasible {}, fan target at floor/between/max {:.0}/{:.0}/{:.0} %, changes {} ({} floor-max flips), pump at floor/between/max {:.0}/{:.0}/{:.0} %, changes {}, RMSE at +120 s direct vs persistence water {:.3}/{:.3}, cpu {:.2}/{:.2}, gpu {:.2}/{:.2} (n {})",
            d.len(), d.iter().filter(|d| d.feasible).count(), sf[0] * 100.0, sf[1] * 100.0, sf[2] * 100.0, Self::changes(&fv), Self::flips(&fv, fan.0, fan.1), sp[0] * 100.0, sp[1] * 100.0, sp[2] * 100.0, Self::changes(&pv), rmse(0, 0), rmse(0, 1), rmse(1, 0), rmse(1, 1), rmse(2, 0), rmse(2, 1), n
        )
    }
}

fn replay(hysteresis: f64) -> (Stats, (f64, f64), (f64, f64)) {
    let g = Golden::load();
    let cfg = base_cfg(|c| {
        c.writes_enabled = false;
        c.control.plan_hysteresis_k = hysteresis;
    });
    let sim = Sim::new();
    let s = sim.clone();
    let factory: Factory = Box::new(move || {
        s.0.borrow_mut().opens += 1;
        Ok(Box::new(s.clone()) as Box<dyn D5Control>)
    });
    let mut c = Control::new(&cfg, Box::new(D5Actuator::new(factory, &cfg.control)), Ok(Box::new(g.model())));
    let ix = |name: &str| g.channels.iter().position(|x| x == name).unwrap();
    let (iw, ic, ig) = (ix("d5next/water/temp"), ix("/intelcpu/0/temperature/0"), ix("nvml/0/temp"));
    let h = cfg.control.horizon_secs as usize;
    let mut st = Stats::default();
    let at = |t: usize, i: usize| g.rows.get(t).and_then(|r| r[i]);
    for t in 0..g.rows.len() {
        let now = (t as i64 + 1) * 1000;
        let mut rows: Vec<Row> = g.channels.iter().zip(&g.rows[t]).filter_map(|(ch, v)| v.map(|v| Row::new("replay", ch.clone(), ch.clone(), if ch.ends_with("/rpm") { "rpm" } else if ch.ends_with("/duty") { "duty" } else { "other" }, "", v))).collect();
        for (ch, kind, v) in [("d5next/pump/duty", "duty", 19.8), ("d5next/pump/rpm", "rpm", 2570.0)] {
            if !g.channels.iter().any(|x| x == ch) {
                rows.push(Row::new("replay", ch, ch, kind, "", v));
            }
        }
        c.step(&mut rows, now, g.k0 + t as i64);
        let status = c.status(now);
        let Some(p) = status.predicted.filter(|p| p.age_ms == 0) else { continue };
        let ctl = |i: usize| status.controls[i].target_rpm;
        let (Some(f), Some(pu)) = (ctl(0), ctl(1)) else { continue };
        let known = [(iw, p.water), (ic, p.cpu), (ig, p.gpu)];
        let mut err = [[0.0; 2]; 3];
        let scored = known.iter().enumerate().all(|(_, (i, pred))| matches!((at(t, *i), at(t + h, *i), pred), (Some(_), Some(_), Some(_))));
        for (k, (i, pred)) in known.iter().enumerate().filter(|_| scored) {
            let (a, b, pr) = (at(t, *i).unwrap(), at(t + h, *i).unwrap(), pred.unwrap());
            err[k] = [(pr - b).powi(2), (a - b).powi(2)];
        }
        if status.model.warm && scored && st.all.len() % 10 == 0 {
            st.timeline.push(format!("t {:>5} s  water {:.2}  cpu {:.0}  fan target {:>5.0} (actual {:>5.0})  pump target {:>5.0}  predicted water +{h} s {:.2}  actual {:.2}  feasible {}", t, at(t, iw).unwrap(), at(t, ic).unwrap(), f, at(t, ix("d5next/fan/rpm")).unwrap_or(f64::NAN), pu, p.water.unwrap(), at(t + h, iw).unwrap(), p.feasible));
        }
        st.all.push(Dec { warm: status.model.warm, feasible: p.feasible, fan: f, pump: pu, err: scored.then_some(err) });
    }
    st.actuator_calls = sim.sets();
    let (fc, pc) = (&c.chans[0], &c.chans[1]);
    (st, fc.rpm_range(), pc.rpm_range())
}

#[test]
fn replay_of_a_recorded_window_through_features_plan_and_telemetry() {
    let (plain, fan, pump) = replay(0.0);
    let (hyst, _, _) = replay(ControlConfig::default().plan_hysteresis_k);
    for (tag, st) in [("no hysteresis", &plain), ("hysteresis 0.3 K", &hyst)] {
        for warm in [false, true] {
            println!("{}", st.report(&format!("{tag}, {}", if warm { "warm only" } else { "all decisions" }), warm, fan, pump));
        }
    }
    for l in &hyst.timeline {
        println!("{l}");
    }
    assert_eq!((plain.actuator_calls, hyst.actuator_calls), (0, 0));
    assert!(hyst.all.iter().filter(|d| d.warm).count() > 50);
}
