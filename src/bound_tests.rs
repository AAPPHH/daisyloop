use super::*;
use crate::model::Forecast;
use std::cell::RefCell;
use std::collections::VecDeque;

struct Blind;

impl Model for Blind {
    fn observe(&mut self, _: &HashMap<String, f64>, _: i64) {}
    fn identified(&self) -> (bool, bool) {
        (true, true)
    }
    fn warm_secs(&self) -> i64 {
        5000
    }
    fn forecast(&self, _: &HashMap<String, f64>, _: u32, _: u32) -> Result<Forecast, String> {
        Ok(Forecast::plain(0.0))
    }
}

fn blind_rig(tweak: impl Fn(&mut Config)) -> Rig {
    rig_with(Box::new(Blind), tweak)
}

pub(super) fn rig_with(model: Box<dyn Model>, tweak: impl Fn(&mut Config)) -> Rig {
    let cfg = base_cfg(tweak);
    let sim = Sim::new();
    let s = sim.clone();
    let factory: Factory = Box::new(move || Ok(Box::new(s.clone()) as Box<dyn D5Control>));
    let c = Control::new(&cfg, Box::new(D5Actuator::new(factory, &cfg.control)), Ok(model));
    Rig { sim, c, now: 0, water: 30.0, cpu: 50.0, gpu: Some(40.0), model_fail: Rc::new(Cell::new(false)), warm: Rc::new(Cell::new(0)), heat: Rc::new(Cell::new(0.0)), extra: vec![], live: false, dir: cfg.state_dir() }
}

fn running(water: f64, tweak: impl Fn(&mut Config)) -> Rig {
    let mut rig = Rig::new((true, true), 3.0, tweak);
    rig.water = water;
    rig.run(5);
    rig.active("d5.fan");
    rig.run(480);
    rig
}

fn duty(rig: &Rig, i: usize) -> Option<f64> {
    rig.status().controls[i].commanded_duty
}

fn bound(rig: &Rig, i: usize) -> f64 {
    rig.c.bound_duty(i, rig.now).unwrap()
}

#[test]
fn r1_the_curves_are_piecewise_linear_on_water_and_must_rise() {
    let mut r = Rig::new((false, false), 3.0, |_| {});
    for (w, fan, pump) in [(30.0, 0.0, 15.0), (34.9, 0.0, 15.0), (35.0, 20.0, 15.0), (35.5, 20.0 + 0.5 / 3.0 * 34.4, 57.5), (36.0, 20.0 + 1.0 / 3.0 * 34.4, 100.0), (38.0, 54.4, 100.0), (40.0, 54.4 + 2.0 / 4.0 * 45.6, 100.0), (42.0, 100.0, 100.0), (47.0, 100.0, 100.0)] {
        r.water = w;
        r.run(12);
        assert!((r.c.curve_bound(0, r.now).unwrap() - fan).abs() < 1e-6 && (r.c.curve_bound(1, r.now).unwrap() - pump).abs() < 1e-6, "{w}");
    }
    let fan = ChannelConfig::defaults(ControlKind::Fan).calibration();
    assert!((fan.rpm(54.4) - 800.0).abs() < 1.0);
    for bad in ["[fan]\nmin_curve = [[35.0, 50.0], [40.0, 20.0]]", "[pump]\nmin_curve = [[36.0, 15.0], [35.0, 100.0]]", "[fan]\nmin_curve = [[35.0, 20.0]]", "[pump]\nmin_curve = [[35.0, 15.0], [36.0, 101.0]]", "[fan]\nmin_run_secs = -1.0"] {
        assert!(toml::from_str::<ControlConfig>(bad).unwrap().validate().is_err(), "{bad}");
    }
    assert!(toml::from_str::<ControlConfig>("[fan]\nmin_curve = [[34.0, 20.0], [44.0, 100.0]]").unwrap().validate().is_ok());
}

#[test]
fn r2_a_missing_row_cannot_block_the_restart_and_the_bound_overrides_the_rate_limit() {
    let mut rig = running(32.0, |c| c.control.fan.step_up_rpm = Some(50.0));
    assert_eq!(duty(&rig, 0), Some(0.0));
    rig.sim.0.borrow_mut().force = [Some(f64::NAN), None];
    rig.run(12);
    assert!(rig.c.operating(&rig.c.chans[0], rig.now).is_none());
    rig.water = 41.0;
    rig.run(3);
    let want = bound(&rig, 0);
    assert!(want > 85.0);
    assert_eq!((duty(&rig, 0), rig.sim.preset(Spec::FAN)), (Some(want), (want * 100.0).round() as u16));
    assert_eq!(rig.ctl("d5.fan").target_rpm, Some(rig.c.chans[0].cal.rpm(want)));
    rig.water = 44.0;
    rig.run(8);
    assert_eq!(duty(&rig, 0), Some(100.0));
    assert_eq!(rig.sim.mode(Spec::FAN), 0);
}

#[test]
fn r3_the_pump_bound_follows_water_and_a_failed_model_hands_over_to_the_firmware() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.water = 30.0;
    rig.run(5);
    rig.active("d5.pump");
    rig.run(400);
    assert!((duty(&rig, 1).unwrap() - 15.0).abs() < 1e-6);
    rig.water = 36.5;
    rig.run(3);
    assert_eq!(duty(&rig, 1), Some(100.0));
    rig.water = 30.0;
    rig.run(1);
    rig.model_fail.set(true);
    rig.run(40);
    assert_eq!(rig.sim.mode(Spec::PUMP), 1);
    assert!(rig.ctl("d5.pump").fallback.unwrap().contains("Modellfehler"));
}

#[test]
fn r4_shadow_values_show_the_bounded_target_and_nothing_is_sent() {
    let mut rig = Rig::new((false, false), 3.0, |_| {});
    rig.water = 40.0;
    rig.run(70);
    let (fan, pump) = (rig.ctl("d5.fan"), rig.ctl("d5.pump"));
    assert!(fan.target_rpm.unwrap() >= rig.c.chans[0].cal.rpm(bound(&rig, 0)) - 1e-6);
    assert_eq!(pump.target_rpm, Some(4800.0));
    assert_eq!(rig.sim.sets(), 0);
    rig.sim.0.borrow_mut().force = [Some(f64::NAN), None];
    rig.run(70);
    assert_eq!(rig.ctl("d5.fan").target_rpm, Some(rig.c.chans[0].cal.rpm(bound(&rig, 0))));
}

#[test]
fn r5_a_restarted_fan_runs_at_least_the_minimum_time_and_the_restart_itself_is_never_delayed() {
    let mut rig = running(32.0, |c| c.control.bound_hold_secs = 60.0);
    rig.live = true;
    rig.water = 36.0;
    rig.run(3);
    assert!(duty(&rig, 0).unwrap() >= 20.0);
    rig.water = 30.0;
    rig.run(560);
    assert!(duty(&rig, 0).unwrap() >= 20.0, "{:?}", duty(&rig, 0));
    rig.run(100);
    assert_eq!(duty(&rig, 0), Some(0.0));
    let mut long = running(32.0, |c| c.control.fan.min_run_secs = Some(1.0e6));
    long.live = true;
    assert_eq!(duty(&long, 0), Some(0.0));
    long.water = 36.0;
    long.run(2);
    assert!(duty(&long, 0).unwrap() >= 20.0);
    long.water = 30.0;
    long.run(1500);
    assert!(duty(&long, 0).unwrap() >= 20.0);
    long.water = 36.0;
    long.run(40);
    assert!(duty(&long, 0).unwrap() >= bound(&long, 0) - 2.0);
}

