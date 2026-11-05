use super::*;
use crate::model::{Axis, Golden, Operating, Planner, Temps};

fn stopped_fan(tweak: impl Fn(&mut Config)) -> Rig {
    let mut rig = Rig::new((true, true), 3.0, tweak);
    rig.water = 32.0;
    rig.run(5);
    rig.active("d5.fan");
    rig.run(480);
    assert_eq!(rig.ctl("d5.fan").commanded_duty, Some(0.0));
    rig
}

fn actuator(sim: &Sim, cfg: &ControlConfig) -> D5Actuator {
    let s = sim.clone();
    D5Actuator::new(Box::new(move || Ok(Box::new(s.clone()) as Box<dyn D5Control>)), cfg)
}

#[test]
fn z1_dead_zone_snaps_to_the_closer_of_zero_and_the_start_duty() {
    let fan = ChannelConfig::defaults(ControlKind::Fan);
    for (duty, zero_ok, want) in [(0.0, true, 0.0), (4.0, true, 0.0), (9.9, true, 0.0), (10.0, true, 20.0), (15.0, true, 20.0), (19.9, true, 20.0), (20.0, true, 20.0), (55.5, true, 55.5), (-3.0, true, 0.0), (400.0, true, 100.0), (4.0, false, 20.0), (0.0, false, 20.0), (50.0, false, 50.0)] {
        assert_eq!(fan.commandable(duty, zero_ok), want, "{duty} {zero_ok}");
    }
    let raised = ChannelConfig { floor_duty: 5.0, ..fan.clone() };
    assert_eq!((raised.commandable(0.0, true), raised.commandable(3.0, true), raised.commandable(30.0, true)), (20.0, 20.0, 30.0));
    let pump = ChannelConfig::defaults(ControlKind::Pump);
    assert_eq!((pump.commandable(0.0, true), pump.commandable(35.0, true), pump.commandable(60.0, false)), (40.0, 40.0, 60.0));
}

#[test]
fn z2_every_duty_path_sends_the_snapped_value_and_reports_it() {
    let mut rig = Rig::new((false, false), 3.0, |c| c.control.step_test.fan = vec![[5.0, 30.0], [15.0, 30.0], [0.0, 30.0], [60.0, 30.0]]);
    rig.run(5);
    rig.c.start_test("d5.fan", true, rig.now).unwrap();
    let mut seen = Vec::new();
    for _ in 0..125 {
        rig.tick();
        if let Some(s) = rig.status().step {
            seen.push(s.duty);
        }
    }
    let presets: Vec<u16> = rig.sim.duty_sets().iter().map(|(_, r)| be16(r, Spec::FAN.preset())).collect();
    assert_eq!(presets, vec![0, 2000, 0, 6000]);
    assert!(seen.iter().all(|d| [0.0, 20.0, 60.0].contains(d)), "{seen:?}");
}

#[test]
fn z3_the_planner_stops_the_fan_only_below_the_stop_water_and_holds_it_until_the_start_water() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.water = 34.0;
    rig.run(5);
    rig.active("d5.fan");
    rig.run(500);
    assert!(rig.sim.duty_sets().iter().all(|(_, r)| be16(r, Spec::FAN.preset()) >= 2000), "running fan stays above the start duty at 34 C");
    rig.water = 32.0;
    rig.run(480);
    assert_eq!((rig.ctl("d5.fan").commanded_duty, rig.sim.preset(Spec::FAN)), (Some(0.0), 0));
    rig.water = 34.5;
    rig.heat.set(1.0);
    rig.run(200);
    assert_eq!((rig.ctl("d5.fan").commanded_duty, rig.sim.preset(Spec::FAN)), (Some(0.0), 0), "a stopped fan stays stopped between the stop and the start water");
    rig.water = 35.5;
    rig.run(3);
    assert!(rig.ctl("d5.fan").commanded_duty.unwrap() >= 20.0 && rig.sim.preset(Spec::FAN) >= 2000);
}

#[test]
fn z4_a_restart_leaves_zero_at_the_start_duty_without_waiting_for_the_rate_limit() {
    let mut rig = stopped_fan(|c| c.control.fan.step_up_rpm = Some(50.0));
    let n = rig.sim.duty_sets().len();
    rig.water = 35.5;
    rig.run(2);
    assert_eq!(rig.sim.duty_sets().len(), n + 1, "restart does not wait for the decision interval");
    assert_eq!(rig.ctl("d5.fan").commanded_duty, Some(20.0));
    assert_eq!(rig.sim.preset(Spec::FAN), 2000);
    assert!(rig.ctl("d5.fan").target_rpm.unwrap() > 290.0);
}

