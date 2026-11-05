use super::*;
use crate::model::Forecast;
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
        Ok(Forecast { water: 0.0, cpu: 0.0, gpu: 0.0 })
    }
}

fn blind_rig(tweak: impl Fn(&mut Config)) -> Rig {
    let cfg = base_cfg(tweak);
    let sim = Sim::new();
    let s = sim.clone();
    let factory: Factory = Box::new(move || Ok(Box::new(s.clone()) as Box<dyn D5Control>));
    let c = Control::new(&cfg, Box::new(D5Actuator::new(factory, &cfg.control)), Ok(Box::new(Blind)));
    Rig { sim, c, now: 0, water: 30.0, cpu: 50.0, gpu: Some(40.0), model_fail: Rc::new(Cell::new(false)), warm: Rc::new(Cell::new(0)), heat: Rc::new(Cell::new(0.0)), extra: vec![], dir: cfg.state_dir() }
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
    rig.water = 36.0;
    rig.run(3);
    assert!(duty(&rig, 0).unwrap() >= 20.0);
    rig.water = 30.0;
    rig.run(560);
    assert!(duty(&rig, 0).unwrap() >= 20.0, "{:?}", duty(&rig, 0));
    rig.run(100);
    assert_eq!(duty(&rig, 0), Some(0.0));
    let mut long = running(32.0, |c| c.control.fan.min_run_secs = Some(1.0e6));
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
fn r7_frozen_water_with_moving_load_and_a_stopped_fan_goes_to_the_firmware() {
    let mut rig = running(32.0, |_| {});
    for t in 0..700 {
        rig.cpu = if t / 10 % 2 == 0 { 40.0 } else { 70.0 };
        rig.tick();
    }
    assert_eq!(rig.sim.mode(Spec::FAN), 1);
    assert!(rig.ctl("d5.fan").fallback.unwrap().contains("unverändert"));
    assert!(rig.alarm(AlarmKind::WaterFrozen) && rig.alarm(AlarmKind::Fallback));
    let mut off = Rig::new((true, true), 3.0, |c| c.writes_enabled = false);
    for t in 0..700 {
        off.cpu = if t / 10 % 2 == 0 { 40.0 } else { 70.0 };
        off.tick();
    }
    assert!(off.alarm(AlarmKind::WaterFrozen) && off.sim.sets() == 0);
}

#[test]
fn r8_cpu_and_gpu_plan_limits_are_not_shifted_by_the_weight_and_a_sustained_excess_raises_cooling() {
    let cfg = ControlConfig::default();
    let (cool, quiet) = (Planner::limits(&cfg, 1.0), Planner::limits(&cfg, 0.0));
    assert_eq!((cool[1], cool[2], quiet[1], quiet[2]), (105.0, 105.0, 105.0, 105.0));
    assert_eq!((quiet[0], cool[0]), (46.0, 41.0));
    let mut rig = Rig::new((false, false), 3.0, |_| {});
    rig.cpu = 100.0;
    rig.run(120);
    assert!(rig.status().predicted.unwrap().feasible);
    let quiet_fan = rig.ctl("d5.fan").target_rpm.unwrap();
    rig.cpu = 112.0;
    rig.run(150);
    let p = rig.status().predicted.unwrap();
    assert!(!p.feasible && p.cpu_limit == 105.0);
    assert!(rig.ctl("d5.fan").target_rpm.unwrap() > quiet_fan);
}

#[test]
fn r9_a_stale_cpu_or_gpu_holds_the_plan_with_the_bound_active_and_only_stale_water_takes_over() {
    let mut rig = running(34.0, |_| {});
    let held = duty(&rig, 0);
    rig.gpu = None;
    rig.run(60);
    let c = rig.ctl("d5.fan");
    assert!(rig.sim.mode(Spec::FAN) == 0 && c.fallback.is_none() && c.note.unwrap().contains("GPU"));
    assert!(duty(&rig, 0) == held && rig.alarm(AlarmKind::Stale));
    rig.water = 44.0;
    rig.run(8);
    assert_eq!((duty(&rig, 0), rig.sim.mode(Spec::FAN)), (Some(100.0), 0));
    rig.cpu = f64::NAN;
    rig.run(20);
    assert_eq!(rig.sim.mode(Spec::FAN), 0);
    rig.water = f64::NAN;
    rig.run(14);
    assert_eq!(rig.sim.mode(Spec::FAN), 1);
    assert!(rig.ctl("d5.fan").fallback.unwrap().contains("Wasser"));
}

#[test]
fn r10_a_pump_at_its_floor_does_not_fall_back_from_its_own_floor_and_pump_low_stays() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
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
}