#[test]
fn r6_bound_writes_ignore_an_exhausted_budget_and_do_not_postpone_the_planner() {
    let mut rig = running(32.0, |_| {});
    rig.c.chans[0].used = rig.c.chans[0].cfg.daily_writes;
    rig.water = 36.0;
    rig.run(2);
    assert!(duty(&rig, 0).unwrap() >= 20.0);
    rig.water = 40.0;
    rig.run(7);
    assert!(duty(&rig, 0).unwrap() >= bound(&rig, 0) - 2.0 - 1e-9, "{:?} {}", duty(&rig, 0), bound(&rig, 0));
    rig.water = 44.0;
    rig.run(7);
    assert_eq!(duty(&rig, 0), Some(100.0));
    let before = rig.c.last_write;
    rig.water = 45.0;
    rig.run(7);
    assert_eq!(rig.c.last_write, before);
}

#[test]
fn r7_frozen_water_with_moving_load_and_a_stopped_fan_runs_both_channels_at_full_duty() {
    let mut rig = running(32.0, |_| {});
    rig.live = false;
    for t in 0..700 {
        rig.cpu = if t / 10 % 2 == 0 { 40.0 } else { 70.0 };
        rig.tick();
    }
    assert_eq!((rig.sim.mode(Spec::FAN), duty(&rig, 0), rig.ctl("d5.fan").fallback), (0, Some(100.0), None));
    assert!(rig.ctl("d5.fan").fallback.is_none() && rig.alarm(AlarmKind::WaterFrozen) && !rig.alarm(AlarmKind::Fallback));
    let mut off = Rig::new((true, true), 3.0, |c| c.writes_enabled = false);
    for t in 0..700 {
        off.cpu = if t / 10 % 2 == 0 { 40.0 } else { 70.0 };
        off.tick();
    }
    assert!(off.alarm(AlarmKind::WaterFrozen) && off.sim.sets() == 0);
}

#[test]
fn r8_only_the_water_plan_limit_exists_and_cpu_gpu_excess_changes_nothing() {
    let cfg = ControlConfig::default();
    assert_eq!((Planner::limit(&cfg, 0.0), Planner::limit(&cfg, 1.0)), (46.0, 41.0));
    let mut rig = Rig::new((false, false), 3.0, |_| {});
    rig.run(120);
    let quiet_fan = rig.ctl("d5.fan").target_rpm.unwrap();
    rig.cpu = 150.0;
    rig.gpu = Some(150.0);
    rig.run(150);
    let p = rig.status().predicted.unwrap();
    assert!(p.feasible && rig.ctl("d5.fan").target_rpm.unwrap() == quiet_fan);
    assert!(rig.status().alarms.is_empty());
}

#[test]
fn r9_a_stale_channel_the_model_does_not_weight_changes_nothing_and_only_stale_water_takes_over() {
    let mut rig = running(34.0, |_| {});
    let held = duty(&rig, 0);
    rig.gpu = None;
    rig.cpu = f64::NAN;
    rig.run(60);
    let c = rig.ctl("d5.fan");
    assert!(rig.sim.mode(Spec::FAN) == 0 && c.fallback.is_none() && !rig.alarm(AlarmKind::Stale));
    assert!(duty(&rig, 0) == held);
    rig.water = 44.0;
    rig.run(8);
    assert_eq!((duty(&rig, 0), rig.sim.mode(Spec::FAN)), (Some(100.0), 0));
    rig.water = f64::NAN;
    rig.run(14);
    assert_eq!(rig.sim.mode(Spec::FAN), 1);
    assert!(rig.ctl("d5.fan").fallback.unwrap().contains("Wasser"));
}

#[test]
fn r10_a_pump_at_its_floor_does_not_fall_back_from_its_own_floor_and_pump_low_stays() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.live = true;
    rig.water = 30.0;
    rig.run(5);
    rig.active("d5.pump");
    rig.run(400);
    assert!((duty(&rig, 1).unwrap() - 15.0).abs() < 1e-6);
    rig.sim.0.borrow_mut().force = [None, Some(1835.0)];
    rig.run(300);
    assert!(rig.ctl("d5.pump").fallback.is_none() && rig.sim.mode(Spec::PUMP) == 0 && !rig.alarm(AlarmKind::PumpLow));
    rig.sim.0.borrow_mut().force = [None, Some(1790.0)];
    rig.run(10);
    assert!(rig.alarm(AlarmKind::PumpLow));
    assert!(ChannelConfig::defaults(ControlKind::Pump).rpm_tolerance * (4800.0 - 2432.0) > 2432.0 - 1800.0);
}

#[test]
fn r11_water_limit_above_the_plausibility_cap_is_rejected_by_name() {
    for (bad, key) in [("water_limit = 90.0\nwater_top_limit = 95.0", "water_limit"), ("water_top_limit = 85.0", "water_top_limit")] {
        let e = toml::from_str::<ControlConfig>(bad).unwrap().validate().unwrap_err();
        assert!(e.contains(key) && e.contains("80"), "{e}");
    }
    assert!(toml::from_str::<ControlConfig>("bound_raise_cap = 0").unwrap().validate().unwrap_err().contains("bound_raise_cap"));
    assert!(toml::from_str::<ControlConfig>("water_limit = 80.0\nwater_top_limit = 80.0").unwrap().validate().is_ok());
    let rig = Rig::new((false, false), 3.0, |c| c.control.water_limit = 90.0);
    assert_eq!(rig.c.cfg.water_limit, 48.0);
}

#[derive(Clone, Copy)]
struct PlantSpec {
    ambient: f64,
    p0: f64,
    g_stop: f64,
    exponent: f64,
    c_scale: f64,
    g_full: f64,
    cold_w: f64,
}

