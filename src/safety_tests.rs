use super::*;
use crate::model::{Axis, Forecast, Golden, Operating, Planner, Temps};

struct Lin(fn(f64, f64, u32) -> f64);

impl Model for Lin {
    fn observe(&mut self, _: &HashMap<String, f64>, _: i64) {}
    fn identified(&self) -> (bool, bool) {
        (true, true)
    }
    fn warm_secs(&self) -> i64 {
        5000
    }
    fn forecast(&self, hold: &HashMap<String, f64>, rollout: u32, _: u32) -> Result<Forecast, String> {
        let g = |k: &str| hold.get(k).copied().unwrap_or(0.0);
        Ok(Forecast { water: self.0(g("d5next/fan/rpm"), g("d5next/pump/rpm"), rollout), cpu: 0.0, gpu: 0.0 })
    }
}

struct Warming {
    n: i64,
}

impl Model for Warming {
    fn observe(&mut self, _: &HashMap<String, f64>, _: i64) {
        self.n += 1;
    }
    fn identified(&self) -> (bool, bool) {
        (true, true)
    }
    fn warm_secs(&self) -> i64 {
        self.n
    }
    fn forecast(&self, _: &HashMap<String, f64>, _: u32, _: u32) -> Result<Forecast, String> {
        Ok(Forecast { water: 0.0, cpu: 0.0, gpu: 0.0 })
    }
}

fn rig_with(model: impl Model + 'static, tweak: impl Fn(&mut Config)) -> Rig {
    let cfg = base_cfg(tweak);
    let sim = Sim::new();
    let s = sim.clone();
    let factory: Factory = Box::new(move || Ok(Box::new(s.clone()) as Box<dyn D5Control>));
    let c = Control::new(&cfg, Box::new(D5Actuator::new(factory, &cfg.control)), Ok(Box::new(model)));
    Rig { sim, c, now: 0, water: 30.0, cpu: 50.0, gpu: Some(40.0), model_fail: Rc::new(Cell::new(false)), warm: Rc::new(Cell::new(0)), heat: Rc::new(Cell::new(0.0)), extra: vec![], live: false, dir: cfg.state_dir() }
}

fn stepped(secs: f64) -> impl Fn(&mut Config) {
    move |c| c.control.step_test.fan = vec![[100.0, secs], [60.0, secs], [40.0, secs]]
}

#[test]
fn b1_manual_setting_without_marker_is_never_overwritten_but_alarmed() {
    let sim = Sim::new();
    sim.set_raw(|r| r[Spec::PUMP.mode()] = 0);
    let mut rig = Rig::with_sim(sim.clone(), (false, false), 3.0, |_| {});
    rig.run(120);
    assert_eq!((sim.mode(Spec::PUMP), sim.sets()), (0, 0));
    assert!(rig.alarm(AlarmKind::Foreign));
}

#[test]
fn b1_marker_is_set_before_the_first_duty_set_and_cleared_after_release() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.run(5);
    rig.active("d5.fan");
    assert!(rig.marker().is_empty());
    while rig.sim.sets() == 0 {
        rig.tick();
        assert!(rig.now < 60_000);
    }
    assert_eq!(rig.marker(), vec![ControlKind::Fan]);
    rig.c.set_mode("d5.fan", Mode::Shadow, false, rig.now).unwrap();
    assert_eq!((rig.sim.mode(Spec::FAN), rig.marker()), (1, vec![]));
}

#[test]
fn b1_unwritable_marker_blocks_the_duty_set() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    std::fs::write(&rig.dir, "x").unwrap();
    rig.run(5);
    rig.active("d5.fan");
    rig.run(40);
    assert_eq!((rig.sim.sets(), rig.sim.mode(Spec::FAN)), (0, 1));
    assert!(rig.ctl("d5.fan").fallback.unwrap().contains("Besitzmarke"));
}

#[test]
fn h2_writes_off_reads_the_mode_bytes_alarms_and_never_sets() {
    let sim = Sim::new();
    sim.set_raw(|r| r[Spec::FAN.mode()] = 0);
    let mut rig = Rig::with_sim(sim.clone(), (true, true), 3.0, |c| c.writes_enabled = false);
    rig.run(40);
    for m in [Mode::Active, Mode::Off, Mode::Shadow] {
        let _ = rig.c.set_mode("d5.fan", m, true, rig.now);
    }
    assert!(rig.c.start_test("d5.fan", true, rig.now).is_err());
    rig.c.shutdown(rig.now);
    assert!(rig.alarm(AlarmKind::Foreign));
    assert_eq!((sim.sets(), sim.mode(Spec::FAN)), (0, 0));
}

