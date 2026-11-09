use super::*;
use crate::correct::Rls;

const COLD_W: f64 = 17.0;
const HORIZON: usize = 120;

struct Run {
    rig: Rig,
    world: Option<plan::XgbWorld>,
    plant: Rc<RefCell<Plant>>,
    feed: Rc<Cell<(f64, f64)>>,
    rng: Noise,
    noise: f64,
    fan_cal: Calibration,
    profile: Profile,
    t: usize,
}

impl Run {
    fn new(spec: PlantSpec, profile: Profile, tweak: impl Fn(&mut Config)) -> Run {
        let plant = Rc::new(RefCell::new(Plant::new(spec)));
        let feed = Rc::new(Cell::new((0.0, 0.0)));
        let (mut rig, mut world) = plan::rig(plan::Kind::Biased, &plant, &feed, tweak);
        plant.borrow_mut().retarget(30.0);
        rig.water = 30.0;
        for _ in 0..5 {
            plan::advance(&mut rig, &mut world, 0.0);
        }
        rig.active("d5.fan");
        rig.active("d5.pump");
        Run { rig, world, plant, feed, rng: Noise(88172645463325252), noise: SENSOR_NOISE, fan_cal: ChannelConfig::defaults(ControlKind::Fan).calibration(), profile, t: 0 }
    }

    fn tick(&mut self, garbage: impl Fn(usize, f64) -> Option<f64>) -> f64 {
        let r = self.rig.sim.report();
        let s = Spec::FAN;
        let fan_rpm = self.fan_cal.rpm(if r[s.mode()] == 0 { be16(&r, s.preset()) as f64 / 100.0 } else { 90.0 });
        let power = self.profile.power(self.t);
        self.feed.set((power, fan_rpm));
        self.plant.borrow_mut().step(power, fan_rpm);
        self.rig.extra = vec![(CPU_POWER, power), (GPU_POWER, 0.0), (PUMP_POWER, 5.0)];
        let clean = ((self.plant.borrow().reading() + self.rng.gauss(self.noise)) * 100.0).round() / 100.0;
        self.rig.water = garbage(self.t, clean).unwrap_or(clean);
        self.rig.cpu = self.rig.water + 15.0 + 0.1 * power;
        plan::advance(&mut self.rig, &mut self.world, fan_rpm);
        self.t += 1;
        fan_rpm
    }

    fn bounded(&self) -> bool {
        let c = &self.rig.c.cfg.correction;
        self.rig.c.corr.state().values().all(|r: &Rls| r.bounded(c))
    }
}

fn no_garbage(_: usize, _: f64) -> Option<f64> {
    None
}

fn cfg(c: &mut Config, on: bool) {
    c.control.plan_secs = 5.0;
    c.control.decision_secs = 30.0;
    c.control.correction.enabled = on;
}

fn biased_spec() -> PlantSpec {
    PlantSpec { cold_w: COLD_W, ..PlantSpec::MEASURED }
}

fn load() -> Profile {
    Profile::Square(60.0, 160.0, 900, 900)
}

fn rmse(v: &[f64]) -> f64 {
    (v.iter().map(|x| x * x).sum::<f64>() / v.len().max(1) as f64).sqrt()
}

fn scored(cold_w: f64) -> ((f64, f64, usize), (f64, f64, usize), Vec<(u32, Rls)>) {
    let mut run = Run::new(PlantSpec { cold_w, ..PlantSpec::MEASURED }, load(), |c| cfg(c, true));
    let mut samples: Vec<(usize, f64, f64)> = Vec::new();
    let (mut waters, mut fans): (Vec<f64>, Vec<f64>) = (vec![], vec![]);
    for t in 0..8 * 3600 {
        let fan = run.tick(no_garbage);
        waters.push(run.plant.borrow().reading());
        fans.push(fan);
        if run.rig.c.last_decision == Some(run.rig.now) {
            if let (Some(c), Some(r)) = (run.rig.c.shown, run.rig.c.shown_raw) {
                samples.push((t, c, r));
            }
        }
    }
    let profile = load();
    let held = |t: usize| (t..=t + HORIZON).all(|k| (fans[k] - fans[t]).abs() < 1.0 && profile.power(k) == profile.power(t));
    let score = |only_held: bool| {
        let (mut e_c, mut e_r) = (vec![], vec![]);
        for (t, c, r) in &samples {
            if *t >= 6 * 3600 && t + HORIZON < waters.len() && (!only_held || held(*t)) {
                e_c.push(c - waters[t + HORIZON]);
                e_r.push(r - waters[t + HORIZON]);
            }
        }
        (rmse(&e_c), rmse(&e_r), e_c.len())
    };
    assert!(run.bounded());
    (score(true), score(false), run.rig.c.corr.state().iter().map(|(h, s)| (*h, *s)).collect())
}