impl PlantSpec {
    const MEASURED: PlantSpec = PlantSpec { ambient: 25.0, p0: 0.0, g_stop: 0.5, exponent: 1.61, c_scale: 1.0, g_full: 24.0, cold_w: 0.0 };
    const ALTERNATIVE: PlantSpec = PlantSpec { ambient: 20.0, p0: 119.0, g_stop: 5.0, exponent: 0.865, c_scale: 1.0, g_full: 24.0, cold_w: 0.0 };
    const NAMED: [(&'static str, PlantSpec); 2] = [("measured", Self::MEASURED), ("alternative", Self::ALTERNATIVE)];
}

#[derive(Clone)]
struct Plant {
    spec: PlantSpec,
    t1: f64,
    t2: f64,
    seen: VecDeque<f64>,
}

impl Plant {
    const C1: f64 = 7100.0;
    const C2: f64 = 4600.0;
    const K: f64 = 24.0;
    const DELAY: usize = 31;

    fn new(spec: PlantSpec) -> Plant {
        Plant { spec, t1: 30.0, t2: 30.0, seen: VecDeque::from(vec![30.0]) }
    }
    fn retarget(&mut self, t: f64) {
        (self.t1, self.t2) = (t, t);
        self.seen = VecDeque::from(vec![t]);
    }
    fn reading(&self) -> f64 {
        self.seen[0]
    }
    fn step(&mut self, power: f64, fan_rpm: f64) -> f64 {
        let g = self.spec.g_stop + (self.spec.g_full - self.spec.g_stop) * (fan_rpm / 1470.0).powf(self.spec.exponent);
        let flow = Self::K * (self.t1 - self.t2);
        self.t1 += (power + self.spec.p0 + if fan_rpm < 1000.0 { self.spec.cold_w } else { 0.0 } - g * (self.t1 - self.spec.ambient) - flow) / (Self::C1 * self.spec.c_scale);
        self.t2 += flow / (Self::C2 * self.spec.c_scale);
        self.seen.push_back(self.t1);
        if self.seen.len() > Self::DELAY + 1 {
            self.seen.pop_front();
        }
        (self.seen[0] * 100.0).round() / 100.0
    }
}

#[derive(Clone, Copy, Debug)]
enum Profile {
    Constant(f64),
    Square(f64, f64, usize, usize),
    Sine(f64),
    Golden,
    Once(f64, usize, f64),
    Step(f64, f64, usize),
}

impl Profile {
    const SECS: usize = 86_400;
    const STEP_TAIL: usize = 1800;
    fn power(&self, t: usize) -> f64 {
        match *self {
            Profile::Constant(w) => w,
            Profile::Square(lo, hi, on, off) => if t % (on + off) >= off { hi } else { lo },
            Profile::Once(hi, secs, lo) => if t < secs { hi } else { lo },
            Profile::Step(lo, hi, at) => if t < at { lo } else { hi },
            Profile::Sine(_) => 100.0,
            Profile::Golden => 0.0,
        }
    }
    fn closed(&self) -> bool {
        matches!(self, Profile::Constant(_) | Profile::Square(..) | Profile::Step(..))
    }
    fn secs(&self) -> usize {
        if let Profile::Step(_, _, at) = *self { at + Self::STEP_TAIL } else { Self::SECS }
    }
    const IDLE_W: f64 = 20.0;
    fn load_end(&self) -> usize {
        if let Profile::Once(_, secs, _) = *self { secs } else { 0 }
    }
    fn recurring() -> Profile {
        Profile::Square(Self::IDLE_W, 400.0, 300, 7200)
    }
    fn grid() -> Vec<Profile> {
        let mut v = vec![];
        for lo in [30.0, 45.0, 60.0] {
            for hi in [80.0, 120.0, 200.0] {
                for on in [3, 10, 30] {
                    for off in [5, 20, 60, 125, 135, 180] {
                        v.push(Profile::Square(lo, hi, on * 60, off * 60));
                    }
                }
            }
        }
        v
    }
    fn name(&self) -> String {
        match self {
            Profile::Constant(w) => format!("const {w:.0} W"),
            Profile::Square(lo, hi, on, off) => format!("square {lo:.0}/{hi:.0} W {}+{} min", on / 60, off / 60),
            Profile::Sine(c) => format!("sine {c:.1} C"),
            Profile::Golden => "golden".into(),
            Profile::Once(hi, secs, lo) => format!("once {hi:.0} W {} min then {lo:.0} W", secs / 60),
            Profile::Step(lo, hi, at) => format!("step {lo:.0} to {hi:.0} W after {} min", at / 60),
        }
    }
}

struct Trace {
    channels: Vec<String>,
    rows: Vec<Vec<f64>>,
}

impl Trace {
    fn golden() -> Trace {
        let g = crate::model::Golden::load();
        let mut rows = vec![vec![f64::NAN; g.channels.len()]; g.rows.len()];
        for c in 0..g.channels.len() {
            let known: Vec<usize> = (0..g.rows.len()).filter(|t| g.rows[*t][c].is_some()).collect();
            for t in 0..g.rows.len() {
                let (a, b) = (known.iter().rev().find(|k| **k <= t), known.iter().find(|k| **k >= t));
                rows[t][c] = match (a, b) {
                    (Some(a), Some(b)) if a != b => g.rows[*a][c].unwrap() + (g.rows[*b][c].unwrap() - g.rows[*a][c].unwrap()) * (t - a) as f64 / (b - a) as f64,
                    (Some(a), _) => g.rows[*a][c].unwrap(),
                    (None, Some(b)) => g.rows[*b][c].unwrap(),
                    _ => f64::NAN,
                };
            }
        }
        let w = g.channels.iter().position(|c| c == "d5next/water/temp").unwrap();
        let known: Vec<usize> = (0..g.rows.len()).filter(|t| g.rows[*t][w].is_some()).collect();
        let rows = rows[known[0]..=known[known.len() - 1]].to_vec();
        Trace { channels: g.channels, rows }
    }
    fn at(&self, t: usize, channel: &str) -> f64 {
        let n = self.rows.len();
        let k = t % (2 * n);
        self.rows[if k < n { k } else { 2 * n - 1 - k }][self.channels.iter().position(|c| c == channel).unwrap()]
    }
}

struct Noise(u64);

impl Noise {
    fn next(&mut self, amp: f64) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0) * amp
    }
    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 11) as f64 + 1.0) / ((1u64 << 53) as f64 + 1.0)
    }
    fn gauss(&mut self, sigma: f64) -> f64 {
        let (a, b) = (self.unit(), self.unit());
        sigma * (-2.0 * a.ln()).sqrt() * (std::f64::consts::TAU * b).cos()
    }
}

#[derive(Debug, Default)]
struct Outcome {
    idle_at: Option<usize>,
    fan_cold: f64,
    budget: bool,
    writes: usize,
    stops: usize,
    peak: f64,
    pump_hi: f64,
    fan_hi: f64,
    fallback: bool,
    frozen: bool,
    hits: usize,
    releases: usize,
    unchanged: f64,
    exact_max: usize,
    swings: usize,
    short_cycles: usize,
    rise: Option<usize>,
    pre: (f64, f64),
}

const CPU_POWER: &str = "/intelcpu/0/power/0";
const GPU_POWER: &str = "nvml/0/power";
const PUMP_POWER: &str = "d5next/pump/power";

const SENSOR_NOISE: f64 = 0.003;

fn simulate(profile: Profile, spec: PlantSpec, tweak: impl Fn(&mut Config)) -> Outcome {
    simulate_noisy(profile, spec, SENSOR_NOISE, tweak)
}

fn simulate_noisy(profile: Profile, spec: PlantSpec, noise: f64, tweak: impl Fn(&mut Config)) -> Outcome {
    simulate_with(plan::Kind::Blind, profile, spec, noise, tweak)
}