#[test]
fn z5_stale_or_missing_water_never_means_stopped() {
    let cfg = ControlConfig::default();
    assert!(cfg.fan_may_stop(Some(32.9), false) && !cfg.fan_may_stop(Some(33.0), false) && cfg.fan_may_stop(Some(34.9), true) && !cfg.fan_may_stop(Some(35.0), true));
    assert!(!cfg.fan_may_stop(None, false) && !cfg.fan_may_stop(None, true) && !cfg.fan_may_stop(Some(f64::NAN), true));
    let fc = ChannelConfig::defaults(ControlKind::Fan);
    let fcal = fc.calibration();
    let op = Operating { duty: 90.0, rpm: 1323.0, power: None, current: None };
    let mut fan = Axis { name: "fan", cfg: &fc, cal: &fcal, free: true, base: op, zero_ok: true };
    assert_eq!(fan.candidates(7)[0], 0.0);
    fan.zero_ok = false;
    let c = fan.candidates(7);
    assert!((c[0] - fcal.rpm(20.0)).abs() < 1e-9 && c.iter().all(|r| *r > 0.0) && c.windows(2).all(|w| w[1] > w[0]));
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.water = 32.0;
    rig.run(40);
    assert_eq!(rig.ctl("d5.fan").target_rpm, Some(0.0));
    rig.water = f64::NAN;
    rig.run(45);
    assert!(rig.ctl("d5.fan").target_rpm.is_none_or(|t| t > 0.0));
    let mut live = stopped_fan(|_| {});
    live.water = f64::NAN;
    live.run(12);
    assert_eq!(live.sim.mode(Spec::FAN), 1);
}

#[test]
fn z6_water_limit_releases_a_stopped_fan_to_the_firmware_pid() {
    let mut rig = stopped_fan(|_| {});
    let sets = rig.sim.sets();
    rig.water = 40.5;
    rig.run(3);
    assert_eq!((rig.sim.mode(Spec::FAN), rig.sim.sets()), (1, sets + 1));
    let c = rig.ctl("d5.fan");
    assert!(c.fallback.is_some() && c.drive == Drive::Firmware && c.commanded_duty.is_none());
    assert!(rig.alarm(AlarmKind::WaterHigh) && rig.alarm(AlarmKind::Fallback));
}

#[test]
fn z7_commanded_zero_with_a_stopped_fan_is_no_stall_and_a_nonzero_command_without_rpm_is() {
    let mut rig = stopped_fan(|_| {});
    rig.run(200);
    assert!(!rig.alarm(AlarmKind::FanStopped) && rig.ctl("d5.fan").fallback.is_none() && rig.ctl("d5.fan").actual_rpm == Some(0.0));
    rig.sim.0.borrow_mut().force = [Some(300.0), None];
    rig.run(150);
    assert!(!rig.alarm(AlarmKind::FanStopped) && rig.ctl("d5.fan").fallback.is_none(), "a slow spin-down is tolerated");
    let mut run = Rig::new((true, true), 3.0, |_| {});
    run.water = 34.0;
    run.run(5);
    run.active("d5.fan");
    run.run(40);
    assert!(run.ctl("d5.fan").commanded_duty.unwrap() >= 20.0 && !run.alarm(AlarmKind::FanStopped));
    run.sim.0.borrow_mut().force = [Some(0.0), None];
    run.run(8);
    assert!(!run.alarm(AlarmKind::FanStopped), "inside the hold time");
    run.run(8);
    assert!(run.alarm(AlarmKind::FanStopped));
    run.run(80);
    assert_eq!(run.sim.mode(Spec::FAN), 1);
    assert!(run.ctl("d5.fan").fallback.unwrap().contains("folgt"));
    let mut start = stopped_fan(|_| {});
    start.sim.0.borrow_mut().force = [Some(0.0), None];
    start.water = 35.5;
    start.run(3);
    assert!(start.ctl("d5.fan").commanded_duty.unwrap() >= 20.0);
    start.run(18);
    assert!(!start.alarm(AlarmKind::FanStopped), "the settle time after the command has not passed");
    start.run(45);
    assert!(start.alarm(AlarmKind::FanStopped));
}

#[test]
fn z8_shadow_mode_computes_the_snapped_stop_and_sends_nothing() {
    let mut rig = Rig::new((false, false), 3.0, |_| {});
    rig.water = 32.0;
    rig.run(200);
    assert_eq!(rig.sim.sets(), 0);
    let c = rig.ctl("d5.fan");
    assert!(c.mode == Mode::Shadow && c.target_rpm == Some(0.0) && c.commanded_duty.is_none() && c.drive == Drive::Firmware);
    let rows = rig.tick();
    assert!(rows.iter().any(|r| r.channel == "daisyloop/fan/target_rpm" && r.value == 0.0));
    assert!(!rows.iter().any(|r| r.channel == "daisyloop/fan/duty_cmd"));
    rig.water = 34.0;
    rig.run(70);
    assert!(rig.ctl("d5.fan").target_rpm.unwrap() >= rig.c.chans[0].cal.rpm(20.0) - 1e-9);
    assert_eq!((rig.sim.sets(), rig.sim.mode(Spec::FAN)), (0, 1));
}