#[test]
fn a_biased_plant_corrected_120_s_water_rmse_over_hours_6_to_8_is_at_most_half_of_the_uncorrected_in_held_windows() {
    let ((c, r, n), (ca, ra, na), state) = scored(COLD_W);
    println!("A plant with {COLD_W} W extra heat below 1000 rpm, hours 6-8, windows with fan and load held 120 s: corrected {c:.4} uncorrected {r:.4} ratio {:.3} (n {n}); all windows incl. fan moves {ca:.4}/{ra:.4} (n {na})", c / r);
    for (h, s) in &state {
        println!("A state h {h}: bias {:.3} scale {:.3} n {} rejected {}", s.theta[0], 1.0 + s.theta[1], s.n, s.rejected);
    }
    assert!(n > 100 && c <= 0.5 * r, "corrected {c} uncorrected {r}");
}

#[test]
fn a_plant_without_bias_is_left_alone_by_the_correction() {
    let ((c, r, n), _, state) = scored(0.0);
    println!("A control, no bias: corrected {c:.4} uncorrected {r:.4} (n {n})");
    assert!(n > 100 && c <= r * 1.05 + 0.002, "{c} {r}");
    assert!(state.iter().all(|(_, s)| s.theta[0].abs() < 0.02 && s.theta[1].abs() < 0.02), "{state:?}");
}

#[test]
fn c_garbage_only_leaves_the_corrector_idle_and_the_planner_equal_to_the_uncorrected_one() {
    let garbage = |t: usize, clean: f64| match t % 600 {
        0..=199 => Some(f64::NAN),
        200..=399 => Some(33.3),
        _ => Some(if t / 5 % 2 == 0 { clean + 7.0 } else { clean }),
    };
    let profile = Profile::Square(60.0, 160.0, 300, 300);
    let (mut a, mut b) = (Run::new(PlantSpec::MEASURED, profile, |c| cfg(c, true)), Run::new(PlantSpec::MEASURED, profile, |c| cfg(c, false)));
    let mut compared = 0;
    for t in 0..2 * 3600 {
        a.tick(garbage);
        b.tick(garbage);
        let (pa, pb) = (a.rig.c.plan.map(|p| (p.0.fan_rpm, p.0.pump_rpm)), b.rig.c.plan.map(|p| (p.0.fan_rpm, p.0.pump_rpm)));
        let (da, db) = (a.rig.c.chans.iter().map(|c| c.duty_cmd).collect::<Vec<_>>(), b.rig.c.chans.iter().map(|c| c.duty_cmd).collect::<Vec<_>>());
        assert!(pa == pb && da == db && a.bounded(), "t {t}: {pa:?} {pb:?} {da:?} {db:?}");
        compared += pa.is_some() as usize;
    }
    assert!(compared > 500);
    assert!(a.rig.c.corr.state().values().all(|r| r.n == 0 && r.theta == [0.0, 0.0]));
}

#[test]
fn c_after_a_clean_learning_phase_garbage_keeps_bias_and_scale_inside_their_bounds_and_the_plant_safe() {
    let garbage = |t: usize, clean: f64| {
        if t < 3 * 3600 {
            return None;
        }
        match (t - 3 * 3600) % 900 {
            0..=119 => Some(f64::NAN),
            120..=299 => Some(clean + if t % 11 < 5 { 9.0 } else { -9.0 }),
            300..=599 => Some(34.0),
            _ => Some(if t % 3 == 0 { f64::NAN } else { clean + 2.0 }),
        }
    };
    let mut a = Run::new(biased_spec(), load(), |c| cfg(c, true));
    let mut peak: f64 = 0.0;
    for _ in 0..5 * 3600 {
        a.tick(garbage);
        peak = peak.max(a.plant.borrow().t1);
        assert!(a.bounded(), "t {} {:?}", a.t, a.rig.c.corr.state());
    }
    let learned = a.rig.c.corr.state().get(&120).copied().unwrap();
    assert!(learned.n > 200 && learned.theta[0].abs() <= 1.0 && (0.5..=1.5).contains(&(1.0 + learned.theta[1])));
    assert!(peak < a.rig.c.cfg.water_limit);
}