fn simulate_with(kind: plan::Kind, profile: Profile, spec: PlantSpec, noise: f64, tweak: impl Fn(&mut Config)) -> Outcome {
    let mut rng = Noise(88172645463325252);
    let plant = Rc::new(RefCell::new(Plant::new(spec)));
    let feed = Rc::new(Cell::new((0.0, 0.0)));
    let (mut rig, mut world) = plan::rig(kind, &plant, &feed, tweak);
    let trace = matches!(profile, Profile::Golden).then(Trace::golden);
    plant.borrow_mut().retarget(world.as_ref().map_or(30.0, |w| w.start));
    rig.water = plant.borrow().t1;
    for _ in 0..5 {
        plan::advance(&mut rig, &mut world, 0.0);
    }
    rig.active("d5.fan");
    rig.active("d5.pump");
    rig.c.frozen_slow_ms = std::env::var("FROZEN_SLOW_S").map_or(i64::MAX, |_| slow_ms());
    let fan_cal = ChannelConfig::defaults(ControlKind::Fan).calibration();
    let mut out = Outcome::default();
    let (mut pump_hi, mut fan_hi, mut fan_cold, mut high) = (0usize, 0usize, 0usize, false);
    let (mut fan_on, mut fan_since, mut before) = (None::<bool>, 0usize, (0.0, 0.0));
    let secs = profile.secs();
    let (mut was_hit, mut unchanged, mut prev, mut run) = (false, 0usize, f64::NAN, 0usize);
    for t in 0..secs {
        let r = rig.sim.report();
        let rpm = |s: Spec, cal: &Calibration| cal.rpm(if r[s.mode()] == 0 { be16(&r, s.preset()) as f64 / 100.0 } else { 90.0 });
        let power = profile.power(t);
        let fan_rpm = rpm(Spec::FAN, &fan_cal);
        feed.set((power, fan_rpm));
        plant.borrow_mut().step(power, fan_rpm);
        out.peak = out.peak.max(plant.borrow().t1);
        rig.extra = vec![(CPU_POWER, power), (GPU_POWER, 0.0), (PUMP_POWER, 5.0)];
        rig.water = match (&profile, &trace) {
            (_, Some(g)) => {
                rig.cpu = g.at(t, "/intelcpu/0/temperature/0");
                rig.gpu = Some(g.at(t, "nvml/0/temp"));
                rig.extra.clear();
                g.at(t, "d5next/water/temp")
            }
            (Profile::Sine(c), _) => c + 0.3 * (std::f64::consts::TAU * t as f64 / 600.0).sin(),
            _ => ((plant.borrow().reading() + rng.gauss(noise)) * 100.0).round() / 100.0,
        };
        if trace.is_none() {
            rig.cpu = rig.water + 15.0 + 0.1 * power;
        }
        plan::advance(&mut rig, &mut world, fan_rpm);
        let hit = rig.c.frozen_hit(rig.now).is_some();
        out.frozen |= hit;
        out.hits += (hit && !was_hit) as usize;
        out.releases += (!hit && was_hit) as usize;
        was_hit = hit;
        unchanged += (rig.water == prev) as usize;
        run = if rig.water == prev { run + 1 } else { 0 };
        out.exact_max = out.exact_max.max(run);
        prev = rig.water;
        let d = |i: usize| rig.c.chans[i].duty_cmd;
        pump_hi += d(1).is_some_and(|x| x >= 90.0) as usize;
        high |= d(1).is_some_and(|x| x >= 90.0);
        if high && d(1).is_some_and(|x| x <= 20.0) {
            (high, out.swings) = (false, out.swings + 1);
        }
        fan_hi += d(0).is_some_and(|x| x >= 90.0) as usize;
        if out.idle_at.is_none() && t >= profile.load_end() && d(0) == Some(0.0) && d(1).is_some_and(|x| x < 15.5) {
            out.idle_at = Some(t - profile.load_end());
        }
        fan_cold += (d(0).is_some_and(|x| x > 0.0) && rig.water < 33.0) as usize;
        out.budget |= rig.c.chans.iter().any(|c| c.over);
        let on = d(0).is_some_and(|x| x > 0.0);
        if fan_on.is_some_and(|was| was != on) {
            out.short_cycles += (t - fan_since < ChannelConfig::defaults(ControlKind::Fan).min_run_secs as usize) as usize;
        }
        if fan_on != Some(on) {
            (fan_on, fan_since) = (Some(on), t);
        }
        if let Profile::Step(_, _, at) = profile {
            let level = (d(0).unwrap_or(0.0), d(1).unwrap_or(0.0));
            if t + 1 == at {
                before = level;
                out.pre = level;
            } else if t >= at && out.rise.is_none() && level.0 > before.0 + 2.0 {
                out.rise = Some(t - at);
            }
        }
    }
    out.fan_cold = fan_cold as f64 / secs as f64;
    out.unchanged = unchanged as f64 / secs as f64;
    let sets = rig.sim.duty_sets();
    let fan: Vec<u16> = sets.iter().map(|(_, r)| be16(r, Spec::FAN.preset())).collect();
    out.stops = fan.windows(2).filter(|w| w[0] != 0 && w[1] == 0).count();
    out.writes = sets.len();
    out.pump_hi = pump_hi as f64 / secs as f64;
    out.fan_hi = fan_hi as f64 / secs as f64;
    out.fallback = rig.c.chans.iter().any(|c| c.fallback.is_some());
    out
}

fn profiles() -> Vec<Profile> {
    let mut v: Vec<Profile> = [40.0, 60.0, 100.0, 200.0, 300.0, 400.0].iter().map(|w| Profile::Constant(*w)).collect();
    v.extend([Profile::Square(60.0, 300.0, 600, 600), Profile::Sine(34.5), Profile::Sine(35.0), Profile::Sine(35.5), Profile::Sine(36.0), Profile::Sine(37.5), Profile::Golden]);
    v
}

#[test]
#[ignore]
fn r13_writes_and_pump_table_per_24_h() {
    let mut table = String::from("plant | profile | writes | stops | peak C | pump >=90 % | fan >=90 % | fallback | frozen\n");
    for (name, spec) in PlantSpec::NAMED {
        for p in profiles().into_iter().filter(|p| p.closed() || name == "measured") {
            let o = simulate(p, spec, |_| {});
            table.push_str(&format!("{name} | {} | {} | {} | {:.1} | {:.0} | {:.0} | {} | {}\n", p.name(), o.writes, o.stops, o.peak, o.pump_hi * 100.0, o.fan_hi * 100.0, o.fallback, o.frozen));
        }
    }
    eprintln!("{table}");
}

fn assert_quiet(profile: Profile, spec: PlantSpec, limit: usize) {
    let o = simulate(profile, spec, |_| {});
    assert!(o.writes < limit && !o.budget && !o.fallback && !o.frozen, "{} {o:?}", profile.name());
}

#[test]
fn r14_a_constant_load_writes_less_than_250_times_a_day_on_both_plants() {
    for (_, spec) in PlantSpec::NAMED {
        for w in pump_loads().into_iter().chain([100.0, 250.0, 400.0]) {
            assert_quiet(Profile::Constant(w), spec, 250);
        }
    }
}

#[test]
fn r35_the_pump_does_not_swing_between_the_floor_and_full_duty_at_constant_loads_40_to_80_w() {
    for (name, w, o) in pump_sweep(&|_| {}) {
        assert!(o.swings <= 2 && !o.budget && !o.fallback && !o.frozen, "{name} {w} W swings {} writes {}", o.swings, o.writes);
    }
}

#[test]
fn r25_after_one_load_the_fan_stands_and_the_pump_reaches_its_floor_on_both_plants_with_sensor_noise() {
    for (name, spec) in PlantSpec::NAMED {
        for (hi, secs, lo) in [(400.0, 1800, 20.0), (300.0, 3600, 40.0), (150.0, 900, 25.0), (100.0, 600, 15.0)] {
            let p = Profile::Once(hi, secs, lo);
            let o = simulate(p, spec, |_| {});
            let hold = ControlConfig::default().bound_hold_secs as usize;
            assert!(o.idle_at.is_some_and(|t| t < 3 * hold + 3600) && !o.fallback && !o.frozen, "{name} {} {o:?}", p.name());
        }
    }
}