#[test]
fn b2_missing_samples_keep_the_model_warm_and_only_gaps_over_ten_seconds_reset_it() {
    let g = Golden::load();
    let mut m = g.model();
    for t in 3000..g.rows.len() {
        m.observe(&g.row(t), g.k0 + t as i64);
    }
    let (w, last) = (m.warm_secs(), g.k0 + g.rows.len() as i64 - 1);
    assert!(w > 1800);
    m.observe(&HashMap::new(), last + 1);
    assert_eq!(m.warm_secs(), w + 1);
    m.observe(&g.row(g.rows.len() - 1), last + 10);
    assert_eq!(m.warm_secs(), w + 2);
    m.observe(&g.row(g.rows.len() - 1), last + 21);
    assert_eq!(m.warm_secs(), 0);
}

#[test]
fn b2_active_channel_that_is_not_warm_holds_its_command_and_the_bound_keeps_acting() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.run(5);
    rig.active("d5.fan");
    rig.run(40);
    assert_eq!(rig.sim.mode(Spec::FAN), 0);
    let held = rig.ctl("d5.fan").commanded_duty;
    rig.warm.set(1);
    rig.run(100);
    let c = rig.ctl("d5.fan");
    assert!(rig.sim.mode(Spec::FAN) == 0 && c.fallback.is_none() && c.commanded_duty == held && c.note.unwrap().contains("wärmt"));
    assert!(rig.c.set_mode("d5.pump", Mode::Active, true, rig.now).unwrap_err().contains("wärmt"));
    rig.water = 42.0;
    rig.run(10);
    assert_eq!(rig.ctl("d5.fan").commanded_duty, Some(100.0));
}

#[test]
fn b3_foreign_mode_on_one_channel_does_not_block_the_release_of_the_other() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.run(5);
    rig.active("d5.fan");
    rig.active("d5.pump");
    rig.run(40);
    rig.sim.set_raw(|r| r[Spec::PUMP.mode()] = 2);
    rig.water = 48.5;
    rig.run(5);
    rig.water = 30.0;
    rig.run(120);
    assert_eq!((rig.sim.mode(Spec::FAN), rig.sim.mode(Spec::PUMP)), (1, 2));
    assert_eq!((rig.ctl("d5.fan").drive, rig.ctl("d5.pump").drive), (Drive::Firmware, Drive::Firmware));
    let sets = rig.sim.sets();
    rig.run(300);
    rig.c.shutdown(rig.now);
    assert_eq!((rig.sim.sets(), rig.sim.mode(Spec::PUMP)), (sets, 2));
}

#[test]
fn b4_recovery_at_start_is_retried_until_it_succeeds_with_an_alarm_while_pending() {
    for (fail_get, fail_sets) in [(true, 0), (false, 1)] {
        let sim = Sim::new();
        sim.set_raw(|r| r[Spec::FAN.mode()] = 0);
        {
            let mut w = sim.0.borrow_mut();
            (w.fail_get, w.fail_sets) = (fail_get, fail_sets);
        }
        let mut rig = Rig::with_marker(sim.clone(), &[ControlKind::Fan], (true, true), 3.0, |_| {});
        rig.run(2);
        assert_eq!(sim.mode(Spec::FAN), 0);
        assert!(rig.alarm(AlarmKind::Fallback));
        sim.0.borrow_mut().fail_get = false;
        rig.run(60);
        assert_eq!((sim.mode(Spec::FAN), rig.marker()), (1, vec![]));
        assert!(!rig.alarm(AlarmKind::Fallback));
    }
}

#[test]
fn h3_mode_off_stops_a_step_test() {
    let mut rig = Rig::new((false, false), 3.0, stepped(60.0));
    rig.run(5);
    rig.c.start_test("d5.fan", true, rig.now).unwrap();
    rig.run(2);
    assert_eq!(rig.sim.mode(Spec::FAN), 0);
    rig.c.set_mode("d5.fan", Mode::Off, false, rig.now).unwrap();
    let sets = rig.sim.sets();
    rig.run(200);
    assert_eq!((rig.sim.mode(Spec::FAN), rig.sim.sets(), rig.status().step.is_none()), (1, sets, true));
}