const OWNER_NOISE: f64 = 0.01;

fn owner_set(c: &mut Config, target: f64) {
    (c.control.water_target, c.control.fan_start_water, c.control.fan_stop_water) = (Some(target), Some(33.0), Some(31.5));
    (c.control.water_plan_limit, c.control.weight_span_k) = (Some(34.5), Some(1.0));
    c.control.plan_secs = 5.0;
}

struct Owner {
    writes: usize,
    swings: usize,
    late: usize,
    changes: usize,
    mean: f64,
    peak: f64,
    fallback: bool,
}

fn owner_day(spec: PlantSpec, profile: Profile, on: bool, target: f64) -> Owner {
    let mut run = Run::new(spec, profile, move |c| {
        owner_set(c, target);
        c.control.correction.enabled = on;
    });
    run.noise = OWNER_NOISE;
    let (mut tail, mut peak) = (vec![], 0.0f64);
    let (mut last, mut dir, mut swings, mut late, mut changes) = (None::<f64>, 0.0f64, 0usize, 0usize, 0usize);
    for t in 0..24 * 3600 {
        changes += (t >= 12 * 3600 && profile.power(t) != profile.power(t - 1)) as usize;
        run.tick(no_garbage);
        peak = peak.max(run.plant.borrow().t1);
        if t >= 12 * 3600 {
            tail.push(run.plant.borrow().reading());
        }
        if let Some(d) = run.rig.c.chans[0].duty_cmd {
            match last {
                Some(l) if (d - l).abs() >= 10.0 => {
                    if dir != 0.0 && (d - l).signum() != dir {
                        swings += 1;
                        if t >= 12 * 3600 {
                            late += 1;
                        }
                    }
                    dir = (d - l).signum();
                    last = Some(d);
                }
                None => last = Some(d),
                _ => {}
            }
        }
    }
    Owner { writes: run.rig.sim.duty_sets().len(), swings, late, changes, mean: tail.iter().sum::<f64>() / tail.len() as f64, peak, fallback: run.rig.c.chans.iter().any(|c| c.fallback.is_some()) }
}

#[test]
fn the_planner_holds_the_water_near_the_target_without_chatter_in_simulation() {
    let cc: ControlConfig = toml::from_str("water_target = 34.0\n").unwrap();
    assert!(cc.validate().is_ok());
    let rig = Rig::new((true, true), 3.0, |c| owner_set(c, 34.0));
    assert_eq!((rig.c.cfg.target, rig.c.cfg.fan_start(), rig.c.cfg.fan_stop()), (34.0, 33.0, 31.5));
    let legacy: ControlConfig = toml::from_str(&["water_plan_limit = 34.5", "weight_span_k = 1.0", "fan_start_water = 33.0", "fan_stop_water = 31.5"].join("\n")).unwrap();
    assert_eq!(legacy.default_target(), 34.0);
    let defaults = ControlConfig::default();
    assert_eq!((defaults.target_min, defaults.target_max), (32.0, 36.0));
    for target in [32.0, 33.0, 34.0, 35.0, 36.0] {
        let mut loads = vec![("60 W", PlantSpec::MEASURED, Profile::Constant(60.0)), ("120 W", PlantSpec::MEASURED, Profile::Constant(120.0)), ("square 60/160 W", PlantSpec::MEASURED, Profile::Square(60.0, 160.0, 900, 900))];
        if target <= 34.0 {
            loads.push(("17 W bias, 120 W", biased_spec(), Profile::Constant(120.0)));
        }
        for (name, spec, profile) in loads {
            for on in [false, true] {
                let o = owner_day(spec, profile, on, target);
                println!("target {target} {name} correction {on}: writes {} fan swings {} late swings {} load changes {} mean water second half {:.2} peak {:.2}", o.writes, o.swings, o.late, o.changes, o.mean, o.peak);
                assert!(o.writes <= 250 && o.late <= o.changes + 2 && o.peak <= target + 0.5 && !o.fallback, "{target} {name} {on}");
                if matches!(profile, Profile::Constant(_)) {
                    assert!((target - 0.7..=target + 0.2).contains(&o.mean), "{target} {name} {on} mean {}", o.mean);
                }
            }
        }
    }
}

