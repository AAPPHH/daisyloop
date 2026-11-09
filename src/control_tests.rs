use super::*;
use crate::d5::*;
use crate::protocol::AlarmKind;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

struct World {
    report: Vec<u8>,
    sets: Vec<(i64, Vec<u8>)>,
    fail_get: bool,
    fail_sets: u32,
    corrupt_readback: bool,
    freeze_rpm: bool,
    force: [Option<f64>; 2],
    opens: u32,
    now: i64,
    ignore_sets: bool,
    on_settle: Option<fn(&mut Vec<u8>)>,
}

#[derive(Clone)]
struct Sim(Rc<RefCell<World>>);

impl Sim {
    fn new() -> Sim {
        let mut r: Vec<u8> = (0..Guard::LEN).map(|i| (i * 7 % 251) as u8).collect();
        r[0] = 3;
        for s in Spec::ALL {
            r[s.mode()] = 1;
            r[s.preset()..s.preset() + 2].copy_from_slice(&10000u16.to_be_bytes());
        }
        Crc16Usb::seal(&mut r);
        Sim(Rc::new(RefCell::new(World { report: r, sets: vec![], fail_get: false, fail_sets: 0, corrupt_readback: false, freeze_rpm: false, force: [None, None], opens: 0, now: 0, ignore_sets: false, on_settle: None })))
    }
    fn report(&self) -> Vec<u8> {
        self.0.borrow().report.clone()
    }
    fn mode(&self, s: Spec) -> u8 {
        self.0.borrow().report[s.mode()]
    }
    fn preset(&self, s: Spec) -> u16 {
        be16(&self.0.borrow().report, s.preset())
    }
    fn sets(&self) -> usize {
        self.0.borrow().sets.len()
    }
    fn duty_sets(&self) -> Vec<(i64, Vec<u8>)> {
        self.0.borrow().sets.iter().filter(|(_, r)| Spec::ALL.iter().any(|s| r[s.mode()] == 0)).cloned().collect()
    }
    fn set_raw(&self, f: impl Fn(&mut Vec<u8>)) {
        let mut w = self.0.borrow_mut();
        f(&mut w.report);
        Crc16Usb::seal(&mut w.report);
    }
}

impl D5Control for Sim {
    fn get(&mut self) -> Result<Vec<u8>, String> {
        let w = self.0.borrow();
        if w.fail_get {
            return Err("device gone".into());
        }
        Ok(w.report.clone())
    }
    fn settle(&mut self, _: u64) {
        let mut w = self.0.borrow_mut();
        if let Some(f) = w.on_settle.take() {
            f(&mut w.report);
            Crc16Usb::seal(&mut w.report);
        }
    }
    fn set(&mut self, report: &[u8]) -> Result<(), String> {
        let mut w = self.0.borrow_mut();
        if w.fail_sets > 0 {
            w.fail_sets -= 1;
            return Err("set failed".into());
        }
        let now = w.now;
        w.sets.push((now, report.to_vec()));
        if w.ignore_sets {
            return Ok(());
        }
        w.report = report.to_vec();
        if w.corrupt_readback {
            w.report[0x10] ^= 1;
            Crc16Usb::seal(&mut w.report);
        }
        Ok(())
    }
}

struct Fake {
    ident: (bool, bool),
    warm: Rc<Cell<i64>>,
    heat: Rc<Cell<f64>>,
    fail: Rc<Cell<bool>>,
}

impl Model for Fake {
    fn observe(&mut self, _: &HashMap<String, f64>, _: i64) {}
    fn identified(&self) -> (bool, bool) {
        self.ident
    }
    fn ready(&self) -> bool {
        self.warm.get() > 0
    }
    fn forecast(&self, hold: &HashMap<String, f64>, _: u32, _: u32) -> Result<crate::model::Forecast, String> {
        if self.fail.get() {
            return Err("boom".into());
        }
        let (f, p) = (hold.get("d5next/fan/rpm").copied().unwrap_or(1470.0), hold.get("d5next/pump/rpm").copied().unwrap_or(4800.0));
        Ok(crate::model::Forecast::plain(self.heat.get() - 4.0 * f / 1470.0 - (p - 2000.0) / 2800.0))
    }
}

struct Rig {
    sim: Sim,
    c: Control,
    now: i64,
    water: f64,
    cpu: f64,
    gpu: Option<f64>,
    model_fail: Rc<Cell<bool>>,
    warm: Rc<Cell<i64>>,
    heat: Rc<Cell<f64>>,
    extra: Vec<(&'static str, f64)>,
    live: bool,
    dir: std::path::PathBuf,
}

struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
        let _ = std::fs::remove_file(&self.0);
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        drop(TempDir(self.dir.clone()));
    }
}

