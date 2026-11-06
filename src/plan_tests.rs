use super::*;
use crate::model::{Forecast, Golden, XgbModel};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Kind {
    Blind,
    Informed,
    Xgb,
}

impl Kind {
    fn parse(s: &str) -> Kind {
        match s {
            "informed" => Kind::Informed,
            "xgb" => Kind::Xgb,
            _ => Kind::Blind,
        }
    }
}

struct Shipped(XgbModel);

impl Model for Shipped {
    fn observe(&mut self, row: &HashMap<String, f64>, epoch: i64) {
        self.0.observe(row, epoch);
    }
    fn identified(&self) -> (bool, bool) {
        (true, true)
    }
    fn warm_secs(&self) -> i64 {
        self.0.warm_secs()
    }
    fn forecast(&self, hold: &HashMap<String, f64>, rollout: u32, horizon: u32) -> Result<Forecast, String> {
        self.0.forecast(hold, rollout, horizon)
    }
}

struct Informed {
    plant: Rc<RefCell<Plant>>,
    feed: Rc<Cell<(f64, f64)>>,
}

impl Model for Informed {
    fn observe(&mut self, _: &HashMap<String, f64>, _: i64) {}
    fn identified(&self) -> (bool, bool) {
        (true, true)
    }
    fn warm_secs(&self) -> i64 {
        5000
    }
    fn forecast(&self, hold: &HashMap<String, f64>, rollout: u32, horizon: u32) -> Result<Forecast, String> {
        let (power, rpm0) = self.feed.get();
        let rpm = hold.get("d5next/fan/rpm").copied().unwrap_or(rpm0);
        let mut p = self.plant.borrow().clone();
        let now = p.reading();
        for _ in 0..rollout + horizon {
            p.step(power, rpm);
        }
        let d = p.reading() - now;
        Ok(Forecast { water: d, cpu: d, gpu: 0.0 })
    }
}

pub(super) struct XgbWorld {
    base: HashMap<String, f64>,
    epoch: i64,
    pub(super) start: f64,
}

pub(super) fn rig(kind: Kind, plant: &Rc<RefCell<Plant>>, feed: &Rc<Cell<(f64, f64)>>, tweak: impl Fn(&mut Config)) -> (Rig, Option<XgbWorld>) {
    match kind {
        Kind::Blind => (blind_rig(tweak), None),
        Kind::Informed => (rig_with(Box::new(Informed { plant: plant.clone(), feed: feed.clone() }), tweak), None),
        Kind::Xgb => {
            let g = Golden::load();
            let mut r = rig_with(Box::new(Shipped(g.model())), tweak);
            let mut epoch = g.k0;
            for t in 3000..g.rows.len() {
                r.now += 1000;
                epoch += 1;
                r.sim.0.borrow_mut().now = r.now;
                let mut rows: Vec<Row> = g.row(t).into_iter().map(|(k, v)| Row::new("replay", k.clone(), k, "other", "", v)).collect();
                for (ch, v) in [("d5next/pump/duty", 19.8), ("d5next/pump/rpm", 2570.0)] {
                    if !rows.iter().any(|x| x.channel == ch) {
                        rows.push(Row::new("replay", ch, ch, "other", "", v));
                    }
                }
                r.c.step(&mut rows, r.now, epoch);
            }
            let base = g.row(g.rows.len() - 1);
            let start = base["d5next/water/temp"];
            (r, Some(XgbWorld { base, epoch, start }))
        }
    }
}

pub(super) fn advance(r: &mut Rig, world: &mut Option<XgbWorld>, fan_rpm: f64) {
    let Some(w) = world.as_mut() else {
        r.tick();
        return;
    };
    r.now += 1000;
    w.epoch += 1;
    r.sim.0.borrow_mut().now = r.now;
    let mut rows = r.rows();
    for (k, v) in [("d5next/fan/power", 9.3 * (fan_rpm / 1470.0).powi(3)), ("d5next/fan/current", 0.8 * (fan_rpm / 1470.0).powi(2))] {
        rows.push(Row::new("replay", k, k, "other", "", v));
    }
    for (k, v) in &w.base {
        if !rows.iter().any(|x| x.channel == *k) {
            rows.push(Row::new("replay", k.clone(), k.clone(), "other", "", *v));
        }
    }
    r.c.step(&mut rows, r.now, w.epoch);
}