#[test]
fn z9_the_pump_keeps_its_floor_hard_minimum_and_never_stops() {
    let sim = Sim::new();
    let mut a = actuator(&sim, &ControlConfig::default());
    for duty in [0.0, 29.0, 35.0, f64::MIN] {
        a.apply(&[(ControlKind::Pump, Setting::Duty(duty))]).unwrap();
        assert_eq!(sim.preset(Spec::PUMP), 4000, "{duty}");
    }
    let mut low: ControlConfig = toml::from_str("[pump]\nfloor_duty = 30.0\n").unwrap();
    actuator(&sim, &low).apply(&[(ControlKind::Pump, Setting::Duty(0.0))]).unwrap();
    assert_eq!(sim.preset(Spec::PUMP), 3000);
    low.pump.floor_duty = Some(25.0);
    assert!(low.validate().is_err());
    actuator(&sim, &low).apply(&[(ControlKind::Pump, Setting::Duty(0.0))]).unwrap();
    assert_eq!(sim.preset(Spec::PUMP), 4000);
    let mut g = Guard::new(Box::new(sim.clone()));
    assert!(g.write(&[(Spec::PUMP, Setting::Duty(29.0))]).is_err() && g.write(&[(Spec::PUMP, Setting::Duty(0.0))]).is_err());
    assert!(g.write(&[(Spec::FAN, Setting::Duty(0.0))]).is_ok());
    assert_eq!(sim.preset(Spec::FAN), 0);
}

#[test]
fn z10_calibration_and_the_features_cope_with_rpm_zero_and_duty_zero() {
    let fan = ChannelConfig::defaults(ControlKind::Fan).calibration();
    assert_eq!((fan.rpm(0.0), fan.duty(0.0), fan.duty(-5.0), fan.rpm(-5.0)), (0.0, 0.0, 0.0, 0.0));
    let flat = Calibration::new(&[[0.0, 0.0], [10.0, 0.0], [100.0, 1000.0]]).unwrap();
    assert_eq!((flat.rpm(5.0), flat.duty(0.0)), (0.0, 0.0));
    assert!((0.0..=100.0).contains(&flat.duty(1.0)) && flat.duty(5000.0) == 100.0);
    let pump = ChannelConfig::defaults(ControlKind::Pump).calibration();
    assert_eq!((pump.rpm(0.0), pump.duty(0.0)), (2000.0, 0.0));
    let g = Golden::load();
    let mut m = g.model();
    for t in 3000..g.rows.len() {
        m.observe(&g.row(t), g.k0 + t as i64);
    }
    let (fc, pc) = (ChannelConfig::defaults(ControlKind::Fan), ChannelConfig::defaults(ControlKind::Pump));
    let (fcal, pcal) = (fc.calibration(), pc.calibration());
    let cfg = ControlConfig::default();
    let now = Temps { water: Some(31.0), cpu: Some(70.0), gpu: Some(55.0) };
    for (rpm, duty) in [(0.0, 0.0), (0.5, 0.0), (3.0, 1.0), (1200.0, 80.0)] {
        let op = Operating { duty, rpm, power: Some(0.02), current: Some(0.01) };
        let fan = Axis { name: "fan", cfg: &fc, cal: &fcal, free: true, base: op, zero_ok: true };
        let pump = Axis { name: "pump", cfg: &pc, cal: &pcal, free: true, base: Operating { duty: 50.0, rpm: 3400.0, ..op }, zero_ok: false };
        let plan = Planner { cfg: &cfg }.plan(&m, &fan, &pump, &now, 0.5, Some((0.0, 3132.0))).unwrap();
        assert!([plan.fan_rpm, plan.pump_rpm, plan.water.unwrap(), plan.cpu.unwrap(), plan.gpu.unwrap()].iter().all(|v| v.is_finite()), "{rpm} {plan:?}");
        assert!(plan.fan_rpm == 0.0 || plan.fan_rpm >= fcal.rpm(20.0) - 1e-9);
    }
    let hold: HashMap<String, f64> = [("d5next/fan/rpm", 0.0), ("d5next/fan/duty", 0.0), ("d5next/fan/power", 0.0), ("d5next/fan/current", 0.0)].into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    let f = m.forecast(&hold, 120, 120).unwrap();
    assert!(f.water.is_finite() && f.cpu.is_finite() && f.gpu.is_finite());
}

#[test]
fn z11_stop_and_start_water_must_be_ordered_below_the_plan_limit() {
    for bad in ["fan_stop_water = 36.0", "fan_start_water = 38.0", "fan_stop_water = 35.0", "fan_start_water = 30.0"] {
        assert!(toml::from_str::<ControlConfig>(bad).unwrap().validate().is_err(), "{bad}");
    }
    assert!(toml::from_str::<ControlConfig>("fan_stop_water = 30.0\nfan_start_water = 36.0").unwrap().validate().is_ok());
    let (d, f) = (ControlConfig::default(), ChannelConfig::defaults(ControlKind::Fan));
    assert_eq!((d.fan_stop_water, d.fan_start_water, f.start_duty, f.floor_duty), (33.0, 35.0, 20.0, 0.0));
}