fn base_cfg(tweak: impl Fn(&mut Config)) -> Config {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!("daisyloop-test-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
    let mut cfg = Config { writes_enabled: true, data_dir: dir, ..Config::default() };
    cfg.control.decision_secs = 30.0;
    cfg.control.plan_secs = 30.0;
    (cfg.control.target_min, cfg.control.target_max) = (30.0, 44.0);
    cfg.control.water_target = Some(43.5);
    cfg.control.fan_start_water = Some(35.0);
    cfg.control.fan_stop_water = Some(33.0);
    tweak(&mut cfg);
    cfg
}

impl Rig {
    fn new(ident: (bool, bool), heat: f64, tweak: impl Fn(&mut Config)) -> Rig {
        Rig::with_sim(Sim::new(), ident, heat, tweak)
    }
    fn with_sim(sim: Sim, ident: (bool, bool), heat: f64, tweak: impl Fn(&mut Config)) -> Rig {
        Rig::with_marker(sim, &[], ident, heat, tweak)
    }
    fn with_marker(sim: Sim, owned: &[ControlKind], ident: (bool, bool), heat: f64, tweak: impl Fn(&mut Config)) -> Rig {
        let cfg = base_cfg(tweak);
        if !owned.is_empty() {
            Ownership::new(cfg.state_dir()).store(owned).unwrap();
        }
        Rig::build(cfg, sim, ident, heat)
    }
    fn reborn(&self, ident: (bool, bool), tweak: impl Fn(&mut Config)) -> Rig {
        let mut cfg = base_cfg(tweak);
        cfg.data_dir = self.dir.clone();
        Rig::build(cfg, self.sim.clone(), ident, 3.0)
    }
    fn build(cfg: Config, sim: Sim, ident: (bool, bool), heat: f64) -> Rig {
        let s = sim.clone();
        let factory: Factory = Box::new(move || {
            s.0.borrow_mut().opens += 1;
            Ok(Box::new(s.clone()) as Box<dyn D5Control>)
        });
        let model_fail = Rc::new(Cell::new(false));
        let warm = Rc::new(Cell::new(5000));
        let heat = Rc::new(Cell::new(heat));
        let model = Fake { ident, warm: warm.clone(), heat: heat.clone(), fail: model_fail.clone() };
        let c = Control::new(&cfg, Box::new(D5Actuator::new(factory, &cfg.control)), Ok(Box::new(model)));
        Rig { sim, c, now: 0, water: 30.0, cpu: 50.0, gpu: Some(40.0), model_fail, warm, heat, extra: vec![], live: false, dir: cfg.state_dir() }
    }
    fn rows(&self) -> Vec<Row> {
        let r = self.sim.report();
        let mut rows = vec![
            Row::new("d5next", "d5next/water/temp", "water", "temperature", "degC", self.water + if self.live { (self.now / 1000 % 2) as f64 * 0.05 } else { 0.0 }),
            Row::new("lhm", "/intelcpu/0/temperature/0", "cpu", "temperature", "degC", self.cpu),
        ];
        if let Some(g) = self.gpu {
            rows.push(Row::new("nvml", "nvml/0/temp", "gpu", "temperature", "degC", g));
        }
        for (ch, v) in &self.extra {
            rows.push(Row::new("test", *ch, *ch, "power", "W", *v));
        }
        let (frozen, force) = (self.sim.0.borrow().freeze_rpm, self.sim.0.borrow().force);
        for (n, (s, curve)) in [(Spec::FAN, ChannelConfig::defaults(ControlKind::Fan).calibration()), (Spec::PUMP, ChannelConfig::defaults(ControlKind::Pump).calibration())].into_iter().enumerate() {
            let duty = if r[s.mode()] == 0 { be16(&r, s.preset()) as f64 / 100.0 } else { 90.0 };
            let rpm = force[n].unwrap_or(if frozen { curve.rpm(100.0) } else { curve.rpm(duty) });
            rows.push(Row::new("d5next", format!("d5next/{}/duty", s.name), "duty", "duty", "%", duty));
            rows.push(Row::new("d5next", format!("d5next/{}/rpm", s.name), "rpm", "rpm", "rpm", rpm));
        }
        rows
    }
    fn tick(&mut self) -> Vec<Row> {
        self.now += 1000;
        self.sim.0.borrow_mut().now = self.now;
        let mut rows = self.rows();
        self.c.step(&mut rows, self.now, 1_800_000_000 + self.now / 1000);
        rows
    }
    fn run(&mut self, secs: i64) {
        for _ in 0..secs {
            self.tick();
        }
    }
    fn active(&mut self, id: &str) {
        self.c.set_mode(id, Mode::Model, true, self.now).unwrap();
    }
    fn once(&mut self, id: &str) {
        self.active(id);
        let i = self.c.ix(id).unwrap();
        self.c.chans[i].chosen = Mode::Pid;
    }
    fn status(&self) -> Status {
        self.c.status(self.now)
    }
    fn ctl(&self, id: &str) -> PControl {
        self.status().controls.into_iter().find(|c| c.id == id).unwrap()
    }
    fn marker(&self) -> Vec<ControlKind> {
        Ownership::new(self.dir.clone()).read()
    }
    fn alarm(&self, k: AlarmKind) -> bool {
        self.status().alarms.iter().any(|a| a.kind == k)
    }
}

#[test]
fn calibration_interpolates_both_ways() {
    let pump = ChannelConfig::defaults(ControlKind::Pump).calibration();
    assert!((pump.rpm(100.0) - 4800.0).abs() < 1e-9 && (pump.rpm(19.8) - 2570.0).abs() < 1e-9);
    assert!((pump.duty(2570.0) - 19.8).abs() < 1e-6 && pump.duty(9999.0) == 100.0 && pump.duty(0.0) == 0.0);
    let fan = ChannelConfig::defaults(ControlKind::Fan).calibration();
    assert!((fan.duty(735.0) - 50.0).abs() < 1e-9);
    assert!(Calibration::new(&[[0.0, 5.0], [10.0, 4.0]]).is_err() && Calibration::new(&[[0.0, 5.0]]).is_err());
}

#[test]
fn channel_config_defaults_are_per_channel_and_invalid_values_are_rejected() {
    let cfg: ControlConfig = toml::from_str("[fan]\nfloor_duty = 35.0\n").unwrap();
    assert_eq!(ChannelConfig::resolve(ControlKind::Fan, &cfg.fan).unwrap().floor_duty, 35.0);
    let pump = ChannelConfig::resolve(ControlKind::Pump, &cfg.pump).unwrap();
    assert_eq!((pump.floor_duty, pump.alarm_rpm), (15.0, 1800.0));
    assert!(cfg.validate().is_ok() && ControlConfig::default().validate().is_ok());
    for bad in ["[fan]\nfloor_duty = 90.0\nmax_duty = 50.0", "[pump]\nstep_down_rpm = -1.0", "[fan]\ncurve = [[0.0, 5.0]]", "decision_secs = 1.0", "target_max = 46.0", "target_min = 40.0", "water_target = 50.0", "start_offset = 3.0", "fan_stop_water = 36.0\nfan_start_water = 35.0", "[step_test]\nfan = [[100.0, 5.0]]", "[step_test]\npump = [[10.0, 60.0]]"] {
        let c: ControlConfig = toml::from_str(bad).unwrap();
        assert!(c.validate().is_err(), "{bad}");
    }
}

#[test]
fn invalid_config_falls_back_to_defaults_instead_of_panicking() {
    let rig = Rig::new((true, true), 5.0, |c| {
        c.control.fan.floor_duty = Some(80.0);
        c.control.fan.max_duty = Some(40.0);
    });
    assert_eq!(rig.ctl("d5.fan").floor_duty, 0.0);
}

#[test]
fn guard_touches_only_the_requested_channel_bytes() {
    let sim = Sim::new();
    let before = sim.report();
    let mut g = Guard::new(Box::new(sim.clone()));
    assert!(g.write(&[(Spec::FAN, Setting::Duty(55.0))]).unwrap());
    let after = sim.report();
    let changed: Vec<usize> = (0..after.len()).filter(|i| after[*i] != before[*i]).collect();
    assert_eq!(changed, vec![0x41, 0x42, 0x43, Guard::CRC, Guard::CRC + 1]);
    assert_eq!((sim.mode(Spec::FAN), sim.preset(Spec::FAN)), (0, 5500));
    assert_eq!(&after[Spec::PUMP.base..Spec::PUMP.base + 0x55], &before[Spec::PUMP.base..Spec::PUMP.base + 0x55]);
    assert!(g.write(&[(Spec::FAN, Setting::Firmware)]).unwrap());
    assert_eq!(sim.mode(Spec::FAN), 1);
    assert_eq!(sim.preset(Spec::FAN), 5500);
    assert!(!g.write(&[(Spec::FAN, Setting::Firmware)]).unwrap());
    let w = sim.0.borrow();
    assert!(w.sets.iter().all(|(_, r)| r.len() == Guard::LEN && r[0] == 3));
}

#[test]
fn guard_refuses_foreign_bytes_bad_crc_curve_mode_and_failed_readback() {
    let sim = Sim::new();
    let fresh = sim.report();
    let mut target = fresh.clone();
    target[0x200] ^= 1;
    Crc16Usb::seal(&mut target);
    assert!(Guard::check_diff(&fresh, &target, &[(Spec::FAN, Setting::Duty(50.0))]).is_err());
    let mut high = fresh.clone();
    high[Spec::FAN.preset()..Spec::FAN.preset() + 2].copy_from_slice(&10001u16.to_be_bytes());
    Crc16Usb::seal(&mut high);
    assert!(Guard::check_diff(&fresh, &high, &[(Spec::FAN, Setting::Duty(100.0))]).is_err());
    let mut g = Guard::new(Box::new(sim.clone()));
    sim.set_raw(|r| r[Spec::PUMP.mode()] = 2);
    assert!(g.write(&[(Spec::PUMP, Setting::Duty(50.0))]).unwrap_err().contains("mode 2"));
    sim.set_raw(|r| r[Spec::PUMP.mode()] = 1);
    sim.0.borrow_mut().report[5] ^= 1;
    assert!(g.write(&[(Spec::FAN, Setting::Duty(50.0))]).unwrap_err().contains("crc"));
    sim.set_raw(|_| {});
    sim.0.borrow_mut().corrupt_readback = true;
    assert!(g.write(&[(Spec::FAN, Setting::Duty(50.0))]).unwrap_err().contains("readback"));
    sim.0.borrow_mut().fail_get = true;
    assert!(g.write(&[(Spec::FAN, Setting::Duty(60.0))]).is_err());
}

#[test]
fn actuator_floors_are_independent_of_the_caller() {
    let sim = Sim::new();
    let cfg = ControlConfig::default();
    let s = sim.clone();
    let mut a = D5Actuator::new(Box::new(move || Ok(Box::new(s.clone()) as Box<dyn D5Control>)), &cfg);
    a.apply(&[(ControlKind::Fan, Setting::Duty(1.0)), (ControlKind::Pump, Setting::Duty(5.0))]).unwrap();
    assert_eq!((sim.preset(Spec::FAN), sim.preset(Spec::PUMP)), (100, 1500));
    assert_eq!(sim.sets(), 1);
    a.apply(&[(ControlKind::Fan, Setting::Duty(500.0))]).unwrap();
    assert_eq!(sim.preset(Spec::FAN), 10000);
}

#[test]
fn shadow_is_the_default_and_writes_nothing() {
    let mut rig = Rig::new((false, false), 6.0, |_| {});
    rig.run(100);
    assert_eq!(rig.sim.sets(), 0);
    let st = rig.status();
    assert!(st.controls.iter().all(|c| c.mode == Mode::Pid && c.drive == Drive::Firmware && c.target_rpm.is_some()));
    let p = st.predicted.unwrap();
    assert!(p.water.is_some() && p.horizon_s == 120);
    assert!(st.controls[0].note.as_deref().unwrap().contains("nicht identifiziert"));
    let rows = rig.tick();
    assert!(rows.iter().any(|r| r.channel == "daisyloop/fan/target_rpm") && rows.iter().any(|r| r.channel == "daisyloop/plan/water"));
}

#[test]
fn active_is_refused_with_a_reason() {
    let mut rig = Rig::new((false, false), 6.0, |_| {});
    rig.run(5);
    assert!(rig.c.set_mode("d5.fan", Mode::Model, true, rig.now).unwrap_err().contains("nicht identifiziert"));
    let mut off = Rig::new((true, true), 6.0, |c| c.writes_enabled = false);
    off.run(5);
    assert!(off.c.set_mode("d5.fan", Mode::Model, true, off.now).unwrap_err().contains("writes_enabled"));
    let mut ok = Rig::new((true, true), 6.0, |_| {});
    ok.run(5);
    assert!(ok.c.set_mode("d5.pump", Mode::Model, false, ok.now).unwrap_err().contains("Bestätigung"));
    assert!(ok.c.set_mode("d5.nope", Mode::Model, true, ok.now).is_err());
    ok.gpu = None;
    ok.run(12);
    assert!(ok.c.set_mode("d5.fan", Mode::Model, true, ok.now).is_ok());
    ok.c.set_mode("d5.fan", Mode::Pid, true, ok.now).unwrap();
    ok.water = f64::NAN;
    ok.run(12);
    assert!(ok.c.set_mode("d5.fan", Mode::Model, true, ok.now).unwrap_err().contains("Wasser"));
    assert_eq!(ok.sim.sets(), 0);
    let mut cold = Rig::new((true, true), 6.0, |_| {});
    cold.c.model = Some(Box::new(Fake { ident: (true, true), warm: Rc::new(Cell::new(0)), heat: Rc::new(Cell::new(6.0)), fail: Rc::new(Cell::new(false)) }));
    cold.run(5);
    assert!(cold.c.set_mode("d5.fan", Mode::Model, true, cold.now).is_ok());
    let c = cold.ctl("d5.fan");
    assert!(c.mode == Mode::Pid && c.chosen == Mode::Model && c.note.unwrap().contains("Keine Verlaufsdaten"));
    let mut none = Rig::new((true, true), 6.0, |_| {});
    none.c.model = None;
    none.c.model_error = Some("missing".into());
    none.run(5);
    assert!(none.c.set_mode("d5.fan", Mode::Model, true, none.now).unwrap_err().contains("missing"));
}

#[test]
fn active_writes_floored_duty_once_per_interval_and_only_for_its_channel() {
    let initial = Sim::new().report();
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.live = true;
    rig.run(5);
    rig.active("d5.fan");
    rig.run(400);
    let sets = rig.sim.duty_sets();
    assert!(!sets.is_empty());
    assert!(rig.ctl("d5.fan").drive == Drive::Duty && rig.ctl("d5.pump").drive == Drive::Firmware);
    for (_, r) in &sets {
        assert!((be16(r, Spec::FAN.preset()) == 0 || be16(r, Spec::FAN.preset()) >= 2000) && r[Spec::FAN.mode()] == 0 && r[Spec::PUMP.mode()] == 1);
        assert_eq!(&r[Spec::PUMP.base..Spec::PUMP.base + 0x55], &initial[Spec::PUMP.base..Spec::PUMP.base + 0x55]);
    }
    assert!(sets.windows(2).all(|w| w[1].0 - w[0].0 >= 30_000));
    let n = rig.sim.sets();
    rig.run(600);
    assert_eq!(rig.sim.sets(), n, "a steady plan must not rewrite the report");
    let cmd = rig.ctl("d5.fan").commanded_duty.unwrap();
    assert!(cmd == 0.0 || cmd >= 20.0);
}

#[test]
fn target_moves_down_slowly_and_up_fast() {
    let mut rig = Rig::new((true, true), 20.0, |c| c.control.fan.down_step_duty = Some(0.0));
    rig.run(5);
    rig.active("d5.fan");
    rig.run(35);
    let (high, high_cmd) = (rig.ctl("d5.fan").target_rpm.unwrap(), rig.ctl("d5.fan").commanded_duty.unwrap() * 14.7);
    rig.c.model = Some(Box::new(Fake { ident: (true, true), warm: Rc::new(Cell::new(5000)), heat: Rc::new(Cell::new(0.0)), fail: rig.model_fail.clone() }));
    rig.run(31);
    let (low, low_cmd) = (rig.ctl("d5.fan").target_rpm.unwrap(), rig.ctl("d5.fan").commanded_duty.unwrap() * 14.7);
    assert!(high - low > 150.0, "published target is the plan, not the rate limited step: {high} {low}");
    assert!(high_cmd - low_cmd <= 150.0 + 1e-6 && low_cmd < high_cmd, "{high_cmd} {low_cmd}");
}

#[test]
fn the_default_down_step_is_one_coarse_step_of_35_duty_points_and_zero_gives_the_slow_ramp_the_tests_above_rely_on() {
    let mut rig = Rig::new((true, true), 20.0, |_| {});
    rig.run(5);
    rig.active("d5.fan");
    rig.run(35);
    let high = rig.ctl("d5.fan").commanded_duty.unwrap();
    rig.c.model = Some(Box::new(Fake { ident: (true, true), warm: Rc::new(Cell::new(5000)), heat: Rc::new(Cell::new(0.0)), fail: rig.model_fail.clone() }));
    rig.run(31);
    let low = rig.ctl("d5.fan").commanded_duty.unwrap();
    assert!((high - low - 35.0).abs() < 0.5, "{high} {low}");
}

#[test]
fn infeasible_plan_takes_maximum() {
    let mut rig = Rig::new((true, true), 50.0, |_| {});
    rig.run(35);
    let st = rig.status();
    assert!(!st.predicted.as_ref().unwrap().feasible);
    assert_eq!((st.controls[0].target_rpm.unwrap(), st.controls[1].target_rpm.unwrap()), (1470.0, 4800.0));
}

#[test]
fn a_higher_water_target_lets_the_planner_choose_less_cooling_and_the_range_clamps() {
    let mut cool = Rig::new((true, true), 15.5, |c| c.control.water_target = Some(41.0));
    let mut quiet = Rig::new((true, true), 15.5, |c| c.control.water_target = Some(44.0));
    cool.water = 30.0;
    quiet.water = 30.0;
    cool.run(35);
    quiet.run(35);
    let (a, b) = (cool.ctl("d5.fan").target_rpm.unwrap() + cool.ctl("d5.pump").target_rpm.unwrap(), quiet.ctl("d5.fan").target_rpm.unwrap() + quiet.ctl("d5.pump").target_rpm.unwrap());
    assert!(a > b, "{a} {b}");
    assert!(cool.c.set_target(f64::NAN).is_err() && cool.c.set_target(f64::INFINITY).is_err());
    assert!(cool.c.set_target(80.0).is_ok() && cool.c.cfg.target == 44.0 && cool.status().target == 44.0);
    assert!(cool.c.set_target(-5.0).is_ok() && cool.c.cfg.target == 30.0);
    assert!(cool.c.set_target(35.5).is_ok() && cool.status().target == 35.5 && (cool.status().target_min, cool.status().target_max) == (30.0, 44.0));
}

#[test]
fn the_water_target_survives_a_restart_and_a_corrupt_file_falls_back_to_the_admin_default() {
    let mut rig = Rig::new((false, false), 3.0, |c| (c.control.water_target, c.control.fan_start_water, c.control.fan_stop_water) = (Some(34.0), None, None));
    assert_eq!(rig.c.cfg.target, 34.0);
    rig.c.set_target(37.5).unwrap();
    assert_eq!(rig.c.cfg.target, 37.5);
    let next = rig.reborn((false, false), |c| (c.control.water_target, c.control.fan_start_water, c.control.fan_stop_water) = (Some(34.0), None, None));
    assert_eq!(next.c.cfg.target, 37.5);
    std::fs::write(next.dir.join("target.json"), r#"{"celsius":99.0}"#).unwrap();
    let clamped = rig.reborn((false, false), |c| (c.control.water_target, c.control.fan_start_water, c.control.fan_stop_water) = (Some(34.0), None, None));
    assert_eq!(clamped.c.cfg.target, 44.0);
    std::fs::write(rig.dir.join("target.json"), "{ not json").unwrap();
    let broken = rig.reborn((false, false), |c| (c.control.water_target, c.control.fan_start_water, c.control.fan_stop_water) = (Some(34.0), None, None));
    assert_eq!(broken.c.cfg.target, 34.0);
    std::fs::write(rig.dir.join("target.json"), r#"{"celsius":null}"#).unwrap();
    assert_eq!(rig.reborn((false, false), |c| (c.control.water_target, c.control.fan_start_water, c.control.fan_stop_water) = (Some(34.0), None, None)).c.cfg.target, 34.0);
    let _ = std::fs::remove_file(rig.dir.join("target.json"));
    assert_eq!(rig.reborn((false, false), |c| (c.control.water_target, c.control.fan_start_water, c.control.fan_stop_water) = (Some(34.0), None, None)).c.cfg.target, 34.0);
}

#[test]
fn the_old_keys_are_accepted_and_derive_the_default_target() {
    let cc: ControlConfig = toml::from_str("water_plan_limit = 34.5\nweight_span_k = 1.0\nweight = 0.5\nfan_start_water = 33.0\nfan_stop_water = 31.5\n").unwrap();
    assert!(cc.validate().is_ok() && (cc.default_target() - 34.0).abs() < 1e-9 && cc.legacy_keys().len() == 3);
    let rig = Rig::new((false, false), 3.0, |c| c.control = cc.clone());
    assert_eq!(rig.c.cfg.target, 34.0);
    assert_eq!((rig.c.cfg.target, rig.c.cfg.fan_start(), rig.c.cfg.fan_stop()), (34.0, 33.0, 31.5));
    let explicit: ControlConfig = toml::from_str("water_target = 38.0\nwater_plan_limit = 34.5\n").unwrap();
    assert_eq!(explicit.default_target(), 38.0);
}

#[test]
fn the_fan_hysteresis_follows_every_allowed_target() {
    let mut cc = ControlConfig::default();
    assert!(cc.validate().is_ok());
    for t in [30.0, 32.0, 34.0, 38.0, 44.0] {
        cc.target = t;
        assert_eq!((cc.fan_start(), cc.fan_stop()), (t - 1.0, t - 2.5));
        assert!(cc.fan_stop() < cc.fan_start() && cc.fan_start() < t && t + ControlConfig::TARGET_HEADROOM_K <= cc.water_limit);
    }
    let owner: ControlConfig = toml::from_str("fan_start_water = 33.0\nfan_stop_water = 31.5\n").unwrap();
    assert!(owner.validate().is_ok());
    for t in [30.0, 31.0, 33.0, 34.0, 44.0] {
        let c = ControlConfig { target: t, ..owner.clone() };
        assert!(c.fan_stop() < c.fan_start() && c.fan_start() < t, "{t}");
    }
    let hi = ControlConfig { water_limit: 38.0, ..ControlConfig::default() };
    assert!(hi.validate().is_err());
}

#[test]
fn stale_sensor_falls_back_to_the_firmware_pid_and_latches() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.run(5);
    rig.once("d5.fan");
    rig.run(60);
    assert_eq!(rig.sim.mode(Spec::FAN), 0);
    rig.water = f64::NAN;
    rig.run(9);
    assert_eq!(rig.sim.mode(Spec::FAN), 0);
    rig.run(4);
    assert_eq!(rig.sim.mode(Spec::FAN), 1);
    let c = rig.ctl("d5.fan");
    assert!(c.mode == Mode::Pid && c.drive == Drive::Firmware && c.fallback.as_deref().unwrap().contains("veraltet"));
    assert!(rig.alarm(AlarmKind::Stale) && rig.alarm(AlarmKind::Fallback));
    rig.water = 30.0;
    rig.run(120);
    assert_eq!(rig.ctl("d5.fan").mode, Mode::Pid);
    assert_eq!(rig.sim.mode(Spec::FAN), 1);
}

#[test]
fn water_at_the_limit_releases_every_active_channel() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.run(5);
    rig.active("d5.fan");
    rig.active("d5.pump");
    rig.run(40);
    assert_eq!((rig.sim.mode(Spec::FAN), rig.sim.mode(Spec::PUMP)), (0, 0));
    rig.water = 48.5;
    rig.run(2);
    assert_eq!((rig.sim.mode(Spec::FAN), rig.sim.mode(Spec::PUMP)), (1, 1));
    assert!(rig.alarm(AlarmKind::WaterHigh));
}

#[test]
fn cpu_and_gpu_spikes_and_heat_never_release_the_fan_and_never_alarm() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.run(5);
    rig.active("d5.fan");
    rig.run(40);
    let sets = rig.sim.sets();
    rig.cpu = 120.0;
    rig.gpu = Some(120.0);
    rig.run(3);
    rig.cpu = 60.0;
    rig.gpu = Some(60.0);
    rig.run(30);
    assert!(rig.status().alarms.is_empty());
    assert_eq!(rig.sim.mode(Spec::FAN), 0);
    rig.cpu = 110.0;
    rig.gpu = Some(110.0);
    rig.run(300);
    assert!(rig.status().alarms.is_empty());
    assert_eq!(rig.sim.mode(Spec::FAN), 0);
    assert!(rig.ctl("d5.fan").fallback.is_none() && !rig.alarm(AlarmKind::Fallback) && !rig.alarm(AlarmKind::WaterHigh));
    assert!(rig.sim.sets() >= sets);
    assert!(rig.status().limits.iter().all(|l| l.channel == "d5next/water/temp" || l.channel == "d5next/pump/rpm"));
}