#[test]
fn m1_nan_is_rejected_in_the_step_plan_and_in_the_guard() {
    let cfg: ControlConfig = toml::from_str("[step_test]\nfan = [[nan, 600.0]]\n").unwrap();
    assert!(cfg.validate().is_err());
    let sim = Sim::new();
    let mut g = Guard::new(Box::new(sim.clone()));
    assert!(g.write(&[(Spec::FAN, Setting::Duty(f64::NAN))]).is_err());
    assert_eq!(sim.sets(), 0);
}

#[test]
fn m2_hard_minima_cannot_be_configured_away_and_the_guard_enforces_them() {
    for text in ["[fan]\nfloor_duty = -1.0\n", "[pump]\nfloor_duty = 14.0\n", "[pump]\nfloor_duty = 0.0\n", "[fan]\nstart_duty = -1.0\n", "[fan]\nstart_duty = 101.0\n"] {
        assert!(toml::from_str::<ControlConfig>(text).unwrap().validate().is_err(), "{text}");
    }
    assert!(toml::from_str::<ControlConfig>("[fan]\nfloor_duty = 0.0\n[pump]\nfloor_duty = 30.0\n").unwrap().validate().is_ok());
    let fresh = Sim::new().report();
    for (spec, pct, ok) in [(Spec::FAN, 0.0, true), (Spec::FAN, 5.0, true), (Spec::FAN, 100.01, false), (Spec::PUMP, 0.0, false), (Spec::PUMP, 14.0, false), (Spec::PUMP, 14.99, false), (Spec::PUMP, 15.0, true)] {
        let mut t = fresh.clone();
        t[spec.mode()] = 0;
        t[spec.preset()..spec.preset() + 2].copy_from_slice(&((pct * 100.0) as u16).to_be_bytes());
        Crc16Usb::seal(&mut t);
        assert_eq!(Guard::check_diff(&fresh, &t, &[(spec, Setting::Duty(pct))]).is_ok(), ok, "{} {pct}", spec.name);
    }
}

#[test]
fn m3_water_limit_fallback_names_the_firmware_setpoints_without_extra_writes() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.run(5);
    rig.active("d5.fan");
    rig.run(40);
    let sets = rig.sim.sets();
    rig.water = 48.5;
    rig.run(3);
    assert_eq!((rig.sim.mode(Spec::FAN), rig.sim.sets()), (1, sets + 1));
    let f = rig.ctl("d5.fan").fallback.unwrap();
    assert!(f.contains("Firmware-Sollwerte: Lüfter") && f.contains("Pumpe"), "{f}");
}

#[test]
fn m4_autostart_waits_for_the_model_to_warm_up() {
    let mut rig = rig_with(Warming { n: 0 }, |c| c.control.fan.autostart_active = Some(true));
    rig.live = true;
    rig.run(1700);
    assert_eq!(rig.ctl("d5.fan").mode, Mode::Shadow);
    rig.run(300);
    assert_eq!(rig.ctl("d5.fan").mode, Mode::Active);
}

#[test]
fn m5_a_device_change_between_get_and_set_aborts_the_write() {
    let sim = Sim::new();
    sim.0.borrow_mut().on_settle = Some(|r| r[0x46] = 0x0D);
    let mut g = Guard::new(Box::new(sim.clone()));
    let e = g.write(&[(Spec::FAN, Setting::Duty(50.0))]).unwrap_err();
    assert!(e.contains("changed") && sim.sets() == 0 && sim.report()[0x46] == 0x0D, "{e}");
    assert!(g.write(&[(Spec::FAN, Setting::Duty(50.0))]).unwrap());
    assert_eq!(sim.report()[0x46], 0x0D);
}

