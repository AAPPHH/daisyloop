use super::*;
use std::cell::RefCell;
use std::rc::Rc;

struct World {
    report: Vec<u8>,
    sets: Vec<Vec<u8>>,
    get_fail: bool,
    bad_crc: bool,
    corrupt_readback: bool,
    water: f64,
    duty: f64,
    inner_i: f64,
    heat: f64,
    cpu: f64,
    pump_rpm: f64,
    glitch: f64,
}

#[derive(Clone)]
struct Sim(Rc<RefCell<World>>);

impl Sim {
    fn new() -> Sim {
        let mut r: Vec<u8> = (0..Guard::LEN).map(|i| (i * 7 % 251) as u8).collect();
        r[0] = 3;
        for s in Spec::ALL {
            r[s.mode()] = 1;
        }
        r[Spec::FAN.setpoint()..Spec::FAN.setpoint() + 2].copy_from_slice(&3000u16.to_be_bytes());
        r[Spec::PUMP.setpoint()..Spec::PUMP.setpoint() + 2].copy_from_slice(&4500u16.to_be_bytes());
        Crc16Usb::seal(&mut r);
        Sim(Rc::new(RefCell::new(World { report: r, sets: vec![], get_fail: false, bad_crc: false, corrupt_readback: false, water: 31.0, duty: 100.0, inner_i: 0.0, heat: 300.0, cpu: 50.0, pump_rpm: 2570.0, glitch: 0.0 })))
    }
    fn sp(&self, s: Spec) -> f64 {
        be16(&self.0.borrow().report, s.setpoint()) as f64 / 100.0
    }
    fn sets(&self) -> usize {
        self.0.borrow().sets.len()
    }
    fn report(&self) -> Vec<u8> {
        self.0.borrow().report.clone()
    }
    fn foreign(&self, at: usize) {
        let mut w = self.0.borrow_mut();
        w.report[at] ^= 0x55;
        Crc16Usb::seal(&mut w.report);
    }
    fn advance(&self, dt: f64) {
        let mut w = self.0.borrow_mut();
        let sp = be16(&w.report, Spec::FAN.setpoint()) as f64 / 100.0;
        let err = w.water - sp;
        w.inner_i = (w.inner_i + 0.02 * err * dt).clamp(-50.0, 50.0);
        w.duty = (50.0 + 30.0 * err + w.inner_i).clamp(5.0, 100.0);
        let g = 10.0 + 40.0 * w.duty / 100.0;
        w.water += (w.heat - g * (w.water - 25.0)) / 4000.0 * dt;
    }
    fn rows(&self) -> Vec<Row> {
        let w = self.0.borrow();
        vec![
            Row::new("d5next", "d5next/water/temp", "water", "temperature", "degC", w.water),
            Row::new("d5next", "d5next/pump/rpm", "pump", "rpm", "rpm", w.pump_rpm),
            Row::new("lhm", "/intelcpu/0/temperature/0", "cpu", "temperature", "degC", w.cpu),
        ]
    }
}

impl D5Control for Sim {
    fn get(&mut self) -> Result<Vec<u8>, String> {
        let w = self.0.borrow();
        if w.get_fail {
            return Err("device gone".into());
        }
        let mut r = w.report.clone();
        if w.bad_crc {
            let n = r.len();
            r[n - 1] ^= 1;
        }
        Ok(r)
    }
    fn set(&mut self, report: &[u8]) -> Result<(), String> {
        let mut w = self.0.borrow_mut();
        w.sets.push(report.to_vec());
        w.report = report.to_vec();
        if w.corrupt_readback {
            w.report[0x10] ^= 1;
            Crc16Usb::seal(&mut w.report);
        }
        w.pump_rpm += w.glitch;
        Ok(())
    }
}