#[test]
fn only_water_above_the_takeover_releases_and_the_top_limit_is_a_display_alarm() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.run(5);
    rig.active("d5.fan");
    rig.run(40);
    rig.water = 47.9;
    rig.run(20);
    assert_eq!(rig.sim.mode(Spec::FAN), 0);
    assert!(!rig.alarm(AlarmKind::WaterHigh) && !rig.alarm(AlarmKind::WaterTop));
    rig.water = 48.0;
    rig.run(3);
    assert_eq!(rig.sim.mode(Spec::FAN), 1);
    assert!(rig.alarm(AlarmKind::WaterHigh) && !rig.alarm(AlarmKind::WaterTop));
    rig.water = 50.2;
    rig.run(2);
    assert!(rig.alarm(AlarmKind::WaterTop));
    let l = &rig.status().limits[0];
    assert_eq!((l.plan, l.high, l.top), (Some(43.5), Some(48.0), Some(50.0)));
}

#[test]
fn rpm_that_does_not_follow_the_command_is_a_fault() {
    let mut rig = Rig::new((true, true), 3.0, |c| c.control.fan.down_step_duty = Some(0.0));
    rig.run(5);
    rig.once("d5.fan");
    rig.run(35);
    rig.sim.0.borrow_mut().freeze_rpm = true;
    rig.run(20);
    assert_eq!(rig.sim.mode(Spec::FAN), 0);
    rig.run(120);
    assert_eq!(rig.sim.mode(Spec::FAN), 1);
    assert!(rig.ctl("d5.fan").fallback.unwrap().contains("folgt"));
}