#[test]
fn r15_a_10_plus_10_minute_load_writes_less_than_250_times_a_day_on_both_plants() {
    for (_, spec) in PlantSpec::NAMED {
        assert_quiet(Profile::Square(60.0, 300.0, 600, 600), spec, 250);
    }
}

#[test]
fn r16_water_jitter_around_the_pump_slope_and_the_golden_trace_stay_below_the_write_limits() {
    for c in [35.0, 35.5, 36.0] {
        assert_quiet(Profile::Sine(c), PlantSpec::MEASURED, 250);
    }
    assert_quiet(Profile::Golden, PlantSpec::MEASURED, 250);
}

#[test]
fn r30_a_recurring_short_load_keeps_the_fan_on_below_33_c_for_at_most_30_percent_of_the_day() {
    for (name, spec) in PlantSpec::NAMED {
        let o = simulate(Profile::recurring(), spec, |_| {});
        assert!(o.fan_cold <= 0.30 && !o.budget && !o.fallback, "{name} {o:?}");
    }
}

#[test]
fn r17_only_raises_are_exempt_a_raise_is_never_delayed_and_the_bound_lowers_after_a_fall_and_the_hold() {
    let mut rig = running(34.0, |c| c.control.bound_hold_secs = 120.0);
    let used = rig.c.chans[0].cfg.daily_writes;
    rig.c.chans[0].used = used;
    rig.water = 41.0;
    rig.run(3);
    let high = bound(&rig, 0);
    assert!(high >= rig.c.curve_bound(0, rig.now).unwrap() && duty(&rig, 0).unwrap() >= high - 2.0);
    rig.water = 36.0;
    rig.run(60);
    assert!(bound(&rig, 0) >= high && duty(&rig, 0).unwrap() >= high - 2.0);
    rig.run(100);
    assert!(bound(&rig, 0) < high - 20.0);
    assert!(duty(&rig, 0).unwrap() >= high - 2.0 && rig.alarm(AlarmKind::Budget));
    let mut fresh = running(34.0, |c| c.control.bound_hold_secs = 120.0);
    fresh.water = 41.0;
    fresh.run(3);
    fresh.water = 36.0;
    fresh.run(400);
    assert!(duty(&fresh, 0).unwrap() < high - 20.0 && !fresh.alarm(AlarmKind::Budget));
}

#[test]
fn r18_a_small_fall_does_not_lower_the_bound_and_the_raise_rate_cap_stops_exemption() {
    let mut rig = running(35.0, |c| c.control.bound_hold_secs = 60.0);
    rig.live = true;
    rig.water = 37.0;
    rig.run(10);
    let high = bound(&rig, 1);
    rig.water = 36.4;
    rig.run(300);
    assert_eq!(bound(&rig, 1), high);
    let mut capped = running(34.0, |c| c.control.bound_raise_cap = 3);
    let now = capped.now;
    capped.c.chans[0].raises.extend([now; 3]);
    capped.c.chans[0].used = capped.c.chans[0].cfg.daily_writes;
    capped.water = 41.0;
    capped.run(10);
    assert!(capped.c.chans[0].over && duty(&capped, 0).unwrap() < bound(&capped, 0) - 20.0);
}

#[test]
fn r19_a_step_test_the_bound_overrides_is_flagged_in_status_and_telemetry() {
    let mut rig = Rig::new((true, true), 3.0, |c| c.control.step_test.fan = vec![[40.0, 60.0]]);
    rig.water = 40.0;
    rig.run(40);
    rig.c.start_test("d5.fan", true, rig.now).unwrap();
    let rows = rig.tick();
    let step = rig.status().step.unwrap();
    assert!(step.overridden && step.duty > 70.0);
    assert!(rows.iter().any(|r| r.channel == "daisyloop/step/overridden" && r.value == 1.0));
    let mut cool = Rig::new((true, true), 3.0, |c| c.control.step_test.fan = vec![[40.0, 60.0]]);
    cool.water = 30.0;
    cool.run(40);
    cool.c.start_test("d5.fan", true, cool.now).unwrap();
    cool.run(2);
    assert!(!cool.status().step.unwrap().overridden);
}

#[test]
fn r20_the_rate_limit_after_a_hold_starts_from_the_current_duty() {
    let mut rig = running(34.0, |_| {});
    rig.gpu = None;
    rig.run(40);
    rig.water = 38.5;
    rig.run(8);
    let held = duty(&rig, 0).unwrap();
    assert!(held >= 60.0 && held <= 80.0 && held >= bound(&rig, 0) - 2.0);
    rig.heat.set(14.0);
    rig.gpu = Some(40.0);
    rig.run(35);
    assert!(duty(&rig, 0).unwrap() >= 95.0, "{:?} {held}", duty(&rig, 0));
}

struct Frozen {
    detect: Option<usize>,
    peak: f64,
    fan: Option<f64>,
    pump: Option<f64>,
    handed_over: bool,
    alarm: bool,
    why: Option<String>,
    changes: [usize; 2],
}

fn frozen_run(spec: PlantSpec, stuck: f64, power: f64, cpu_alive: bool, dither: usize) -> Frozen {
    let mut rig = blind_rig(|_| {});
    let mut plant = Plant::new(spec);
    (plant.t1, plant.t2, plant.seen) = (stuck, stuck, VecDeque::from(vec![stuck]));
    rig.water = stuck;
    rig.run(5);
    rig.active("d5.fan");
    rig.active("d5.pump");
    rig.c.frozen_slow_ms = slow_ms();
    let cal = ChannelConfig::defaults(ControlKind::Fan).calibration();
    let (mut detect, mut peak, mut changes, mut last) = (None, 0.0f64, [0usize; 2], [None, None]);
    for t in 0..7200 {
        let r = rig.sim.report();
        plant.step(power, cal.rpm(if r[Spec::FAN.mode()] == 0 { be16(&r, Spec::FAN.preset()) as f64 / 100.0 } else { 90.0 }));
        peak = peak.max(plant.t1);
        rig.water = stuck + if dither > 0 && t / dither % 2 == 1 { 0.01 } else { 0.0 };
        rig.cpu = if cpu_alive { 50.0 } else { f64::NAN };
        rig.extra = if cpu_alive { vec![(CPU_POWER, power * 0.6), (GPU_POWER, power * 0.4), (PUMP_POWER, 5.0)] } else { vec![(GPU_POWER, power), (PUMP_POWER, 5.0)] };
        rig.tick();
        if detect.is_none() && rig.c.frozen_hit(rig.now).is_some() {
            detect = Some(t);
        }
        for i in 0..2 {
            let d = duty(&rig, i);
            changes[i] += (detect.is_some() && d != last[i]) as usize;
            last[i] = d;
        }
    }
    Frozen { detect, peak, fan: duty(&rig, 0), pump: duty(&rig, 1), handed_over: rig.sim.mode(Spec::FAN) == 1 || rig.sim.mode(Spec::PUMP) == 1, alarm: rig.alarm(AlarmKind::WaterFrozen), why: rig.ctl("d5.fan").fallback, changes }
}