#[test]
fn m6_daily_write_budget_holds_the_last_duty_with_an_alarm_and_resets_next_day() {
    let mut rig = Rig::new((true, true), 3.0, |c| {
        c.control.fan.daily_writes = Some(3);
        c.control.fan.down_step_duty = Some(0.0);
    });
    rig.live = true;
    rig.run(5);
    rig.active("d5.fan");
    for i in 0..1200 {
        rig.heat.set(if (i / 30) % 2 == 0 { 3.0 } else { 12.0 });
        rig.tick();
    }
    assert_eq!(rig.sim.duty_sets().len(), 3);
    assert!(rig.alarm(AlarmKind::Budget));
    assert_eq!((rig.ctl("d5.fan").mode, rig.sim.mode(Spec::FAN)), (Mode::Active, 0));
    for i in 0..17 * 3600 {
        rig.heat.set(if (i / 30) % 2 == 0 { 3.0 } else { 12.0 });
        rig.tick();
    }
    assert!(rig.sim.duty_sets().len() > 3);
    rig.c.shutdown(rig.now);
    assert_eq!(rig.sim.mode(Spec::FAN), 1);
}

#[test]
fn a_device_that_ignores_sets_is_retried_with_backoff() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.run(5);
    rig.active("d5.fan");
    rig.run(40);
    let sets = rig.sim.sets();
    rig.sim.0.borrow_mut().ignore_sets = true;
    rig.water = 48.5;
    rig.run(3600);
    assert!(rig.sim.sets() - sets <= 80, "{}", rig.sim.sets() - sets);
    rig.sim.0.borrow_mut().ignore_sets = false;
    rig.run(120);
    assert_eq!(rig.sim.mode(Spec::FAN), 1);
}

#[test]
fn shadow_target_is_the_plan_not_the_rate_limited_step() {
    let mut rig = Rig::new((false, false), 3.0, |_| {});
    rig.run(35);
    let c = rig.ctl("d5.fan");
    assert!((c.target_rpm.unwrap() - c.min_rpm).abs() < 1.0 && c.actual_rpm.unwrap() > c.min_rpm + 500.0, "{:?} {:?}", c.target_rpm, c.actual_rpm);
}

fn knife(fan: f64, _pump: f64, _r: u32) -> f64 {
    15.9 - 0.0005 * (fan - 441.0)
}

fn axes<'a>(fc: &'a ChannelConfig, pc: &'a ChannelConfig, fcal: &'a Calibration, pcal: &'a Calibration) -> (Axis<'a>, Axis<'a>) {
    let op = Operating { duty: 90.0, rpm: 1323.0, power: None, current: None };
    (Axis { name: "fan", cfg: fc, cal: fcal, free: true, base: op, zero_ok: true }, Axis { name: "pump", cfg: pc, cal: pcal, free: false, base: Operating { duty: 90.0, rpm: 4400.0, ..op }, zero_ok: false })
}

#[test]
fn plan_changes_need_a_margin_and_missing_inputs_are_not_feasible() {
    let cfg = ControlConfig::default();
    let (fc, pc) = (ChannelConfig::defaults(ControlKind::Fan), ChannelConfig::defaults(ControlKind::Pump));
    let (fcal, pcal) = (fc.calibration(), pc.calibration());
    let (fan, pump) = axes(&fc, &pc, &fcal, &pcal);
    let m = Lin(knife);
    let now = Temps { water: Some(30.0), cpu: Some(50.0), gpu: Some(40.0) };
    let planner = Planner { cfg: &cfg };
    let fresh = planner.plan(&m, &fan, &pump, &now, 0.0, None).unwrap();
    assert!((fresh.fan_rpm - fan.cal.rpm(fc.start_duty)).abs() < 1e-9 && fresh.feasible);
    let kept = planner.plan(&m, &fan, &pump, &now, 0.0, Some((fan.rpm_range().1, 4400.0))).unwrap();
    assert!(kept.fan_rpm > fan.rpm_range().0 + 100.0, "{}", kept.fan_rpm);
    let blind = planner.plan(&m, &fan, &pump, &Temps { cpu: None, ..now }, 0.0, None).unwrap();
    assert!(!blind.feasible);
}

#[test]
fn grid_top_is_always_the_maximum_rpm() {
    let pc = ChannelConfig::defaults(ControlKind::Pump);
    let pcal = pc.calibration();
    for floor in [20.0, 31.0, 33.3, 47.0, 99.0] {
        let fc = ChannelConfig { floor_duty: floor, ..ChannelConfig::defaults(ControlKind::Fan) };
        let fcal = fc.calibration();
        let (fan, _) = axes(&fc, &pc, &fcal, &pcal);
        for n in [2, 5, 7, 16] {
            assert_eq!(*fan.candidates(n).last().unwrap(), fan.rpm_range().1);
        }
    }
}