#[test]
fn model_error_in_active_mode_is_a_fault() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.run(5);
    rig.active("d5.fan");
    rig.run(40);
    rig.model_fail.set(true);
    rig.run(35);
    assert_eq!(rig.sim.mode(Spec::FAN), 1);
    assert!(rig.ctl("d5.fan").fallback.unwrap().contains("Modellfehler"));
}

#[test]
fn failed_release_is_retried_with_a_reopened_handle() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.run(5);
    rig.active("d5.fan");
    rig.run(40);
    let opens = rig.sim.0.borrow().opens;
    rig.sim.0.borrow_mut().fail_sets = 2;
    rig.water = f64::NAN;
    rig.run(12);
    assert_eq!(rig.sim.mode(Spec::FAN), 0);
    assert!(rig.ctl("d5.fan").note.unwrap().contains("Rückfall fehlgeschlagen"));
    rig.run(12);
    assert_eq!(rig.sim.mode(Spec::FAN), 1);
    assert!(rig.sim.0.borrow().opens >= opens + 2);
    assert!(!rig.ctl("d5.fan").note.unwrap_or_default().contains("Rückfall"));
}

#[test]
fn write_failure_falls_back_instead_of_retrying_the_duty() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.run(5);
    rig.active("d5.fan");
    rig.sim.0.borrow_mut().corrupt_readback = true;
    rig.run(35);
    rig.sim.0.borrow_mut().corrupt_readback = false;
    rig.run(5);
    let c = rig.ctl("d5.fan");
    assert!(c.mode == Mode::Pid && c.fallback.unwrap().contains("Schreibfehler"));
    assert_eq!(rig.sim.mode(Spec::FAN), 1);
}