impl PlantSpec {
    const MEASURED: PlantSpec = PlantSpec { ambient: 25.0, p0: 0.0, g_stop: 0.5, exponent: 1.61 };
    const ALTERNATIVE: PlantSpec = PlantSpec { ambient: 20.0, p0: 119.0, g_stop: 5.0, exponent: 0.865 };
    const NAMED: [(&'static str, PlantSpec); 2] = [("measured", Self::MEASURED), ("alternative", Self::ALTERNATIVE)];
}

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
    const G_FULL: f64 = 24.0;
    const DELAY: usize = 31;

    fn new(spec: PlantSpec) -> Plant {
        Plant { spec, t1: 30.0, t2: 30.0, seen: VecDeque::from(vec![30.0]) }
    }
    fn step(&mut self, power: f64, fan_rpm: f64) -> f64 {
        let g = self.spec.g_stop + (Self::G_FULL - self.spec.g_stop) * (fan_rpm / 1470.0).powf(self.spec.exponent);
        let flow = Self::K * (self.t1 - self.t2);
        self.t1 += (power + self.spec.p0 - g * (self.t1 - self.spec.ambient) - flow) / Self::C1;
        self.t2 += flow / Self::C2;
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
}

impl Profile {
    const SECS: usize = 86_400;
    fn power(&self, t: usize) -> f64 {
        match *self {
            Profile::Constant(w) => w,
            Profile::Square(lo, hi, on, off) => if t % (on + off) >= off { hi } else { lo },
            Profile::Once(hi, secs, lo) => if t < secs { hi } else { lo },
            Profile::Sine(_) => 100.0,
            Profile::Golden => 0.0,
        }
    }
    fn closed(&self) -> bool {
        matches!(self, Profile::Constant(_) | Profile::Square(..))
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
        for lo in [30.0, 60.0] {
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
}

const CPU_POWER: &str = "/intelcpu/0/power/0";
const GPU_POWER: &str = "nvml/0/power";
const PUMP_POWER: &str = "d5next/pump/power";

const SENSOR_NOISE: f64 = 0.02;

fn simulate(profile: Profile, spec: PlantSpec, tweak: impl Fn(&mut Config)) -> Outcome {
    simulate_noisy(profile, spec, SENSOR_NOISE, tweak)
}

fn simulate_noisy(profile: Profile, spec: PlantSpec, noise: f64, tweak: impl Fn(&mut Config)) -> Outcome {
    let mut rng = Noise(88172645463325252);
    let mut rig = blind_rig(tweak);
    let mut plant = Plant::new(spec);
    let trace = matches!(profile, Profile::Golden).then(Trace::golden);
    rig.water = plant.t1;
    rig.run(5);
    rig.active("d5.fan");
    rig.active("d5.pump");
    let fan_cal = ChannelConfig::defaults(ControlKind::Fan).calibration();
    let mut out = Outcome::default();
    let (mut pump_hi, mut fan_hi, mut fan_cold) = (0usize, 0usize, 0usize);
    for t in 0..Profile::SECS {
        let r = rig.sim.report();
        let rpm = |s: Spec, cal: &Calibration| cal.rpm(if r[s.mode()] == 0 { be16(&r, s.preset()) as f64 / 100.0 } else { 90.0 });
        let power = profile.power(t);
        let reading = plant.step(power, rpm(Spec::FAN, &fan_cal));
        out.peak = out.peak.max(plant.t1);
        rig.extra = vec![(CPU_POWER, power), (GPU_POWER, 0.0), (PUMP_POWER, 5.0)];
        rig.water = match (&profile, &trace) {
            (_, Some(g)) => {
                rig.cpu = g.at(t, "/intelcpu/0/temperature/0");
                rig.gpu = Some(g.at(t, "nvml/0/temp"));
                rig.extra.clear();
                g.at(t, "d5next/water/temp")
            }
            (Profile::Sine(c), _) => c + 0.3 * (std::f64::consts::TAU * t as f64 / 600.0).sin(),
            _ => ((reading + rng.next(noise)) * 100.0).round() / 100.0,
        };
        if trace.is_none() {
            rig.cpu = rig.water + 15.0 + 0.1 * power;
        }
        rig.tick();
        out.frozen |= rig.c.frozen_hit(rig.now).is_some();
        let d = |i: usize| rig.c.chans[i].duty_cmd;
        pump_hi += d(1).is_some_and(|x| x >= 90.0) as usize;
        fan_hi += d(0).is_some_and(|x| x >= 90.0) as usize;
        if out.idle_at.is_none() && t >= profile.load_end() && d(0) == Some(0.0) && d(1).is_some_and(|x| x < 15.5) {
            out.idle_at = Some(t - profile.load_end());
        }
        fan_cold += (d(0).is_some_and(|x| x > 0.0) && rig.water < 33.0) as usize;
        out.budget |= rig.c.chans.iter().any(|c| c.over);
    }
    out.fan_cold = fan_cold as f64 / Profile::SECS as f64;
    let sets = rig.sim.duty_sets();
    let fan: Vec<u16> = sets.iter().map(|(_, r)| be16(r, Spec::FAN.preset())).collect();
    out.stops = fan.windows(2).filter(|w| w[0] != 0 && w[1] == 0).count();
    out.writes = sets.len();
    out.pump_hi = pump_hi as f64 / Profile::SECS as f64;
    out.fan_hi = fan_hi as f64 / Profile::SECS as f64;
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

fn assert_quiet(profile: Profile, spec: PlantSpec) {
    let o = simulate(profile, spec, |_| {});
    assert!(o.writes < 100 && !o.fallback && !o.frozen, "{} {o:?}", profile.name());
}

#[test]
fn r14_a_constant_load_writes_less_than_100_times_a_day_on_both_plants() {
    for (_, spec) in PlantSpec::NAMED {
        for w in [40.0, 100.0, 250.0, 400.0] {
            assert_quiet(Profile::Constant(w), spec);
        }
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
fn r15_a_10_plus_10_minute_load_writes_less_than_100_times_a_day_on_both_plants() {
    for (_, spec) in PlantSpec::NAMED {
        assert_quiet(Profile::Square(60.0, 300.0, 600, 600), spec);
    }
}

#[test]
fn r16_water_jitter_around_the_pump_slope_and_the_golden_trace_write_less_than_100_times_a_day() {
    for c in [35.0, 35.5, 36.0] {
        assert_quiet(Profile::Sine(c), PlantSpec::MEASURED);
    }
    assert_quiet(Profile::Golden, PlantSpec::MEASURED);
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
    rig.water = 37.0;
    rig.run(10);
    let high = bound(&rig, 1);
    rig.water = 36.4;
    rig.run(300);
    assert_eq!(bound(&rig, 1), high);
    let mut capped = running(34.0, |c| c.control.bound_raise_cap = 0);
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

fn frozen_after(spec: PlantSpec, cpu_alive: bool, power: f64) -> Option<usize> {
    const SETTLE: usize = 900;
    let mut rig = blind_rig(|_| {});
    let mut plant = Plant::new(spec);
    (plant.t1, plant.t2, plant.seen) = (37.5, 37.5, VecDeque::from(vec![37.5]));
    rig.water = 37.5;
    rig.run(5);
    rig.active("d5.fan");
    rig.active("d5.pump");
    let cal = ChannelConfig::defaults(ControlKind::Fan).calibration();
    for t in 0..SETTLE + 900 {
        let r = rig.sim.report();
        let live = plant.step(if t < SETTLE { 100.0 } else { power }, cal.rpm(if r[Spec::FAN.mode()] == 0 { be16(&r, Spec::FAN.preset()) as f64 / 100.0 } else { 90.0 }));
        let load = if t < SETTLE { 100.0 } else { power };
        if t < SETTLE {
            rig.water = live;
        }
        let alive = cpu_alive || t < SETTLE;
        rig.cpu = if alive { 50.0 } else { f64::NAN };
        rig.extra = if alive { vec![(CPU_POWER, load / 2.0), (GPU_POWER, load / 2.0), (PUMP_POWER, 8.0)] } else { vec![(GPU_POWER, load), (PUMP_POWER, 8.0)] };
        rig.tick();
        let hit = rig.c.frozen_hit(rig.now).is_some();
        assert!(t >= SETTLE || !hit, "false positive before the freeze at {t}");
        if hit {
            assert_eq!(rig.sim.mode(Spec::FAN), 1, "handed over at any duty");
            return Some(t - SETTLE);
        }
    }
    None
}

#[test]
fn r21_frozen_water_under_heat_is_detected_within_3_minutes_with_and_without_the_cpu() {
    for (name, spec) in PlantSpec::NAMED {
        for alive in [true, false] {
            let t = frozen_after(spec, alive, 300.0);
            assert!(t.is_some_and(|t| t < 180), "{name} cpu {alive} {t:?}");
        }
    }
    assert!(frozen_after(PlantSpec::MEASURED, true, 40.0).is_none());
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
fn r23_frozen_water_falls_back_at_any_duty_and_stays_an_alarm_with_writes_off() {
    let mut high = running(44.0, |_| {});
    for t in 0..700 {
        high.cpu = if t / 10 % 2 == 0 { 40.0 } else { 70.0 };
        high.tick();
    }
    assert!(high.ctl("d5.fan").fallback.is_some() && high.sim.mode(Spec::FAN) == 1);
    assert!(high.alarm(AlarmKind::WaterFrozen));
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

