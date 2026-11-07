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
        Run { rig, world, plant, feed, rng: Noise(88172645463325252), fan_cal: ChannelConfig::defaults(ControlKind::Fan).calibration(), profile, t: 0 }
    }

    fn tick(&mut self, garbage: impl Fn(usize, f64) -> Option<f64>) -> f64 {
        let r = self.rig.sim.report();
        let s = Spec::FAN;
        let fan_rpm = self.fan_cal.rpm(if r[s.mode()] == 0 { be16(&r, s.preset()) as f64 / 100.0 } else { 90.0 });
        let power = self.profile.power(self.t);
        self.feed.set((power, fan_rpm));
        self.plant.borrow_mut().step(power, fan_rpm);
        self.rig.extra = vec![(CPU_POWER, power), (GPU_POWER, 0.0), (PUMP_POWER, 5.0)];
        let clean = ((self.plant.borrow().reading() + self.rng.gauss(SENSOR_NOISE)) * 100.0).round() / 100.0;
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

#[test]
fn owner_config_values_validate_and_hold_the_water_near_the_target_in_simulation() {
    let cfg_text = "water_plan_limit = 34.5\nweight_span_k = 1.0\nfan_start_water = 33.0\nfan_stop_water = 31.5\n";
    let cc: ControlConfig = toml::from_str(cfg_text).unwrap();
    assert!(cc.validate().is_ok());
    assert!((Planner::limit(&cc, 0.5) - 34.0).abs() < 1e-9 && (Planner::limit(&cc, 1.0) - 33.5).abs() < 1e-9 && (Planner::limit(&cc, 0.0) - 34.5).abs() < 1e-9);
    let set = |c: &mut Config| {
        c.control.water_plan_limit = 34.5;
        c.control.weight_span_k = 1.0;
        c.control.fan_start_water = 33.0;
        c.control.fan_stop_water = 31.5;
        c.control.plan_secs = 5.0;
    };
    let rig = Rig::new((true, true), 3.0, set);
    assert_eq!((rig.c.cfg.water_plan_limit, rig.c.cfg.fan_start_water, rig.c.cfg.fan_stop_water, rig.c.cfg.weight_span_k), (34.5, 33.0, 31.5, 1.0));
    let mut run = Run::new(PlantSpec::MEASURED, Profile::Constant(120.0), move |c| {
        set(c);
        c.control.correction.enabled = true;
    });
    let mut tail: Vec<f64> = vec![];
    let mut peak: f64 = 0.0;
    for t in 0..3 * 3600 {
        run.tick(no_garbage);
        peak = peak.max(run.plant.borrow().t1);
        if t >= 2 * 3600 {
            tail.push(run.plant.borrow().reading());
        }
    }
    let mean = tail.iter().sum::<f64>() / tail.len() as f64;
    println!("owner config 120 W constant: mean water in hour 3 {mean:.2}, peak {peak:.2}");
    assert!(peak < 35.5 && mean < 34.6 && run.rig.c.chans.iter().all(|c| c.fallback.is_none()));
}