#[test]
fn start_releases_only_a_channel_marked_as_owned_and_only_with_writes_enabled() {
    let sim = Sim::new();
    sim.set_raw(|r| r[Spec::FAN.mode()] = 0);
    let mut rig = Rig::with_marker(sim.clone(), &[ControlKind::Fan], (false, false), 3.0, |_| {});
    rig.run(2);
    assert_eq!((sim.mode(Spec::FAN), rig.marker()), (1, vec![]));
    let sim = Sim::new();
    sim.set_raw(|r| r[Spec::FAN.mode()] = 0);
    let mut ro = Rig::with_marker(sim.clone(), &[ControlKind::Fan], (false, false), 3.0, |c| c.writes_enabled = false);
    ro.run(2);
    assert_eq!((sim.mode(Spec::FAN), sim.sets(), ro.marker()), (0, 0, vec![ControlKind::Fan]));
}

#[test]
fn shutdown_and_mode_changes_release_to_the_firmware_pid() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.run(5);
    rig.active("d5.fan");
    rig.run(40);
    assert_eq!(rig.sim.mode(Spec::FAN), 0);
    rig.c.set_mode("d5.fan", Mode::Pid, false, rig.now).unwrap();
    assert_eq!(rig.sim.mode(Spec::FAN), 1);
    rig.active("d5.fan");
    rig.run(35);
    assert_eq!(rig.sim.mode(Spec::FAN), 0);
    rig.c.shutdown(rig.now);
    assert_eq!(rig.sim.mode(Spec::FAN), 1);
}