fn load(spec: &str) -> Profile {
    let n: Vec<f64> = spec.split(':').skip(1).map(|x| x.parse().unwrap()).collect();
    match spec.split(':').next().unwrap() {
        "sq" => Profile::Square(n[0], n[1], n[2] as usize, n[3] as usize),
        "step" => Profile::Step(n[0], n[1], n[2] as usize),
        _ => Profile::Constant(n[0]),
    }
}

fn case_line(case: &str) -> String {
    let f: Vec<&str> = case.split(';').collect();
    let (kind, spec, profile) = (Kind::parse(f[0]), if f[1] == "alternative" { PlantSpec::ALTERNATIVE } else { PlantSpec::MEASURED }, load(f[2]));
    let (plan_secs, write_secs): (f64, f64) = (f[3].parse().unwrap(), f[4].parse().unwrap());
    let o = simulate_with(kind, profile, spec, SENSOR_NOISE, move |c| {
        c.control.plan_secs = plan_secs;
        c.control.decision_secs = write_secs;
    });
    format!("R36 {case} | writes {} | swings {} | stops {} | short {} | peak {:.2} | pre {:.0}/{:.0} | rise {} | fan_cold {:.3} | budget {} | fallback {} | frozen {}", o.writes, o.swings, o.stops, o.short_cycles, o.peak, o.pre.0, o.pre.1, o.rise.map_or("-".into(), |r| r.to_string()), o.fan_cold, o.budget, o.fallback, o.frozen)
}

#[test]
#[ignore]
fn r36_one_case_from_the_environment_kind_plant_load_plan_secs_write_secs() {
    let case = std::env::var("DL_CASE").expect("DL_CASE");
    println!("{}", case_line(&case));
}

#[test]
fn r37_planning_runs_every_plan_second_while_writes_stay_a_decision_interval_apart() {
    let mut rig = Rig::new((true, true), 20.0, |c| {
        c.control.plan_secs = 5.0;
        c.control.decision_secs = 30.0;
    });
    rig.run(5);
    rig.active("d5.fan");
    for _ in 0..400 {
        rig.tick();
        let plan_age = rig.c.plan.map(|p| rig.now - p.1);
        assert!(plan_age.is_some_and(|a| a <= 5000), "{plan_age:?}");
        rig.heat.set(if rig.now / 60_000 % 2 == 0 { 20.0 } else { -6.0 });
    }
    let times: Vec<i64> = rig.sim.0.borrow().sets.iter().map(|s| s.0).collect();
    assert!(times.len() >= 4 && times.windows(2).all(|w| w[1] - w[0] >= 30_000), "{times:?}");
}

#[test]
fn r38_plan_secs_must_be_at_least_one_second_and_not_above_the_write_interval() {
    for bad in ["plan_secs = 0.5", "plan_secs = 40.0", "plan_secs = 6.0\ndecision_secs = 5.0"] {
        let cfg: ControlConfig = toml::from_str(bad).unwrap();
        assert!(cfg.validate().is_err(), "{bad}");
    }
    let cfg: ControlConfig = toml::from_str("plan_secs = 1.0\ndecision_secs = 5.0").unwrap();
    assert!(cfg.validate().is_ok());
}

#[test]
fn r39_rate_limits_are_per_30_s_of_elapsed_time_not_per_write() {
    let mut rig = Rig::new((true, true), -6.0, |c| {
        c.control.plan_secs = 1.0;
        c.control.decision_secs = 5.0;
    });
    rig.run(5);
    rig.active("d5.fan");
    rig.run(300);
    let before = rig.sim.0.borrow().sets.len();
    rig.heat.set(20.0);
    rig.run(120);
    let fan_cal = ChannelConfig::defaults(ControlKind::Fan).calibration();
    let rpms: Vec<f64> = rig.sim.0.borrow().sets[before.saturating_sub(1)..].iter().map(|s| fan_cal.rpm(be16(&s.1, Spec::FAN.preset()) as f64 / 100.0)).collect();
    let max_step = rpms.windows(2).skip(1).map(|w| w[1] - w[0]).fold(0.0, f64::max);
    assert!(rpms.len() >= 5 && max_step <= 700.0 * 5.0 / 30.0 + 1.0, "{rpms:?}");
}

#[test]
fn r40_a_load_step_raises_the_duty_within_the_dead_time_plus_5_s_with_an_informed_model() {
    for (name, spec) in PlantSpec::NAMED {
        let o = simulate_with(Kind::Informed, Profile::Step(60.0, 260.0, 3600), spec, SENSOR_NOISE, |c| c.control.plan_secs = 5.0);
        assert!(o.rise.is_some_and(|r| r <= Plant::DELAY + 5) && !o.fallback && !o.frozen, "{name} {o:?}");
    }
}