fn dir(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("test-tmp").join(format!("ctl-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn make(sim: &Sim, cfg: &Config) -> Control {
    let s = sim.clone();
    Control::new(cfg, Box::new(move || Ok(Box::new(s.clone()) as Box<dyn D5Control>)))
}

struct Run {
    sim: Sim,
    fc: Control,
    now: i64,
}

impl Run {
    fn new(name: &str, tweak: impl Fn(&mut Config)) -> Run {
        let mut cfg = Config { fan_control_enabled: true, data_dir: dir(name), ..Config::default() };
        tweak(&mut cfg);
        let sim = Sim::new();
        let fc = make(&sim, &cfg);
        Run { sim, fc, now: 0 }
    }
    fn tick(&mut self) {
        self.sim.advance(1.0);
        let mut rows = self.sim.rows();
        self.now += 1000;
        self.fc.step(&mut rows, self.now);
    }
    fn run(&mut self, secs: usize) {
        for _ in 0..secs {
            self.tick();
        }
    }
    fn started(name: &str, tweak: impl Fn(&mut Config)) -> Run {
        let mut r = Run::new(name, tweak);
        r.tick();
        r.fc.enable("fan", r.now).unwrap();
        r
    }
    fn state(&self, name: &str) -> u8 {
        self.fc.statuses().iter().find(|s| s.name == name).unwrap().state
    }
    fn until_first_set(&mut self) {
        let mut guard = 0;
        while self.sim.sets() == 0 && guard < 600 {
            self.tick();
            guard += 1;
        }
    }
}

fn fan34(c: &mut Config) {
    c.control.fan.target_water = Some(34.0);
}

fn patched(fresh: &[u8], spec: &Spec, sp: u16) -> Vec<u8> {
    let mut t = fresh.to_vec();
    t[spec.setpoint()..spec.setpoint() + 2].copy_from_slice(&sp.to_be_bytes());
    Crc16Usb::seal(&mut t);
    t
}

#[test]
fn disabled_by_default_opens_nothing() {
    let c = Config::default();
    assert!(!c.fan_control_enabled && !c.pump_control_enabled);
    assert!(!c.control.fan.autostart && !c.control.pump.autostart);
    let cfg = Config { data_dir: dir("disabled"), ..Config::default() };
    let mut fc = Control::new(&cfg, Box::new(|| panic!("write path opened while disabled")));
    let mut rows = vec![Row::new("d5next", "d5next/water/temp", "w", "temperature", "degC", 31.0)];
    fc.step(&mut rows, 1000);
    assert!(fc.enable("fan", 1000).unwrap_err().contains("disabled"));
    assert!(fc.enable("pump", 1000).unwrap_err().contains("disabled"));
    assert!(fc.set_pid("pump", 31.0, 0.1, 0.0, 0.0).is_err());
    assert!(fc.autostart().is_empty());
    assert!(!fc.running("fan") && !fc.running("pump"));
    assert!(rows.iter().all(|r| !r.channel.starts_with("daisyloop/")));
}

#[test]
fn pump_flag_is_separate_from_fan_flag() {
    let mut r = Run::new("flags", |_| {});
    r.tick();
    assert!(r.fc.enable("pump", r.now).unwrap_err().contains("pump_control_enabled"));
    assert!(r.fc.enable("fan", r.now).is_ok());
    assert_eq!(r.sim.sets(), 0);
}

#[test]
fn pump_bytes_changed_is_refused_on_fan_writes() {
    let sim = Sim::new();
    let fresh = sim.report();
    for at in [0x9B, 0x96, 0xEA, 0x97, 0x41, 0x10, 0x100] {
        let mut t = patched(&fresh, &Spec::FAN, 3100);
        t[at] ^= 1;
        Crc16Usb::seal(&mut t);
        assert!(Guard::check_diff(&fresh, &t, &Spec::FAN).is_err(), "byte {at:#x}");
    }
    assert!(Guard::check_diff(&fresh, &patched(&fresh, &Spec::FAN, 3100), &Spec::FAN).is_ok());
    assert!(Guard::check_diff(&fresh, &patched(&fresh, &Spec::FAN, 4000), &Spec::FAN).is_err());
    let mut unsealed = patched(&fresh, &Spec::FAN, 3100);
    unsealed[Guard::CRC] ^= 1;
    assert!(Guard::check_diff(&fresh, &unsealed, &Spec::FAN).is_err());
    assert_eq!(sim.sets(), 0);
}

#[test]
fn fan_bytes_changed_is_refused_on_pump_writes() {
    let fresh = Sim::new().report();
    for at in [0x46, 0x41, 0x42, 0x95, 0x96, 0x97, 0x10, 0x100] {
        let mut t = patched(&fresh, &Spec::PUMP, 4400);
        t[at] ^= 1;
        Crc16Usb::seal(&mut t);
        assert!(Guard::check_diff(&fresh, &t, &Spec::PUMP).is_err(), "byte {at:#x}");
    }
    assert!(Guard::check_diff(&fresh, &patched(&fresh, &Spec::PUMP, 4400), &Spec::PUMP).is_ok());
    assert!(Guard::check_diff(&fresh, &patched(&fresh, &Spec::PUMP, 2900), &Spec::PUMP).is_err());
    assert!(Guard::check_diff(&fresh, &patched(&fresh, &Spec::PUMP, 4600), &Spec::PUMP).is_err());
    assert!(Guard::check_diff(&fresh, &patched(&fresh, &Spec::FAN, 3100), &Spec::PUMP).is_err());
}

#[test]
fn secondary_report_cannot_pass() {
    let secondary = [0x02u8, 0, 0, 0, 2, 0, 0, 0, 0, 0x34, 0xC6];
    assert!(Guard::validate(&secondary).is_err());
    assert!(Guard::validate(&Sim::new().report()).is_ok());
}

#[test]
fn guard_write_limits() {
    let sim = Sim::new();
    let base = sim.report();
    let mut g = Guard::new(Box::new(sim.clone()));
    g.adopt().unwrap();
    let mut nop = |_: &[u8]| {};
    let f = Spec::FAN;
    assert!(matches!(g.write(1_000, &f, 3005, Kind::Normal, &mut nop).unwrap(), Outcome::Skipped("deadband")));
    assert!(matches!(g.write(1_000, &f, 3300, Kind::Normal, &mut nop).unwrap(), Outcome::Sent { .. }));
    assert_eq!(sim.sp(f), 30.5);
    assert!(matches!(g.write(30_000, &f, 3300, Kind::Normal, &mut nop).unwrap(), Outcome::Skipped("interval")));
    assert!(matches!(g.write(61_000, &f, 9000, Kind::Normal, &mut nop).unwrap(), Outcome::Sent { .. }));
    assert_eq!(sim.sp(f), 31.0);
    for k in 0..10 {
        let _ = g.write(122_000 + k * 61_000, &f, 9000, Kind::Normal, &mut nop);
    }
    assert_eq!(sim.sp(f), 36.0);
    assert!(matches!(g.write(900_000, &f, 9000, Kind::Normal, &mut nop).unwrap(), Outcome::Skipped(_)));
    assert!(matches!(g.write(900_001, &f, 100, Kind::Safety, &mut nop).unwrap(), Outcome::Sent { .. }));
    assert_eq!(sim.sp(f), 28.0);
    let p = Spec::PUMP;
    assert!(matches!(g.write(1_000_000, &p, 4000, Kind::Normal, &mut nop).unwrap(), Outcome::Sent { .. }));
    assert_eq!(sim.sp(p), 44.5);
    assert!(matches!(g.write(1_100_000, &p, 100, Kind::Safety, &mut nop).unwrap(), Outcome::Sent { .. }));
    assert_eq!(sim.sp(p), 30.0);
    for s in &sim.0.borrow().sets {
        assert_eq!(s[Spec::PUMP.own()][..5], base[Spec::PUMP.own()][..5]);
        assert_eq!(s[0x00..0x41], base[0x00..0x41]);
        assert_eq!(s[0xEB..Guard::CRC], base[0xEB..Guard::CRC]);
    }
}

#[test]
fn anti_windup_and_unwind() {
    let mut pid = Pid::new(&LoopConfig::default(), &Spec::FAN);
    pid.init(30.0, 33.0, 30.0);
    for _ in 0..20_000 {
        pid.step(30.0, 38.0, 0.0, 1.0);
    }
    assert!((pid.output() - pid.lo).abs() < 0.01);
    assert!(pid.terms.i.abs() <= pid.hi - pid.lo + 1e-9);
    assert!(pid.terms.desired < pid.lo + 1.0);
    let mut t = 0;
    while pid.output() <= pid.lo + 1e-9 && t < 1000 {
        pid.step(30.0, 28.5, 0.0, 1.0);
        t += 1;
    }
    assert!(t < 400, "unwound too slowly: {t}");
}

#[test]
fn rate_limit_and_cpu_guard() {
    let mut pid = Pid::new(&LoopConfig::default(), &Spec::FAN);
    pid.init(34.0, 34.0, 34.0);
    let mut last = pid.output();
    for _ in 0..600 {
        let o = pid.step(34.0, 30.0, 0.0, 1.0);
        assert!((o - last).abs() <= pid.slew + 1e-9);
        last = o;
    }
    assert!(last > 34.0);
    let mut pid = Pid::new(&LoopConfig::default(), &Spec::FAN);
    pid.init(32.0, 32.0, 32.0);
    let o = (0..300).map(|_| pid.step(32.0, 32.0, 2.0, 1.0)).last().unwrap();
    assert!(o < 32.0 - 1.9 && o >= 28.0 - 1e-9);
}

#[test]
fn converges_to_target_with_firmware_inner_loop() {
    let mut r = Run::started("converge", |c| c.control.fan.target_water = Some(32.0));
    r.sim.0.borrow_mut().water = 30.2;
    r.run(4000);
    let w = r.sim.0.borrow().water;
    assert!((w - 32.0).abs() < 0.3, "water {w}");
    assert!((28.0..=36.0).contains(&r.sim.sp(Spec::FAN)));
    assert!(r.fc.running("fan"));
    let sets = r.sim.0.borrow().sets.clone();
    assert!(sets.len() >= 3);
    for w in sets.windows(2) {
        assert!(be16(&w[1], Spec::FAN.setpoint()).abs_diff(be16(&w[0], Spec::FAN.setpoint())) <= Guard::STEP);
    }
    for s in &sets {
        assert_eq!(s[Spec::PUMP.own()], Sim::new().report()[Spec::PUMP.own()]);
    }
}

#[test]
fn writes_are_at_least_a_minute_apart() {
    let mut r = Run::started("interval", fan34);
    r.run(400);
    let n = r.sim.sets();
    assert!((2..=7).contains(&n), "{n}");
}

#[test]
fn foreign_change_pauses_without_overwrite() {
    let mut r = Run::started("foreign", fan34);
    r.run(100);
    let before = r.sim.sets();
    assert!(before >= 1);
    r.sim.foreign(0x200);
    let snapshot = r.sim.report();
    r.run(200);
    assert_eq!(r.state("fan"), 2);
    assert_eq!(r.sim.sets(), before);
    assert_eq!(r.sim.report(), snapshot);
    r.run(500);
    assert_eq!(r.sim.sets(), before);
}

#[test]
fn foreign_pump_change_is_also_foreign() {
    let mut r = Run::started("foreign-pump", fan34);
    r.run(100);
    let before = r.sim.sets();
    r.sim.foreign(0x9B);
    r.run(100);
    assert_eq!(r.state("fan"), 2);
    assert_eq!(r.sim.sets(), before);
}

#[test]
fn crc_failure_stops_control_without_write() {
    let mut r = Run::started("crc", fan34);
    r.sim.0.borrow_mut().bad_crc = true;
    r.run(200);
    assert_eq!(r.state("fan"), 3);
    assert_eq!(r.sim.sets(), 0);
}

#[test]
fn device_loss_disables_control() {
    let mut r = Run::started("loss", fan34);
    r.sim.0.borrow_mut().get_fail = true;
    r.run(200);
    assert_eq!(r.state("fan"), 3);
    assert_eq!(r.sim.sets(), 0);
}

#[test]
fn pump_deviation_after_fan_write_restores_pre_write_report_once() {
    let mut r = Run::started("pump-dev", fan34);
    let base = r.sim.report();
    r.run(5);
    r.sim.0.borrow_mut().glitch = 400.0;
    r.until_first_set();
    assert_eq!(r.sim.sets(), 1);
    r.run(10);
    assert_eq!(r.state("fan"), 3);
    assert_eq!(r.sim.sets(), 2);
    assert_eq!(r.sim.report(), base);
    r.run(300);
    assert_eq!(r.sim.sets(), 2);
}

#[test]
fn readback_mismatch_restores_and_faults() {
    let mut r = Run::started("readback", fan34);
    let base = r.sim.sp(Spec::FAN);
    r.sim.0.borrow_mut().corrupt_readback = true;
    r.until_first_set();
    r.sim.0.borrow_mut().corrupt_readback = false;
    r.run(5);
    assert_eq!(r.state("fan"), 3);
    assert_eq!(r.sim.sp(Spec::FAN), base);
}

#[test]
fn stale_telemetry_holds_then_sensor_loss_goes_to_floor() {
    let mut r = Run::started("stale", fan34);
    let rows = |r: &mut Run, with_water: bool| {
        r.sim.advance(1.0);
        let mut v: Vec<Row> = r.sim.rows().into_iter().filter(|x| with_water || x.channel != "d5next/water/temp").collect();
        r.now += 1000;
        r.fc.step(&mut v, r.now);
    };
    for _ in 0..30 {
        rows(&mut r, false);
    }
    assert_eq!(r.sim.sets(), 0);
    assert_eq!(r.state("fan"), 1);
    for _ in 0..60 {
        rows(&mut r, false);
    }
    assert_eq!(r.sim.sp(Spec::FAN), 28.0);
    assert!(r.sim.sets() >= 1);
    for _ in 0..5 {
        rows(&mut r, true);
    }
    assert_eq!(r.state("fan"), 1);
}

#[test]
fn overtemp_goes_to_floor_immediately() {
    let mut r = Run::started("over", fan34);
    r.run(3);
    r.sim.0.borrow_mut().water = 41.0;
    r.sim.0.borrow_mut().heat = 0.0;
    r.run(3);
    assert_eq!(r.sim.sp(Spec::FAN), 28.0);
    assert_eq!(r.sim.sets(), 1);
}

#[test]
fn cpu_guard_lowers_setpoint() {
    let mut hot = Run::started("cpu-hot", |c| c.control.fan.target_water = Some(31.0));
    let mut cool = Run::started("cpu-cool", |c| c.control.fan.target_water = Some(31.0));
    hot.sim.0.borrow_mut().cpu = 95.0;
    for r in [&mut hot, &mut cool] {
        r.sim.0.borrow_mut().heat = 150.0;
        r.run(900);
    }
    assert!(hot.sim.sp(Spec::FAN) < cool.sim.sp(Spec::FAN));
}

#[test]
fn clean_disable_restores_base_and_removes_file() {
    let mut r = Run::started("disable", fan34);
    r.run(200);
    assert!(r.sim.sp(Spec::FAN) > 30.0);
    assert!(Recovery::load(&r.fc.dir).is_some());
    let now = r.now;
    r.fc.disable("fan", now);
    assert_eq!(r.sim.sp(Spec::FAN), 30.0);
    assert!(Recovery::load(&r.fc.dir).is_none());
    let sets = r.sim.sets();
    r.run(300);
    assert_eq!(r.sim.sets(), sets);
}

#[test]
fn crash_recovery_restores_base() {
    let mut r = Run::started("crash", fan34);
    r.run(200);
    assert!(r.sim.sp(Spec::FAN) > 30.0);
    let cfg = Config { fan_control_enabled: true, data_dir: r.fc.dir.clone(), ..Config::default() };
    let sim = r.sim.clone();
    drop(r);
    let fc = make(&sim, &cfg);
    assert_eq!(sim.sp(Spec::FAN), 30.0);
    assert!(Recovery::load(&fc.dir).is_none());
}

#[test]
fn crash_recovery_leaves_foreign_state_alone() {
    let mut r = Run::started("crash-foreign", fan34);
    r.run(200);
    r.sim.foreign(0x200);
    let cfg = Config { fan_control_enabled: true, data_dir: r.fc.dir.clone(), ..Config::default() };
    let sim = r.sim.clone();
    let sets = sim.sets();
    drop(r);
    let fc = make(&sim, &cfg);
    assert_eq!(sim.sets(), sets);
    assert!(Recovery::load(&fc.dir).is_none());
}

#[test]
fn crash_recovery_needs_the_flag() {
    let mut r = Run::started("crash-flag", fan34);
    r.run(200);
    let cfg = Config { data_dir: r.fc.dir.clone(), ..Config::default() };
    let sim = r.sim.clone();
    let (sets, sp) = (sim.sets(), sim.sp(Spec::FAN));
    drop(r);
    let fc = make(&sim, &cfg);
    assert_eq!((sim.sets(), sim.sp(Spec::FAN)), (sets, sp));
    assert!(Recovery::load(&fc.dir).is_some());
}

#[test]
fn out_of_range_base_refuses_enable() {
    let mut r = Run::new("range", |_| {});
    {
        let mut w = r.sim.0.borrow_mut();
        w.report[Spec::FAN.setpoint()..Spec::FAN.setpoint() + 2].copy_from_slice(&4500u16.to_be_bytes());
        Crc16Usb::seal(&mut w.report);
    }
    r.tick();
    assert!(r.fc.enable("fan", r.now).is_err());
    assert_eq!(r.sim.sets(), 0);
}

fn pump_only(c: &mut Config) {
    c.fan_control_enabled = false;
    c.pump_control_enabled = true;
    c.control.pump.target_water = Some(30.0);
}

#[test]
fn pump_write_changes_only_pump_bytes() {
    let mut r = Run::new("pump-write", pump_only);
    r.tick();
    r.fc.enable("pump", r.now).unwrap();
    let base = r.sim.report();
    r.run(300);
    assert!(r.sim.sets() >= 1);
    assert!(r.sim.sp(Spec::PUMP) < 45.0 && r.sim.sp(Spec::PUMP) >= 30.0);
    for s in &r.sim.0.borrow().sets {
        assert_eq!(s[Spec::FAN.own()], base[Spec::FAN.own()]);
        assert_eq!(s[Spec::PUMP.mode()], 1);
        assert!(Guard::check_diff(&base, s, &Spec::PUMP).is_ok());
    }
    assert_eq!(r.fc.statuses()[1].state, 1);
}

#[test]
fn pump_low_rpm_for_three_seconds_is_a_hard_stop() {
    let mut r = Run::new("pump-low", pump_only);
    r.tick();
    r.fc.enable("pump", r.now).unwrap();
    r.run(200);
    let sets = r.sim.sets();
    assert!(sets >= 1);
    r.sim.0.borrow_mut().pump_rpm = 1700.0;
    r.run(2);
    assert_eq!(r.state("pump"), 1);
    r.run(3);
    assert_eq!(r.state("pump"), 3);
    assert_eq!(r.sim.sp(Spec::PUMP), 30.0);
    assert_eq!(r.sim.sets(), sets + 1);
    assert_eq!(r.sim.report()[Spec::FAN.own()], Sim::new().report()[Spec::FAN.own()]);
}

#[test]
fn pump_telemetry_stale_is_a_hard_stop() {
    let mut r = Run::new("pump-stale", pump_only);
    r.tick();
    r.fc.enable("pump", r.now).unwrap();
    for _ in 0..8 {
        r.sim.advance(1.0);
        let mut v: Vec<Row> = r.sim.rows().into_iter().filter(|x| x.channel != "d5next/pump/rpm").collect();
        r.now += 1000;
        r.fc.step(&mut v, r.now);
    }
    assert_eq!(r.state("pump"), 3);
    assert_eq!(r.sim.sp(Spec::PUMP), 30.0);
}

#[test]
fn fan_and_pump_writes_are_serialised() {
    let mut r = Run::new("serial", |c| {
        c.pump_control_enabled = true;
        c.control.fan.target_water = Some(34.0);
        c.control.pump.target_water = Some(30.0);
    });
    r.tick();
    r.fc.enable("fan", r.now).unwrap();
    r.fc.enable("pump", r.now).unwrap();
    let mut stamps = Vec::new();
    let mut seen = 0;
    for _ in 0..600 {
        r.tick();
        if r.sim.sets() > seen {
            seen = r.sim.sets();
            stamps.push(r.now);
        }
    }
    assert!(stamps.len() >= 4, "{stamps:?}");
    for w in stamps.windows(2) {
        assert!(w[1] - w[0] >= Guard::INTERVAL_MS, "{stamps:?}");
    }
    let sets = r.sim.0.borrow().sets.clone();
    let base = Sim::new().report();
    assert!(sets.iter().any(|s| be16(s, Spec::PUMP.setpoint()) != 4500));
    assert!(sets.iter().any(|s| be16(s, Spec::FAN.setpoint()) != 3000));
    for s in &sets {
        assert_eq!(s[0x00..0x41], base[0x00..0x41]);
        assert_eq!(s[0xEB..Guard::CRC], base[0xEB..Guard::CRC]);
    }
}

#[test]
fn both_restored_on_shutdown() {
    let mut r = Run::new("both", |c| {
        c.pump_control_enabled = true;
        c.control.fan.target_water = Some(34.0);
        c.control.pump.target_water = Some(30.0);
    });
    r.tick();
    r.fc.enable("fan", r.now).unwrap();
    r.fc.enable("pump", r.now).unwrap();
    r.run(400);
    let now = r.now;
    r.fc.shutdown(now);
    assert_eq!(r.sim.sp(Spec::FAN), 30.0);
    assert_eq!(r.sim.sp(Spec::PUMP), 45.0);
    assert!(Recovery::load(&r.fc.dir).is_none());
}