#[test]
fn r21_frozen_water_is_detected_by_heat_within_200_s_or_by_the_plain_rule_after_10_minutes_and_both_channels_run_at_full_duty() {
    for (name, spec) in PlantSpec::NAMED {
        let heat = frozen_run(spec, 30.0, 300.0, true, 0);
        assert!(heat.detect.is_some_and(|t| t < 200), "{name} {:?}", heat.detect);
        for (f, label) in [(heat, "heat"), (frozen_run(spec, 33.0, 40.0, true, 0), "plain")] {
            assert!(f.detect.is_some() && f.fan == Some(100.0) && f.pump == Some(100.0) && !f.handed_over && f.alarm, "{name} {label}");
            assert!(label == "heat" || f.detect.is_some_and(|t| (590..700).contains(&t)), "{name} {label} {:?}", f.detect);
        }
    }
}

#[test]
fn r28_a_stuck_water_value_never_lets_the_measured_plant_pass_50_c_up_to_400_w() {
    for stuck in [30.0, 33.0, 34.5, 35.5, 36.5, 37.5, 39.0, 41.0, 43.0] {
        for power in [40.0, 100.0, 200.0, 300.0, 400.0] {
            let f = frozen_run(PlantSpec::MEASURED, stuck, power, true, 0);
            assert!(f.peak <= 50.0 && f.detect.is_some() && !f.handed_over, "stuck {stuck} load {power} {:?} peak {:.1}", f.detect, f.peak);
        }
    }
}