struct History {
    channels: Vec<String>,
    data: Vec<f32>,
    k0: i64,
    rows: usize,
}

impl History {
    fn load(dir: &str) -> History {
        let meta: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(format!("{dir}/meta.json")).unwrap()).unwrap();
        let channels: Vec<String> = meta["channels"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_string()).collect();
        let bytes = std::fs::read(format!("{dir}/data.f32")).unwrap();
        let data: Vec<f32> = bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        let rows = meta["rows"].as_u64().unwrap() as usize;
        assert_eq!(data.len(), rows * channels.len());
        History { channels, data, k0: meta["k0"].as_i64().unwrap(), rows }
    }
    fn row(&self, t: usize) -> &[f32] {
        let n = self.channels.len();
        &self.data[t * n..(t + 1) * n]
    }
    fn col(&self, name: &str) -> usize {
        self.channels.iter().position(|c| c == name).unwrap()
    }
}

#[derive(Default)]
struct Bins {
    corrected: Vec<f64>,
    raw: Vec<f64>,
}

impl Bins {
    fn add(&mut self, c: f64, r: f64) {
        self.corrected.push(c);
        self.raw.push(r);
    }
    fn line(&self, name: &str) -> String {
        let (c, r) = (rmse(&self.corrected), rmse(&self.raw));
        format!("{name}: n {} corrected {c:.4} uncorrected {r:.4} change {:+.1} %", self.raw.len(), (c / r - 1.0) * 100.0)
    }
}