#[test]
fn autostart_is_off_by_default_and_waits_for_identification() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.run(100);
    assert_eq!(rig.ctl("d5.fan").mode, Mode::Pid);
    let mut auto = Rig::new((true, true), 3.0, |c| c.control.fan.autostart_active = Some(true));
    auto.run(100);
    assert_eq!(auto.ctl("d5.fan").mode, Mode::Model);
    assert_eq!(auto.ctl("d5.pump").mode, Mode::Pid);
    let mut unident = Rig::new((false, false), 3.0, |c| c.control.fan.autostart_active = Some(true));
    unident.run(400);
    assert_eq!((unident.ctl("d5.fan").mode, unident.sim.sets()), (Mode::Pid, 0));
}

#[test]
fn pacing_survives_mode_toggles() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.run(35);
    for _ in 0..30 {
        rig.c.set_mode("d5.fan", Mode::Model, true, rig.now).ok();
        rig.tick();
        rig.c.set_mode("d5.fan", Mode::Pid, false, rig.now).ok();
        rig.tick();
    }
    assert!(rig.sim.duty_sets().len() <= 2, "{}", rig.sim.duty_sets().len());
}

fn stepped(secs: f64) -> impl Fn(&mut Config) {
    move |c| c.control.step_test.fan = vec![[100.0, secs], [60.0, secs], [40.0, secs]]
}

