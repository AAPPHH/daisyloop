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
    Rig { sim, c, now: 0, water: 30.0, cpu: 50.0, gpu: Some(40.0), model_fail: Rc::new(Cell::new(false)), warm: Rc::new(Cell::new(0)), heat: Rc::new(Cell::new(0.0)), dir: cfg.state_dir() }
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
        assert!((bound(&r, 0) - fan).abs() < 1e-6 && (bound(&r, 1) - pump).abs() < 1e-6, "{w}");
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
    let mut rig = running(32.0, |_| {});
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
    let mut high = running(44.0, |_| {});
    for t in 0..700 {
        high.cpu = if t / 10 % 2 == 0 { 40.0 } else { 70.0 };
        high.tick();
    }
    assert!(duty(&high, 0).unwrap() > 90.0 && high.ctl("d5.fan").fallback.is_none() && high.sim.mode(Spec::FAN) == 0);
    assert!(high.alarm(AlarmKind::WaterFrozen));
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

struct Plant {
    c: f64,
    t: f64,
    fan: VecDeque<f64>,
}

impl Plant {
    const G_OFF: f64 = 8.0;
    const G_FULL: f64 = 25.0;
    const AMBIENT: f64 = 26.0;
    const DEAD_SECS: usize = 31;

    fn step(&mut self, power: f64, fan_rpm: f64, pump_rpm: f64) {
        self.fan.push_back(fan_rpm);
        let delayed = if self.fan.len() > Self::DEAD_SECS { self.fan.pop_front().unwrap() } else { self.fan[0] };
        let g = (Self::G_OFF + (Self::G_FULL - Self::G_OFF) * (delayed / 1470.0).powf(0.7)) * (0.9 + 0.1 * pump_rpm / 4800.0);
        self.t += (power - g * (self.t - Self::AMBIENT)) / self.c;
    }
}

fn simulate(power: f64, c: f64, tweak: impl Fn(&mut Config)) -> (usize, usize, f64) {
    let mut rig = blind_rig(tweak);
    let mut plant = Plant { c, t: 30.0, fan: VecDeque::new() };
    rig.water = plant.t;
    rig.run(5);
    rig.active("d5.fan");
    rig.active("d5.pump");
    let (fan_cal, pump_cal) = (ChannelConfig::defaults(ControlKind::Fan).calibration(), ChannelConfig::defaults(ControlKind::Pump).calibration());
    let mut peak: f64 = 0.0;
    for _ in 0..86_400 {
        let r = rig.sim.report();
        let rpm = |s: Spec, cal: &Calibration| cal.rpm(if r[s.mode()] == 0 { be16(&r, s.preset()) as f64 / 100.0 } else { 90.0 });
        plant.step(power, rpm(Spec::FAN, &fan_cal), rpm(Spec::PUMP, &pump_cal));
        peak = peak.max(plant.t);
        rig.water = (plant.t * 100.0).round() / 100.0;
        rig.tick();
    }
    let sets = rig.sim.duty_sets();
    let fan: Vec<u16> = sets.iter().map(|(_, r)| be16(r, Spec::FAN.preset())).collect();
    let stops = fan.windows(2).filter(|w| w[0] != 0 && w[1] == 0).count();
    (stops, sets.len(), peak)
}

fn without_rules(c: &mut Config) {
    c.control.fan.min_run_secs = Some(0.0);
    c.control.fan.min_curve = Some(vec![[35.0, 0.0], [36.0, 0.0]]);
    c.control.pump.min_curve = Some(vec![[35.0, 15.0], [36.0, 15.0]]);
}

#[test]
fn r12_the_dwell_cuts_the_stops_and_the_bound_keeps_the_writes_within_the_budget() {
    let (before, after) = (simulate(70.0, 4_000.0, without_rules), simulate(70.0, 4_000.0, |_| {}));
    assert!(before.0 > 40 && after.0 < before.0 && after.1 <= 500, "{before:?} {after:?}");
    let (before, after) = (simulate(300.0, 11_900.0, without_rules), simulate(300.0, 11_900.0, |_| {}));
    assert!(before.1 > 500 && after.1 <= 500 && after.2 < 48.0, "{before:?} {after:?}");
}

#[test]
#[ignore]
fn r13_cycling_table_per_24_h() {
    let mut table = String::from("C J/K | W | stops, duty writes, peak C without bound and dwell | with
");
    for c in [11_900.0, 4_000.0] {
        for power in [30.0, 70.0, 100.0, 200.0, 300.0] {
            let (before, after) = (simulate(power, c, without_rules), simulate(power, c, |_| {}));
            table.push_str(&format!("{c:.0} | {power:.0} | {} {} {:.1} | {} {} {:.1}
", before.0, before.1, before.2, after.0, after.1, after.2));
            assert!(after.1 <= 500 && after.2 < 48.0, "{c} {power} {after:?}");
        }
    }
    eprintln!("{table}");
}