#[test]
#[ignore]
fn b_replay_of_the_real_history_through_the_deployed_model_and_the_correction() {
    use crate::correct::Corrector;
    use crate::model::XgbModel;
    let h = History::load(&std::env::var("DL_HIST").expect("DL_HIST"));
    let models = vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models")];
    let mut model = XgbModel::load(&models, "feature_spec_deploy.json", "model_deploy.json").unwrap();
    let cfg = crate::correct::CorrectionConfig::default();
    let dir = std::env::temp_dir().join(format!("daisyloop-replay-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut corr = Corrector::new(&cfg, &dir, model.digest());
    let inputs = model.inputs();
    let ix: Vec<usize> = inputs.iter().map(|c| h.col(c)).collect();
    let (iw, iff, ipr, ipp, icpu, igpu, iph) = (h.col("d5next/water/temp"), h.col("d5next/fan/rpm"), h.col("d5next/pump/rpm"), h.col("d5next/pump/power"), h.col("/intelcpu/0/power/0"), h.col("nvml/0/power"), h.col("daisyloop/step/phase"));
    let fin = |v: f32| v.is_finite();
    let mut last_seen = vec![i64::MIN / 2; ix.len()];
    let mut last_data = i64::MIN / 2;
    let mut contiguous_since = 0i64;
    let mut scored: Vec<(usize, f64, f64, f64, bool)> = Vec::new();
    let half = h.rows / 2;
    let mut updates_first_half = 0u32;
    let fb_ix: Vec<usize> = model.fallback_inputs().iter().filter_map(|c| inputs.iter().position(|i| i == c)).collect();
    let repeat: i64 = std::env::var("DL_REPEAT").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut counts = [0usize; 8];
    let mut drift_first: Option<usize> = None;
    let mut stale_by = vec![0usize; ix.len()];
    for t in 0..h.rows {
        if t == half {
            updates_first_half = corr.state().get(&120).map_or(0, |r| r.n);
        }
        let row = h.row(t);
        let epoch = h.k0 + t as i64;
        if !row.iter().enumerate().any(|(i, v)| i != iph && fin(*v)) {
            continue;
        }
        if epoch - last_data > 10 {
            contiguous_since = epoch;
        }
        last_data = epoch;
        let map: HashMap<String, f64> = h.channels.iter().zip(row).filter(|(c, v)| fin(**v) && c.as_str() != "daisyloop/step/phase" && (c.as_str() != "d5next/water/temp" || (5.0..=80.0).contains(&(**v as f64)))).map(|(c, v)| (c.clone(), *v as f64)).collect();
        model.observe(&map, epoch);
        for (k, c) in inputs.iter().enumerate() {
            if map.contains_key(c) {
                last_seen[k] = epoch;
            }
        }
        let water = map.get("d5next/water/temp").copied();
        let live = model.live();
        let step = fin(row[iph]);
        let stale = last_seen.iter().any(|s| epoch - s > 10);
        let ok = water.is_some() && !stale && !step && epoch - contiguous_since >= cfg.restart_hold_secs && map.contains_key("d5next/fan/rpm") && map.contains_key("d5next/pump/rpm");
        counts[0] += 1;
        counts[1] += water.is_some() as usize;
        counts[2] += (!live.is_empty()) as usize;
        counts[3] += stale as usize;
        counts[4] += step as usize;
        counts[5] += ok as usize;
        counts[6] += (ok && !live.is_empty()) as usize;
        for (k, s) in last_seen.iter().enumerate() {
            stale_by[k] += (epoch - s > 10) as usize;
        }
        counts[7] += fb_ix.iter().any(|k| epoch - last_seen[*k] > 10) as usize;
        let watts: f64 = [icpu, igpu, ipp].iter().filter(|i| fin(row[**i])).map(|i| (row[*i] as f64).max(0.0)).sum();
        let rpm = |i: usize| if fin(row[i]) { row[i] as f64 } else { f64::NAN };
        if let (Some(l), Some(w)) = (live.iter().find(|l| l.h == 120), water) {
            if t + HORIZON < h.rows && fin(h.row(t + HORIZON)[iw]) && fin(row[iff]) {
                let actual = h.row(t + HORIZON)[iw] as f64 - w;
                let corrected = l.pred + corr.delta(120, l.response);
                scored.push((t, corrected - actual, l.pred - actual, row[iff] as f64, step));
            }
        }
        corr.feed(epoch, ok, water, [rpm(iff), rpm(ipr), watts], &live);
        if drift_first.is_none() && !corr.drifting().is_empty() {
            drift_first = Some(t);
        }
        if repeat > 0 && epoch % repeat == 0 {
            corr.feed(epoch, ok, water, [rpm(iff), rpm(ipr), watts], &live);
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    let edges = [0.0, 600.0, 1000.0, 1400.0, f64::INFINITY];
    let mut later = Bins::default();
    let mut later_nostep = Bins::default();
    let mut bins: Vec<Bins> = (0..4).map(|_| Bins::default()).collect();
    let mut first = Bins::default();
    for (t, c, r, fan, step) in &scored {
        if *t < half {
            first.add(*c, *r);
            continue;
        }
        later.add(*c, *r);
        if !step {
            later_nostep.add(*c, *r);
        }
        let b = (0..4).find(|b| *fan >= edges[*b] && *fan < edges[*b + 1]).unwrap();
        bins[b].add(*c, *r);
    }
    println!("B seconds: with data, water, live, stale (any input), step, ok, ok and live, hold (fallback input stale) = {counts:?}, hold share {:.2} %, repeat {repeat}", 100.0 * counts[7] as f64 / counts[0] as f64);
    println!("B stale seconds per input {:?}, fallback inputs {:?}", inputs.iter().zip(&stale_by).collect::<Vec<_>>(), model.fallback_inputs());
    println!("B drift alarm first at row {drift_first:?} of {}, horizons drifting at the end {:?}", h.rows, corr.drifting());
    println!("B history rows {} (k0 {}), scored samples {}, updates at h120 by the half {updates_first_half}", h.rows, h.k0, scored.len());
    println!("B first half {}", first.line("first half (learning)"));
    println!("B {}", later.line("later half overall"));
    println!("B {}", later_nostep.line("later half without step-test seconds"));
    for (b, name) in ["fan 0-600", "fan 600-1000", "fan 1000-1400", "fan 1400+"].iter().enumerate() {
        println!("B {}", bins[b].line(name));
    }
    for (hh, s) in corr.state() {
        println!("B final state h {hh}: bias {:.3} scale {:.3} n {} rejected {}", s.theta[0], 1.0 + s.theta[1], s.n, s.rejected);
    }
}