#[test]
fn step_test_needs_confirmation_runs_the_schedule_and_releases() {
    let mut rig = Rig::new((false, false), 3.0, stepped(30.0));
    rig.run(5);
    assert!(rig.c.start_test("d5.fan", false, rig.now).unwrap_err().contains("Bestätigung"));
    rig.c.start_test("d5.fan", true, rig.now).unwrap();
    assert!(rig.c.start_test("d5.fan", true, rig.now).is_err());
    let rows = rig.tick();
    assert!(rows.iter().any(|r| r.channel == "daisyloop/step/phase" && r.value == 1.0));
    assert_eq!((rig.sim.mode(Spec::FAN), rig.sim.preset(Spec::FAN)), (0, 10000));
    assert_eq!(rig.status().step.unwrap().phases, 3);
    assert!(rig.c.set_mode("d5.fan", Mode::Model, true, rig.now).is_err());
    rig.run(30);
    assert_eq!(rig.sim.preset(Spec::FAN), 6000);
    rig.run(30);
    assert_eq!(rig.sim.preset(Spec::FAN), 4000);
    rig.run(31);
    assert_eq!(rig.sim.mode(Spec::FAN), 1);
    assert!(rig.status().step.is_none());
    assert!(rig.sim.duty_sets().windows(2).all(|w| w[1].0 - w[0].0 >= 29_000));
}

#[test]
fn step_test_is_refused_without_writes_and_aborted_by_faults_and_stop() {
    let mut ro = Rig::new((false, false), 3.0, |c| c.writes_enabled = false);
    ro.run(5);
    assert!(ro.c.start_test("d5.fan", true, ro.now).unwrap_err().contains("writes_enabled"));
    let mut rig = Rig::new((false, false), 3.0, stepped(60.0));
    rig.run(5);
    rig.c.start_test("d5.fan", true, rig.now).unwrap();
    rig.run(20);
    assert_eq!(rig.sim.mode(Spec::FAN), 0);
    rig.water = 49.0;
    rig.run(2);
    assert_eq!(rig.sim.mode(Spec::FAN), 1);
    assert!(rig.status().step.is_none());
    rig.water = 30.0;
    rig.run(35);
    rig.c.start_test("d5.fan", true, rig.now).unwrap();
    rig.run(3);
    assert_eq!(rig.sim.mode(Spec::FAN), 0);
    rig.c.stop_test(rig.now).unwrap();
    assert_eq!(rig.sim.mode(Spec::FAN), 1);
}

#[test]
fn alarms_cover_pump_fan_and_stale_sources() {
    let mut rig = Rig::new((false, false), 3.0, |_| {});
    rig.run(5);
    assert!(rig.status().alarms.is_empty());
    rig.sim.0.borrow_mut().force = [Some(0.0), Some(1000.0)];
    rig.run(4);
    assert!(rig.alarm(AlarmKind::PumpLow) && !rig.alarm(AlarmKind::FanStopped));
    rig.run(8);
    assert!(rig.alarm(AlarmKind::FanStopped));
    rig.sim.0.borrow_mut().force = [None, None];
    rig.run(1);
    assert!(rig.status().alarms.is_empty());
    rig.gpu = None;
    rig.run(12);
    assert!(rig.status().alarms.is_empty());
    rig.water = f64::NAN;
    rig.run(12);
    let a = rig.status().alarms;
    assert!(a.iter().any(|x| x.kind == AlarmKind::Stale && x.channel == "d5next/water/temp"));
}

#[path = "safety_tests.rs"]
mod safety;

#[path = "replay_tests.rs"]
mod replay;

#[path = "fan_stop_tests.rs"]
mod fan_stop;

#[path = "bound_tests.rs"]
mod bound;

#[test]
fn the_chosen_mode_survives_a_restart_and_comes_back_without_a_command() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.run(5);
    rig.active("d5.fan");
    rig.run(40);
    assert_eq!((rig.ctl("d5.fan").mode, rig.sim.mode(Spec::FAN)), (Mode::Model, 0));
    rig.c.shutdown(rig.now);
    assert_eq!(rig.sim.mode(Spec::FAN), 1);
    let mut next = rig.reborn((true, true), |_| {});
    let c = next.ctl("d5.fan");
    assert!(c.mode == Mode::Pid && c.chosen == Mode::Model);
    assert_eq!(next.ctl("d5.pump").chosen, Mode::Pid);
    next.run(40);
    assert_eq!((next.ctl("d5.fan").mode, next.sim.mode(Spec::FAN)), (Mode::Model, 0));
    assert_eq!(next.ctl("d5.pump").mode, Mode::Pid);
    next.c.set_mode("d5.fan", Mode::Pid, false, next.now).unwrap();
    let mut last = next.reborn((true, true), |_| {});
    last.run(60);
    assert!(last.ctl("d5.fan").mode == Mode::Pid && last.ctl("d5.fan").chosen == Mode::Pid);
}