#[test]
#[ignore]
fn r27_frozen_table_stuck_value_load_detection_and_peak_true_water() {
    let mut table = String::from("plant | stuck C | load W | detected after s | peak true water C | fan / pump command
");
    let variants = [("measured", PlantSpec::MEASURED), ("alternative", PlantSpec::ALTERNATIVE), ("sink 30 C", PlantSpec { ambient: 30.0, ..PlantSpec::MEASURED }), ("radiator half", PlantSpec { g_full: 12.0, ..PlantSpec::MEASURED }), ("C half", PlantSpec { c_scale: 0.5, ..PlantSpec::MEASURED }), ("C double", PlantSpec { c_scale: 2.0, ..PlantSpec::MEASURED })];
    for (name, spec) in variants {
        for stuck in [30.0, 33.0, 34.5, 35.5, 36.5, 37.5, 39.0, 41.0, 43.0] {
            for power in [40.0, 100.0, 200.0, 300.0, 400.0, 600.0] {
                let f = frozen_run(spec, stuck, power, true, 0);
                table.push_str(&format!("{name} | {stuck} | {power} | {} | {:.1} | {:?} / {:?}
", f.detect.map_or("none".into(), |t| t.to_string()), f.peak, f.fan, f.pump));
            }
        }
    }
    eprintln!("{table}");
}

fn flicker_grid() -> Vec<(String, f64, Frozen)> {
    let mut cases = vec![];
    for (ambient, limit) in [(25.0, 46.0), (30.0, 50.0)] {
        let spec = PlantSpec { ambient, ..PlantSpec::MEASURED };
        for stuck in [37.5, 39.0] {
            for power in (100..=400).step_by(25).map(f64::from) {
                for dither in [1, 60] {
                    cases.push((format!("ambient {ambient} stuck {stuck} load {power} dither {dither}"), limit, frozen_run(spec, stuck, power, true, dither)));
                }
            }
        }
    }
    cases
}

#[test]
fn r31_a_stuck_water_value_flickering_by_one_lsb_is_caught_and_the_true_water_stays_below_46_c_at_25_c_ambient_and_50_c_at_30_c() {
    let bad: Vec<String> = flicker_grid().into_iter().filter(|(_, limit, f)| !(f.detect.is_some() && f.peak <= *limit && !f.handed_over && f.fan == Some(100.0) && f.pump == Some(100.0))).map(|(case, _, f)| format!("{case} {:?} peak {:.1}", f.detect, f.peak)).collect();
    assert!(bad.is_empty(), "{bad:#?}");
}

#[test]
#[ignore]
fn r46_flicker_table_detection_and_peak_true_water() {
    let mut table = String::from("case | detected after s | peak true water C
");
    for (case, _, f) in flicker_grid() {
        table.push_str(&format!("{case} | {} | {:.1}
", f.detect.map_or("none".into(), |t| t.to_string()), f.peak));
    }
    eprintln!("{table}");
}

#[test]
#[ignore]
fn r43_frozen_false_trips_and_releases_per_24_h_at_constant_loads_table() {
    if std::env::var("FROZEN_SLOW_S").is_err() {
        std::env::set_var("FROZEN_SLOW_S", (Control::FROZEN_SLOW_MS / 1000).to_string());
    }
    let mut table = format!("window {} s
plant | load W | trips | releases | exact max s
", slow_ms() / 1000);
    let (mut over, mut open) = (0, 0);
    for (name, spec) in PlantSpec::NAMED {
        for w in (20..=400).step_by(20).map(f64::from).chain(pump_loads()) {
            let o = simulate(Profile::Constant(w), spec, |_| {});
            over += (o.hits > 1) as usize;
            open += (o.releases != o.hits) as usize;
            table.push_str(&format!("{name} | {w} | {} | {} | {}
", o.hits, o.releases, o.exact_max));
        }
    }
    eprintln!("{table}cases with more than 1 trip {over}, trips without a release {open}");
}

#[test]
#[ignore]
fn r44_frozen_rule_on_the_real_extract_has_no_hit() {
    let text = std::fs::read_to_string(std::env::var("REAL_CSV").unwrap()).unwrap();
    let mut rig = Rig::new((false, false), 3.0, |_| {});
    rig.c.frozen_slow_ms = slow_ms();
    let (mut rows, mut hits, mut was) = (0usize, 0usize, false);
    for line in text.lines() {
        let v: Vec<f64> = line.split(',').map(|x| x.parse().unwrap_or(f64::NAN)).collect();
        rig.water = v[0];
        rig.cpu = v[1];
        rig.gpu = v[2].is_finite().then_some(v[2]);
        rig.extra = vec![(CPU_POWER, v[3]), (GPU_POWER, v[4]), (PUMP_POWER, v[5])].into_iter().filter(|x| x.1.is_finite()).collect();
        rig.sim.0.borrow_mut().force = [v[6].is_finite().then_some(v[6]), v[7].is_finite().then_some(v[7])];
        rig.tick();
        rows += 1;
        let hit = rig.c.frozen_hit(rig.now).is_some();
        if hit && !was {
            hits += 1;
            eprintln!("hit at row {rows} water {}", v[0]);
        }
        was = hit;
    }
    eprintln!("REAL rows {rows} hits {hits} window {} s", rig.c.frozen_slow_ms / 1000);
    assert_eq!(hits, 0);
}

#[test]
fn r32_frozen_state_latches_and_the_commands_do_not_chatter_before_the_reading_moves() {
    for (stuck, power) in [(34.5, 400.0), (37.5, 400.0), (39.0, 400.0), (36.5, 300.0), (30.0, 200.0)] {
        for dither in [0, 1] {
            let f = frozen_run(PlantSpec::MEASURED, stuck, power, true, dither);
            assert!(f.changes.iter().all(|c| *c <= 2) && f.fan == Some(100.0) && f.pump == Some(100.0), "stuck {stuck} load {power} dither {dither} {:?}", f.changes);
        }
    }
}

#[test]
fn r33_the_frozen_state_ends_only_when_the_reading_has_moved_clearly_beyond_the_band() {
    let mut rig = running(36.5, |_| {});
    rig.run(700);
    assert!(rig.alarm(AlarmKind::WaterFrozen));
    for w in [36.51, 36.52, 36.53, 36.54, 36.55] {
        rig.water = w;
        rig.run(5);
        assert!(rig.alarm(AlarmKind::WaterFrozen), "{w}");
    }
    rig.water = 36.6;
    rig.run(5);
    assert!(!rig.alarm(AlarmKind::WaterFrozen));
}

#[test]
fn r45_a_water_gap_resets_the_frozen_state_and_a_same_reading_after_it_does_not_trip() {
    let mut rig = running(36.5, |_| {});
    rig.live = true;
    rig.run(120);
    rig.live = false;
    rig.water = 36.5;
    rig.run(2);
    rig.water = f64::NAN;
    rig.run(640);
    assert!(rig.c.frozen.is_none() && rig.c.frozen_hit(rig.now).is_none());
    rig.water = 36.5;
    rig.run(3);
    assert!(rig.c.frozen_hit(rig.now).is_none() && !rig.alarm(AlarmKind::WaterFrozen));
    rig.run(300);
    assert!(rig.c.frozen_hit(rig.now).is_none());
    rig.run(400);
    assert!(rig.c.frozen_hit(rig.now).is_some());
}

#[test]
fn r22_the_recorded_golden_trace_never_looks_frozen() {
    let g = crate::model::Golden::load();
    let col = |name: &str| g.channels.iter().position(|c| c == name).unwrap();
    let (iw, ic, ig, icp, igp, ipp, ifr, ipr) = (col("d5next/water/temp"), col("/intelcpu/0/temperature/0"), col("nvml/0/temp"), col(CPU_POWER), col(GPU_POWER), col(PUMP_POWER), col("d5next/fan/rpm"), col("d5next/pump/rpm"));
    let mut rig = Rig::new((false, false), 3.0, |_| {});
    for row in &g.rows {
        rig.water = row[iw].unwrap_or(f64::NAN);
        rig.cpu = row[ic].unwrap_or(f64::NAN);
        rig.gpu = row[ig];
        rig.extra = [(CPU_POWER, icp), (GPU_POWER, igp), (PUMP_POWER, ipp)].iter().filter_map(|(n, i)| row[*i].map(|v| (*n, v))).collect();
        rig.sim.0.borrow_mut().force = [row[ifr], row[ipr]];
        rig.tick();
        assert!(rig.c.frozen_hit(rig.now).is_none(), "t {}", rig.now);
    }
}

#[test]
fn r23_frozen_water_commands_full_duty_with_writes_on_and_stays_an_alarm_with_writes_off() {
    let mut high = running(44.0, |_| {});
    high.live = false;
    for t in 0..700 {
        high.cpu = if t / 10 % 2 == 0 { 40.0 } else { 70.0 };
        high.tick();
    }
    assert!(high.ctl("d5.fan").fallback.is_none() && high.sim.mode(Spec::FAN) == 0 && duty(&high, 0) == Some(100.0));
    assert!(high.alarm(AlarmKind::WaterFrozen));
    high.water = 38.0;
    high.run(400);
    assert!(!high.alarm(AlarmKind::WaterFrozen) && high.ctl("d5.fan").fallback.is_none());
}

#[test]
fn r29_a_failed_full_duty_write_hands_the_channels_to_the_firmware() {
    let mut rig = running(36.5, |_| {});
    rig.run(100);
    assert!(!rig.alarm(AlarmKind::WaterFrozen));
    rig.sim.0.borrow_mut().fail_sets = 1;
    rig.run(60);
    assert!(rig.alarm(AlarmKind::WaterFrozen));
    assert!(rig.ctl("d5.fan").fallback.is_some_and(|f| f.contains("Schreibfehler")) && rig.sim.mode(Spec::FAN) == 1);
}

#[test]
#[ignore]
fn r24_pump_share_at_or_above_90_percent_by_pump_ramp_start() {
    let mut table = String::from("plant | profile | pump >=90 % (writes) with ramp 35 / 38 / 40 / 42 C
");
    for (name, spec) in PlantSpec::NAMED {
        for p in profiles().into_iter().filter(|p| p.closed() || name == "measured") {
            let cells: Vec<String> = [35.0, 38.0, 40.0, 42.0]
                .iter()
                .map(|x| {
                    let o = simulate(p, spec, |c| c.control.pump.min_curve = Some(vec![[*x, 15.0], [*x + 1.0, 100.0]]));
                    format!("{:.0} % ({})", o.pump_hi * 100.0, o.writes)
                })
                .collect();
            table.push_str(&format!("{name} | {} | {}
", p.name(), cells.join(" / ")));
        }
    }
    eprintln!("{table}");
}


fn sweep(cases: &[(PlantSpec, Profile)], tweak: &(dyn Fn(&mut Config) + Sync)) -> Vec<Outcome> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let done = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..std::env::var("DL_SIM_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(2usize) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Some((spec, p)) = cases.get(i) else { break };
                let o = simulate(*p, *spec, tweak);
                done.lock().unwrap().push((i, o));
            });
        }
    });
    let mut v = done.into_inner().unwrap();
    v.sort_by_key(|x| x.0);
    v.into_iter().map(|x| x.1).collect()
}

fn pump_loads() -> Vec<f64> {
    (0..=16).map(|i| 40.0 + 2.5 * i as f64).collect()
}

fn pump_sweep(tweak: &(dyn Fn(&mut Config) + Sync)) -> Vec<(&'static str, f64, Outcome)> {
    let cases: Vec<(&str, PlantSpec, f64)> = PlantSpec::NAMED.iter().flat_map(|(n, s)| pump_loads().into_iter().map(move |w| (*n, *s, w))).collect();
    let out = sweep(&cases.iter().map(|c| (c.1, Profile::Constant(c.2))).collect::<Vec<_>>(), tweak);
    cases.iter().zip(out).map(|(c, o)| (c.0, c.2, o)).collect()
}

#[test]
#[ignore]
fn r34_pump_swings_and_writes_per_24_h_at_constant_loads_40_to_80_w() {
    let mut table = String::from("plant | load W | pump swings 15-100 | writes | pump >=90 %
");
    for (name, w, o) in pump_sweep(&|_| {}) {
        table.push_str(&format!("{name} | {w} | {} | {} | {:.0}
", o.swings, o.writes, o.pump_hi * 100.0));
    }
    eprintln!("{table}");
}

fn criteria_row(hold: f64, down: f64) -> String {
    let tweak = move |c: &mut Config| {
        c.control.bound_hold_secs = hold;
        c.control.fan.down_step_duty = Some(down);
        c.control.pump.down_step_duty = Some(down);
    };
    let mut cases = vec![];
    for (_, spec) in PlantSpec::NAMED {
        cases.push((spec, Profile::recurring()));
    }
    for (name, spec) in PlantSpec::NAMED {
        let mut base: Vec<Profile> = [40.0, 60.0, 100.0, 200.0, 300.0, 400.0].iter().map(|w| Profile::Constant(*w)).collect();
        base.push(Profile::Square(60.0, 300.0, 600, 600));
        if name == "measured" {
            base.push(Profile::Golden);
        }
        cases.extend(base.into_iter().map(|p| (spec, p)));
    }
    let basic = cases.len();
    for (_, spec) in PlantSpec::NAMED {
        cases.extend(Profile::grid().into_iter().map(|p| (spec, p)));
    }
    let out = sweep(&cases, &tweak);
    let max = |r: std::ops::Range<usize>| out[r].iter().map(|o| o.writes).max().unwrap();
    let grid = Profile::grid().len();
    let budget = out.iter().any(|o| o.budget);
    let frozen = out.iter().any(|o| o.frozen || o.fallback);
    format!("{hold:5.0} | {down:4.0} | {:.0} / {:.0} | {} | {} / {} | {budget} | {frozen}", out[0].fan_cold * 100.0, out[1].fan_cold * 100.0, max(2..basic), max(basic..basic + grid), max(basic + grid..basic + 2 * grid))
}

#[test]
#[ignore]
fn r26_hold_and_coarse_down_step_table() {
    let mut table = String::from("hold s | down duty | fan on <33 C % recurring meas / alt | max writes const,10+10,golden | grid max meas / alt | budget alarm | frozen or fallback\n");
    for hold in [300.0, 600.0, 900.0, 1200.0, 1800.0, 3600.0] {
        for down in [0.0, 35.0] {
            table.push_str(&criteria_row(hold, down));
            table.push('\n');
        }
    }
    eprintln!("{table}");
}


#[path = "plan_tests.rs"]
mod plan;

#[path = "online_tests.rs"]
mod online;

fn scan_env(key: &str, default: f64) -> f64 {
    std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn slow_ms() -> i64 {
    (scan_env("FROZEN_SLOW_S", Control::FROZEN_SLOW_MS as f64 / 1000.0) * 1000.0) as i64
}

#[test]
#[ignore]
fn scan_case() {
    let c_scale = scan_env("SC_CS", 1.0);
    let base = if scan_env("SC_ALT", 0.0) > 0.0 { PlantSpec::ALTERNATIVE } else { PlantSpec::MEASURED };
    let spec = PlantSpec { ambient: scan_env("SC_AMB", base.ambient), c_scale, g_stop: scan_env("SC_GSTOP", base.g_stop), ..base };
    let profile = if std::env::var("SC_REC").is_ok() { Profile::recurring() } else if std::env::var("SC_SQ").is_ok() { Profile::Square(60.0, 300.0, 600, 600) } else if let Ok(v) = std::env::var("SC_STEP_FROM") { Profile::Step(v.parse().unwrap(), scan_env("SC_STEP_TO", 260.0), 3600) } else { Profile::Constant(scan_env("SC_W", 70.0)) };
    let (lift, hold, drop) = (scan_env("SC_LIFT", ControlConfig::default().bound_raise_step), scan_env("SC_HOLD", 900.0), scan_env("SC_DROP", 1.0));
    let o = simulate(profile, spec, |c| {
        c.control.bound_raise_step = lift;
        c.control.bound_hold_secs = hold;
        c.control.bound_drop_k = drop;
    });
    println!("SC fan_cold {:.3} alt {} amb {} cs {c_scale} {} lift {lift} hold {hold} drop {drop} swings {} writes {} peak {:.2} hits {} unchanged {:.3} exact_max {} budget {} fallback {} short {}", o.fan_cold, base.ambient == 20.0, spec.ambient, profile.name(), o.swings, o.writes, o.peak, o.hits, o.unchanged, o.exact_max, o.budget, o.fallback, o.short_cycles);
}

#[test]
fn r41_the_pump_limit_cycle_stays_gone_across_ambient_and_heat_capacity() {
    for (ambient, c_scale, power) in [(20.0, 0.5, 85.0), (20.0, 0.5, 75.0), (21.0, 0.5, 70.0), (22.0, 0.5, 50.0), (22.0, 1.0, 50.0), (23.0, 1.0, 70.0), (24.0, 1.0, 60.0)] {
        let o = simulate(Profile::Constant(power), PlantSpec { ambient, c_scale, ..PlantSpec::MEASURED }, |_| {});
        assert!(o.swings <= 2 && o.writes <= 250 && !o.budget && !o.fallback && !o.frozen, "ambient {ambient} c_scale {c_scale} {power} W swings {} writes {}", o.swings, o.writes);
    }
}

#[test]
#[ignore]
fn r42_ambient_scan_swings_and_writes_per_24_h() {
    let mut table = String::from("ambient C | c_scale | load W | swings | writes | peak C
");
    let (mut worst_swings, mut worst_writes) = (0, 0);
    for ambient in [20.0, 21.0, 22.0, 23.0, 24.0, 25.0, 26.0] {
        for c_scale in [0.5, 1.0] {
            for power in (40..=90).step_by(5) {
                let o = simulate(Profile::Constant(power as f64), PlantSpec { ambient, c_scale, ..PlantSpec::MEASURED }, |_| {});
                (worst_swings, worst_writes) = (worst_swings.max(o.swings), worst_writes.max(o.writes));
                table.push_str(&format!("{ambient} | {c_scale} | {power} | {} | {} | {:.2}
", o.swings, o.writes, o.peak));
            }
        }
    }
    eprintln!("{table}worst swings {worst_swings} worst writes {worst_writes}");
    assert!(worst_swings <= 2 && worst_writes <= 250);
}

#[test]
fn r9c_a_stale_input_outside_the_fallback_block_does_not_hold_the_plan() {
    let mut rig = running(34.0, |_| {});
    rig.live = true;
    rig.c.inputs = vec!["nvml/0/temp".into(), "/intelcpu/0/temperature/0".into()];
    rig.c.fallback_inputs = vec!["/intelcpu/0/temperature/0".into()];
    rig.run(20);
    rig.gpu = None;
    rig.run(60);
    let st = rig.status();
    assert!(st.alarms.iter().any(|a| a.kind == AlarmKind::Stale && a.channel == "nvml/0/temp"));
    assert!(st.predicted.is_some_and(|p| p.water.is_some()) && st.controls[0].note.as_deref().is_none_or(|n| !n.contains("veraltet")));
    assert!(rig.ctl("d5.fan").fallback.is_none());
}

#[test]
fn r9b_a_stale_model_input_raises_an_alarm_and_holds_the_plan_without_a_fallback() {
    let mut rig = running(34.0, |_| {});
    rig.live = true;
    rig.c.inputs = vec!["nvml/0/temp".into(), "/intelcpu/0/temperature/0".into()];
    rig.c.fallback_inputs = rig.c.inputs.clone();
    rig.run(20);
    assert!(!rig.alarm(AlarmKind::Stale) && rig.status().predicted.is_some_and(|p| p.water.is_some()));
    rig.gpu = None;
    rig.run(12);
    let (held, sets) = (duty(&rig, 0), rig.sim.sets());
    rig.heat.set(40.0);
    rig.run(60);
    let st = rig.status();
    assert!(st.alarms.iter().any(|a| a.kind == AlarmKind::Stale && a.channel == "nvml/0/temp"));
    assert!(st.predicted.is_none() && st.controls[0].note.as_deref().is_some_and(|n| n.contains("nvml/0/temp")));
    assert!(duty(&rig, 0) == held && rig.sim.sets() == sets && rig.ctl("d5.fan").fallback.is_none() && rig.sim.mode(Spec::FAN) == 0, "{:?} {:?} {} {} {:?}", duty(&rig, 0), held, rig.sim.sets(), sets, rig.ctl("d5.fan").fallback);
    rig.gpu = Some(40.0);
    rig.run(40);
    assert!(!rig.alarm(AlarmKind::Stale) && rig.status().predicted.is_some());
}