#[test]
fn the_cpu_input_is_a_sustained_value_and_the_shown_limit_is_the_one_used() {
    let mut rig = Rig::new((false, false), 3.0, |_| {});
    rig.run(60);
    rig.cpu = 100.0;
    rig.tick();
    assert!(rig.c.temps(rig.now).cpu == Some(100.0) && rig.c.plan_temps(rig.now).cpu.unwrap() < 55.0);
    rig.cpu = 50.0;
    rig.run(35);
    let p = rig.status().predicted.unwrap();
    assert!((p.water_limit - (ControlConfig::default().water_plan_limit - 0.5 * 5.0)).abs() < 1e-9);
}

#[test]
fn a_fan_only_rollout_holds_the_pump_at_its_actual_speed() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.run(5);
    rig.active("d5.fan");
    rig.run(40);
    let p = rig.ctl("d5.pump");
    assert!((p.target_rpm.unwrap() - p.actual_rpm.unwrap()).abs() < 1.0, "{:?} {:?}", p.target_rpm, p.actual_rpm);
}

#[test]
fn unidentified_response_is_flagged_in_notes_and_telemetry_in_every_mode() {
    let mut rig = Rig::new((false, false), 3.0, |_| {});
    rig.run(34);
    let rows = rig.tick();
    assert!(rig.ctl("d5.fan").note.unwrap().contains("nicht identifiziert"));
    let id = rows.iter().find(|r| r.channel == "daisyloop/fan/identified").unwrap();
    assert_eq!(id.value, 0.0);
    let mut known = Rig::new((true, true), 3.0, |_| {});
    known.run(35);
    assert!(!known.ctl("d5.fan").note.unwrap().contains("nicht identifiziert"));
}

#[test]
fn the_forecast_shown_is_direct_and_the_plan_estimate_is_labelled_with_its_own_lookahead() {
    let mut rig = rig_with(Lin(|_, _, r| r as f64 / 100.0), |_| {});
    rig.run(35);
    let rows = rig.tick();
    let p = rig.status().predicted.unwrap();
    assert_eq!((p.water, p.horizon_s), (Some(30.0), 120));
    let v = |ch: &str| rows.iter().find(|r| r.channel == ch).map(|r| r.value);
    assert_eq!((v("daisyloop/forecast/water"), v("daisyloop/plan/water"), v("daisyloop/plan/ahead")), (Some(30.0), Some(31.2), Some(240.0)));
}

#[test]
fn sparse_input_never_counts_as_warm() {
    let mut m = Golden::load().model();
    let row: HashMap<String, f64> = [("d5next/water/temp".to_string(), 31.0)].into();
    for k in 0..2000 {
        m.observe(&row, 1_800_000_000 + k);
    }
    assert_eq!(m.warm_secs(), 0);
}

#[test]
fn one_missing_sample_at_the_decision_tick_is_forward_filled_through_the_plan() {
    let g = Golden::load();
    let (mut b, mut c) = (g.model(), g.model());
    let n = g.rows.len();
    for t in 3000..n - 1 {
        b.observe(&g.row(t), g.k0 + t as i64);
        c.observe(&g.row(t), g.k0 + t as i64);
    }
    let cut = |k: &String| k.starts_with("/intelcpu/0/temperature/") || k.starts_with("/intelcpu/0/power");
    let mut gap = g.row(n - 1);
    gap.retain(|k, _| !cut(k));
    let mut filled = g.row(n - 1);
    for (k, v) in g.row(n - 2) {
        if cut(&k) {
            filled.insert(k, v);
        }
    }
    b.observe(&gap, g.k0 + n as i64 - 1);
    c.observe(&filled, g.k0 + n as i64 - 1);
    let hold: HashMap<String, f64> = HashMap::new();
    let (fb, fc) = (b.forecast(&hold, 120, 120).unwrap(), c.forecast(&hold, 120, 120).unwrap());
    assert!((fb.cpu - fc.cpu).abs() < 1e-9 && (fb.water - fc.water).abs() < 1e-9 && (fb.gpu - fc.gpu).abs() < 1e-9, "{fb:?} {fc:?}");
}