#[test]
fn a_stored_model_choice_waits_while_writes_are_off_and_returns_when_they_are_on() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.run(5);
    rig.active("d5.fan");
    let mut off = rig.reborn((true, true), |c| c.writes_enabled = false);
    off.run(60);
    let c = off.ctl("d5.fan");
    assert!(c.mode == Mode::Pid && c.chosen == Mode::Model && c.note.unwrap().contains("deaktiviert"));
    assert_eq!(off.sim.sets(), 0);
    let mut on = off.reborn((true, true), |_| {});
    on.run(40);
    assert_eq!(on.ctl("d5.fan").mode, Mode::Model);
}

#[test]
fn a_tripped_channel_returns_to_the_stored_model_choice_after_its_cooldown_and_never_waits_for_a_warm_up() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.run(5);
    rig.active("d5.fan");
    rig.run(60);
    rig.water = f64::NAN;
    rig.run(13);
    let c = rig.ctl("d5.fan");
    assert!(c.mode == Mode::Pid && c.chosen == Mode::Model && c.fallback.is_some() && rig.sim.mode(Spec::FAN) == 1);
    rig.water = 30.0;
    rig.run(3);
    assert_eq!(rig.ctl("d5.fan").mode, Mode::Pid);
    rig.run(60);
    let c = rig.ctl("d5.fan");
    assert!(c.mode == Mode::Model && c.fallback.is_none());
    rig.run(40);
    assert_eq!(rig.sim.mode(Spec::FAN), 0);
}

#[test]
fn after_water_at_the_takeover_limit_the_model_choice_waits_for_the_plan_limit() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.run(5);
    rig.active("d5.fan");
    rig.run(40);
    rig.water = 48.5;
    rig.run(3);
    assert_eq!(rig.ctl("d5.fan").mode, Mode::Pid);
    rig.water = 47.0;
    rig.run(200);
    assert_eq!(rig.ctl("d5.fan").mode, Mode::Pid);
    rig.water = 35.0;
    rig.run(120);
    assert_eq!(rig.ctl("d5.fan").mode, Mode::Model);
}

#[test]
fn a_channel_fault_stays_on_the_pid_until_the_owner_selects_the_model_again() {
    let mut rig = Rig::new((true, true), 3.0, |c| c.control.fan.down_step_duty = Some(0.0));
    rig.run(5);
    rig.active("d5.fan");
    rig.run(35);
    rig.sim.0.borrow_mut().freeze_rpm = true;
    rig.run(200);
    rig.sim.0.borrow_mut().freeze_rpm = false;
    for k in 0..30 {
        rig.water = 30.0 + (k % 2) as f64;
        rig.run(100);
    }
    rig.water = 30.0;
    let c = rig.ctl("d5.fan");
    assert!(c.mode == Mode::Pid && c.chosen == Mode::Model && c.fallback.unwrap().contains("folgt") && c.note.unwrap().contains("erneut gewählt"));
    assert_eq!(rig.sim.mode(Spec::FAN), 1);
    rig.active("d5.fan");
    rig.run(40);
    assert_eq!((rig.ctl("d5.fan").mode, rig.sim.mode(Spec::FAN)), (Mode::Model, 0));
}

#[test]
fn a_write_fault_stays_on_the_pid_until_the_owner_selects_the_model_again() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.run(5);
    rig.active("d5.fan");
    rig.sim.0.borrow_mut().corrupt_readback = true;
    rig.run(35);
    rig.sim.0.borrow_mut().corrupt_readback = false;
    for k in 0..30 {
        rig.water = 30.0 + (k % 2) as f64;
        rig.run(100);
    }
    rig.water = 30.0;
    let c = rig.ctl("d5.fan");
    assert!(c.mode == Mode::Pid && c.chosen == Mode::Model && c.fallback.unwrap().contains("Schreibfehler"));
    rig.active("d5.fan");
    rig.run(40);
    assert_eq!(rig.ctl("d5.fan").mode, Mode::Model);
}

#[test]
fn the_pump_returns_only_from_a_choice_stored_with_confirmation() {
    let mut rig = Rig::new((true, true), 3.0, |_| {});
    rig.run(5);
    rig.active("d5.pump");
    let mut next = rig.reborn((true, true), |_| {});
    next.run(60);
    assert_eq!(next.ctl("d5.pump").mode, Mode::Model);
    let hand: HashMap<String, Saved> = [("pump".to_string(), Saved { mode: Mode::Model, confirmed: false })].into();
    ModeStore::new(next.dir.clone()).store(&hand).unwrap();
    let mut edited = next.reborn((true, true), |_| {});
    edited.run(60);
    let p = edited.ctl("d5.pump");
    assert!(p.mode == Mode::Pid && p.chosen == Mode::Model && p.note.unwrap().contains("Bestätigung"));
    std::fs::write(next.dir.join("modes.json"), r#"{"pump":"model"}"#).unwrap();
    let mut plain = next.reborn((true, true), |_| {});
    plain.run(60);
    assert_eq!(plain.ctl("d5.pump").mode, Mode::Pid);
}

#[test]
fn an_unparsable_mode_file_starts_on_the_pid_even_with_autostart() {
    let mut rig = Rig::new((true, true), 3.0, |c| c.control.fan.autostart_active = Some(true));
    rig.run(5);
    rig.c.shutdown(rig.now);
    std::fs::create_dir_all(&rig.dir).unwrap();
    std::fs::write(rig.dir.join("modes.json"), "{ not json").unwrap();
    let mut next = rig.reborn((true, true), |c| c.control.fan.autostart_active = Some(true));
    next.run(60);
    let c = next.ctl("d5.fan");
    assert!(c.mode == Mode::Pid && c.chosen == Mode::Pid);
    let _ = std::fs::remove_file(rig.dir.join("modes.json"));
    let missing = rig.reborn((true, true), |c| c.control.fan.autostart_active = Some(true));
    assert_eq!(missing.ctl("d5.fan").chosen, Mode::Model);
}
