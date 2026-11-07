use crate::control::{Calibration, ChannelConfig, ControlConfig};
use model_infer::Predictor;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug)]
pub struct Forecast {
    pub water: f64,
}

pub trait Model {
    fn observe(&mut self, row: &HashMap<String, f64>, epoch: i64);
    fn identified(&self) -> (bool, bool);
    fn warm_secs(&self) -> i64;
    fn seed(&mut self, _history: &[(i64, HashMap<String, f64>)], _now: i64, _max_gap: i64) -> usize {
        0
    }
    fn forecast(&self, hold: &HashMap<String, f64>, rollout_s: u32, horizon_s: u32) -> Result<Forecast, String>;
}

pub struct XgbModel {
    p: Predictor,
    fresh: Predictor,
    channels: Vec<String>,
    last: Vec<f64>,
    fill: Vec<f64>,
    fill_at: Vec<i64>,
    x: Vec<f64>,
    epoch: i64,
    since: i64,
    bridge: i64,
}

impl XgbModel {
    fn advance(&mut self, raw: Vec<f64>, epoch: i64) {
        if self.epoch != 0 && epoch - self.epoch > Self::FILL_SECS {
            self.p = self.fresh.clone();
            self.since = 0;
            self.fill = vec![f64::NAN; raw.len()];
        }
        self.epoch = epoch;
        self.since += 1;
        for (i, v) in raw.iter().enumerate().filter(|(_, v)| v.is_finite()) {
            self.fill[i] = *v;
            self.fill_at[i] = epoch;
        }
        self.x = self.p.features(&raw, epoch);
        self.last = raw;
    }

    pub const KEY_CHANNEL: &'static str = "d5next/water/temp";
    pub const FILL_SECS: i64 = 10;

    pub fn from_text(spec: &str, model: &str) -> Result<XgbModel, String> {
        let p = Predictor::load(spec, model)?;
        let channels: Vec<String> = p.engine.raw_channels.to_vec();
        if !channels.iter().any(|c| c == Self::KEY_CHANNEL) {
            return Err("feature spec has no water channel".into());
        }
        let n = channels.len();
        Ok(XgbModel { fresh: p.clone(), p, last: vec![f64::NAN; n], fill: vec![f64::NAN; n], fill_at: vec![0; n], x: vec![], channels, epoch: 0, since: 0, bridge: 0 })
    }

    pub fn load(dirs: &[PathBuf], spec: &str, file: &str) -> Result<XgbModel, String> {
        let read = |name: &str| -> Result<String, String> {
            dirs.iter().find_map(|d| std::fs::read_to_string(d.join(name)).ok()).ok_or_else(|| format!("{name} not found in {}", dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(", ")))
        };
        XgbModel::from_text(&read(spec)?, &read(file)?)
    }

    pub fn wants(&self, channel: &str) -> bool {
        self.channels.iter().any(|c| c == channel)
    }

    pub fn search_dirs(data_dir: &Path, install_dir: Option<&Path>) -> Vec<PathBuf> {
        vec![install_dir.unwrap_or(data_dir).join("models")]
    }

    #[cfg(test)]
    pub fn parts(&self, reps: usize) -> (f64, f64, f64) {
        let raw = self.filled();
        let t0 = std::time::Instant::now();
        let mut x = self.x.clone();
        for _ in 0..reps {
            let mut p = self.p.clone();
            for k in 1..=120i64 {
                x = p.features(&raw, self.epoch + k);
            }
        }
        let roll = t0.elapsed().as_secs_f64() * 1e3 / reps as f64;
        let t1 = std::time::Instant::now();
        for _ in 0..reps {
            std::hint::black_box(self.p.model.predict(&x));
        }
        let pred = t1.elapsed().as_secs_f64() * 1e3 / reps as f64;
        let mut c = self.clone_for_observe();
        let t2 = std::time::Instant::now();
        for i in 0..reps {
            c.p.features(&raw, self.epoch + 1000 + i as i64);
        }
        (roll, pred, t2.elapsed().as_secs_f64() * 1e3 / reps as f64)
    }

    #[cfg(test)]
    fn clone_for_observe(&self) -> XgbModel {
        XgbModel { p: self.p.clone(), fresh: self.fresh.clone(), channels: self.channels.clone(), last: self.last.clone(), fill: self.fill.clone(), fill_at: self.fill_at.clone(), x: self.x.clone(), epoch: self.epoch, since: self.since, bridge: self.bridge }
    }

    fn filled(&self) -> Vec<f64> {
        (0..self.channels.len()).map(|i| if self.epoch - self.fill_at[i] <= Self::FILL_SECS { self.fill[i] } else { f64::NAN }).collect()
    }
}

impl Model for XgbModel {
    fn observe(&mut self, row: &HashMap<String, f64>, epoch: i64) {
        let raw: Vec<f64> = self.channels.iter().map(|c| row.get(c).copied().filter(|v| v.is_finite()).unwrap_or(f64::NAN)).collect();
        let bridge = std::mem::take(&mut self.bridge);
        let gap = epoch - self.epoch;
        if self.epoch != 0 && gap > 1 && gap <= bridge {
            let from = self.last.clone();
            for k in 1..gap {
                let f = k as f64 / gap as f64;
                let mid = from.iter().zip(&raw).map(|(a, b)| if a.is_finite() && b.is_finite() { a + (b - a) * f } else { *a }).collect();
                self.advance(mid, self.epoch + 1);
            }
        }
        self.advance(raw, epoch);
    }

    fn identified(&self) -> (bool, bool) {
        (self.p.model.identified.fan, self.p.model.identified.pump)
    }

    fn warm_secs(&self) -> i64 {
        if !self.x.is_empty() && self.x.iter().all(|v| v.is_finite()) { self.since } else { 0 }
    }

    fn seed(&mut self, history: &[(i64, HashMap<String, f64>)], now: i64, max_gap: i64) -> usize {
        let Some((last, _)) = history.last() else { return 0 };
        if self.epoch != 0 || now < *last || now - *last > max_gap {
            return 0;
        }
        for (epoch, row) in history {
            self.observe(row, *epoch);
        }
        self.bridge = max_gap;
        self.since as usize
    }

    fn forecast(&self, hold: &HashMap<String, f64>, rollout_s: u32, horizon_s: u32) -> Result<Forecast, String> {
        if self.epoch == 0 {
            return Err("model has seen no data".into());
        }
        let mut raw = self.filled();
        for (i, c) in self.channels.iter().enumerate() {
            if let Some(v) = hold.get(c) {
                raw[i] = *v;
            }
        }
        let mut p = self.p.clone();
        let mut x = self.x.clone();
        for k in 1..=rollout_s as i64 {
            x = p.features(&raw, self.epoch + k);
        }
        let out = p.model.predict(&x);
        let get = |prefix: &str| {
            let name = format!("{prefix}_{horizon_s}");
            out.iter().find(|(n, _)| *n == name).map(|(_, v)| *v).filter(|v| v.is_finite()).ok_or_else(|| format!("model has no finite target {name}"))
        };
        Ok(Forecast { water: get("water")? })
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Operating {
    pub duty: f64,
    pub rpm: f64,
    pub power: Option<f64>,
    pub current: Option<f64>,
}

pub struct Axis<'a> {
    pub name: &'static str,
    pub cfg: &'a ChannelConfig,
    pub cal: &'a Calibration,
    pub free: bool,
    pub base: Operating,
    pub zero_ok: bool,
}

impl Axis<'_> {
    const SCALE_MIN_RPM: f64 = 100.0;
    const SCALE_MAX: f64 = 20.0;

    pub fn rpm_range(&self) -> (f64, f64) {
        (self.cal.rpm(self.cfg.floor_duty), self.cal.rpm(self.cfg.max_duty))
    }

    pub fn fit(&self, rpm: f64) -> (f64, f64) {
        if !self.free {
            return (self.base.duty, self.base.rpm);
        }
        let (lo, hi) = self.rpm_range();
        let duty = self.cfg.commandable(self.cal.duty(rpm.clamp(lo, hi)), self.zero_ok);
        (duty, self.cal.rpm(duty))
    }

    pub fn candidates(&self, n: usize) -> Vec<f64> {
        if !self.free {
            return vec![self.base.rpm];
        }
        let (lo, hi) = self.rpm_range();
        let mut out: Vec<f64> = Vec::new();
        for r in (0..n).map(|i| if i + 1 == n { hi } else { lo + (hi - lo) * i as f64 / (n - 1) as f64 }) {
            let r = self.fit(r).1;
            if out.last() != Some(&r) {
                out.push(r);
            }
        }
        out
    }

    fn scale(&self, rpm: f64) -> f64 {
        if self.base.rpm < Self::SCALE_MIN_RPM { 1.0 } else { (rpm / self.base.rpm).clamp(0.0, Self::SCALE_MAX) }
    }

    fn hold(&self, rpm: f64, out: &mut HashMap<String, f64>) {
        let (duty, rpm) = self.fit(rpm);
        let r = self.scale(rpm);
        let key = |s: &str| format!("d5next/{}/{s}", self.name);
        out.insert(key("duty"), duty);
        out.insert(key("rpm"), rpm);
        if let Some(p) = self.base.power {
            out.insert(key("power"), p * r.powi(3));
        }
        if let Some(c) = self.base.current {
            out.insert(key("current"), c * r.powi(2));
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Temps {
    pub water: Option<f64>,
}

#[derive(Clone, Copy, Debug)]
pub struct Plan {
    pub fan_rpm: f64,
    pub pump_rpm: f64,
    pub water: Option<f64>,
    pub limit: f64,
    pub feasible: bool,
}

pub struct Planner<'a> {
    pub cfg: &'a ControlConfig,
}

impl Planner<'_> {
    pub fn limit(cfg: &ControlConfig, weight: f64) -> f64 {
        cfg.water_plan_limit - weight.clamp(0.0, 1.0) * cfg.weight_span_k
    }

    fn noise(&self, fan: &Axis, pump: &Axis, f: f64, p: f64) -> f64 {
        let n = |a: &Axis, r: f64| (r / a.rpm_range().1.max(1.0)).powi(3);
        n(fan, f) + self.cfg.pump_noise * n(pump, p)
    }

    fn within(plan: &Plan, margin: f64) -> bool {
        plan.water.is_some_and(|t| t <= plan.limit - margin)
    }

    pub fn plan(&self, model: &dyn Model, fan: &Axis, pump: &Axis, now: &Temps, weight: f64, prev: Option<(f64, f64)>) -> Result<Plan, String> {
        let limit = Self::limit(self.cfg, weight);
        let eval = |f: f64, p: f64| -> Result<Plan, String> {
            let mut hold = HashMap::new();
            fan.hold(f, &mut hold);
            pump.hold(p, &mut hold);
            let fc = model.forecast(&hold, self.cfg.rollout_secs, self.cfg.horizon_secs)?;
            Ok(Plan { fan_rpm: f, pump_rpm: p, water: now.water.map(|t| t + fc.water), limit, feasible: true })
        };
        let (fans, pumps) = (fan.candidates(self.cfg.fan_candidates), pump.candidates(self.cfg.pump_candidates));
        let mut all: Vec<(f64, Plan)> = Vec::new();
        let mut hottest: Option<Plan> = None;
        for (fi, &f) in fans.iter().enumerate() {
            for (pi, &p) in pumps.iter().enumerate() {
                let plan = eval(f, p)?;
                all.push((self.noise(fan, pump, f, p), plan));
                if fi + 1 == fans.len() && pi + 1 == pumps.len() {
                    hottest = Some(Plan { feasible: false, ..plan });
                }
            }
        }
        let cheapest = |margin: f64| all.iter().filter(|(_, p)| Self::within(p, margin)).min_by(|a, b| a.0.total_cmp(&b.0));
        let stay = match prev {
            Some((f, p)) if now.water.is_some() => {
                let (f, p) = (fan.fit(f).1, pump.fit(p).1);
                let plan = eval(f, p)?;
                Self::within(&plan, 0.0).then(|| (self.noise(fan, pump, f, p), plan))
            }
            _ => None,
        };
        match stay {
            Some((cost, plan)) => Ok(cheapest(self.cfg.plan_hysteresis_k).filter(|b| b.0 < cost).map_or(plan, |b| b.1)),
            None => cheapest(0.0).map(|b| b.1).or(hottest).ok_or_else(|| "no candidates".to_string()),
        }
    }
}

#[cfg(test)]
pub struct Golden {
    pub channels: Vec<String>,
    pub rows: Vec<Vec<Option<f64>>>,
    pub k0: i64,
}

#[cfg(test)]
impl Golden {
    pub fn dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models")
    }
    pub fn load() -> Golden {
        Self::named("golden.json")
    }
    pub fn named(file: &str) -> Golden {
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(Self::dir().join(file)).unwrap()).unwrap();
        let channels = v["channels"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_string()).collect();
        let rows = v["rows"].as_array().unwrap().iter().map(|r| r.as_array().unwrap().iter().map(|x| x.as_f64()).collect()).collect();
        Golden { channels, rows, k0: v["k0"].as_i64().unwrap() }
    }
    pub fn model(&self) -> XgbModel {
        XgbModel::load(&[Self::dir()], "feature_spec_deploy.json", "model_deploy.json").unwrap()
    }
    pub fn linear(&self) -> XgbModel {
        XgbModel::load(&[Self::dir()], "feature_spec_linear.json", "model_linear.json").unwrap()
    }
    pub fn row(&self, t: usize) -> HashMap<String, f64> {
        self.channels.iter().zip(&self.rows[t]).filter_map(|(c, v)| v.map(|v| (c.clone(), v))).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ControlKind;

    fn real() -> XgbModel {
        let g = Golden::load();
        let mut m = g.model();
        for t in 3000..g.rows.len() {
            m.observe(&g.row(t), g.k0 + t as i64);
        }
        m
    }

    #[test]
    fn shipped_model_loads_is_identified_by_owner_override_and_forecasts_finite_values() {
        let m = real();
        assert_eq!(m.identified(), (true, true));
        assert!(m.warm_secs() > 1800, "{}", m.warm_secs());
        let hold: HashMap<String, f64> = [("d5next/fan/rpm".to_string(), 800.0), ("d5next/fan/duty".to_string(), 50.0)].into();
        let f = m.forecast(&hold, 120, 120).unwrap();
        assert!(f.water.is_finite());
        assert!(m.forecast(&hold, 120, 77).is_err());
        assert!(XgbModel::load(&[PathBuf::from("nowhere")], "a", "b").is_err());
    }

    #[test]
    fn a_water_only_model_loads_and_forecasts_like_the_deployed_one() {
        let dir = Golden::dir();
        let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("model_deploy.json")).unwrap()).unwrap();
        v["targets"].as_object_mut().unwrap().retain(|k, _| k.starts_with("water_"));
        let spec = std::fs::read_to_string(dir.join("feature_spec_deploy.json")).unwrap();
        let g = Golden::load();
        let (mut only, mut full) = (XgbModel::from_text(&spec, &v.to_string()).unwrap(), g.model());
        for t in 3000..3400 {
            only.observe(&g.row(t), g.k0 + t as i64);
            full.observe(&g.row(t), g.k0 + t as i64);
        }
        let hold: HashMap<String, f64> = HashMap::new();
        assert!((only.forecast(&hold, 120, 120).unwrap().water - full.forecast(&hold, 120, 120).unwrap().water).abs() < 1e-9);
    }

    #[test]
    fn planner_runs_the_shipped_model_over_the_grid_fast_enough_for_the_tick() {
        let m = real();
        let cfg = ControlConfig::default();
        let (fc, pc) = (ChannelConfig::defaults(ControlKind::Fan), ChannelConfig::defaults(ControlKind::Pump));
        let (fcal, pcal) = (fc.calibration(), pc.calibration());
        let op = Operating { duty: 80.0, rpm: 1200.0, power: None, current: None };
        let fan = Axis { name: "fan", cfg: &fc, cal: &fcal, free: true, base: op, zero_ok: true };
        let pump = Axis { name: "pump", cfg: &pc, cal: &pcal, free: true, base: Operating { duty: 50.0, rpm: 3400.0, ..op }, zero_ok: false };
        let t0 = std::time::Instant::now();
        let plan = Planner { cfg: &cfg }.plan(&m, &fan, &pump, &Temps { water: Some(31.0) }, 0.5, None).unwrap();
        assert!(t0.elapsed().as_secs_f64() < 5.0, "{:?}", t0.elapsed());
        assert!(plan.fan_rpm >= fan.rpm_range().0 && plan.fan_rpm <= fan.rpm_range().1 && plan.pump_rpm >= pump.rpm_range().0);
    }
    #[test]
    #[ignore]
    fn plan_cost_per_run_in_release() {
        let m = real();
        let cfg = ControlConfig::default();
        let (fc, pc) = (ChannelConfig::defaults(ControlKind::Fan), ChannelConfig::defaults(ControlKind::Pump));
        let (fcal, pcal) = (fc.calibration(), pc.calibration());
        let op = Operating { duty: 80.0, rpm: 1200.0, power: None, current: None };
        let fan = Axis { name: "fan", cfg: &fc, cal: &fcal, free: true, base: op, zero_ok: true };
        let pump = Axis { name: "pump", cfg: &pc, cal: &pcal, free: true, base: Operating { duty: 50.0, rpm: 3400.0, ..op }, zero_ok: false };
        let temps = Temps { water: Some(31.0) };
        let runs = 10;
        let t0 = std::time::Instant::now();
        for _ in 0..runs {
            Planner { cfg: &cfg }.plan(&m, &fan, &pump, &temps, 0.5, Some((1200.0, 3400.0))).unwrap();
            m.forecast(&HashMap::new(), 0, cfg.horizon_secs).unwrap();
        }
        eprintln!("PLANCOST ms per run (plan plus direct forecast): {:.2}", t0.elapsed().as_secs_f64() * 1000.0 / runs as f64);
    }

    fn fan_spread(m: &XgbModel) -> Vec<(f64, f64)> {
        let (fc, pc) = (ChannelConfig::defaults(ControlKind::Fan), ChannelConfig::defaults(ControlKind::Pump));
        let (fcal, pcal) = (fc.calibration(), pc.calibration());
        let cfg = ControlConfig::default();
        let op = Operating { duty: 80.0, rpm: 1200.0, power: None, current: None };
        let fan = Axis { name: "fan", cfg: &fc, cal: &fcal, free: true, base: op, zero_ok: true };
        let pump = Axis { name: "pump", cfg: &pc, cal: &pcal, free: false, base: Operating { duty: 50.0, rpm: 3400.0, ..op }, zero_ok: false };
        let out: Vec<(f64, f64)> = fan
            .candidates(cfg.fan_candidates)
            .into_iter()
            .map(|rpm| {
                let mut hold = HashMap::new();
                fan.hold(rpm, &mut hold);
                pump.hold(pump.base.rpm, &mut hold);
                (rpm, m.forecast(&hold, cfg.rollout_secs, cfg.horizon_secs).unwrap().water)
            })
            .collect();
        let plan = Planner { cfg: &cfg }.plan(m, &fan, &pump, &Temps { water: Some(31.0) }, 0.5, None).unwrap();
        assert!(plan.water.is_some_and(f64::is_finite));
        out
    }

    fn step_model_at_200_w() -> (XgbModel, f64) {
        let g = Golden::load();
        let mut m = XgbModel::load(&[Golden::dir().join("fixtures").join("steptrain")], "feature_spec_deploy.json", "model_deploy.json").unwrap();
        let mut water = f64::NAN;
        for t in 3000..g.rows.len() {
            let mut row = g.row(t);
            for (c, w) in [("/intelcpu/0/power/0", 110.0), ("nvml/0/power", 90.0), ("/gpu-nvidia/0/power/0", 90.0)] {
                if row.contains_key(c) {
                    row.insert(c.to_string(), w);
                }
            }
            water = row.get("d5next/water/temp").copied().unwrap_or(water);
            m.observe(&row, g.k0 + t as i64);
        }
        (m, water)
    }

    fn forecast_at(m: &XgbModel, fan_rpm: f64, horizon: u32) -> f64 {
        let (fc, pc) = (ChannelConfig::defaults(ControlKind::Fan), ChannelConfig::defaults(ControlKind::Pump));
        let (fcal, pcal) = (fc.calibration(), pc.calibration());
        let op = Operating { duty: 80.0, rpm: 1200.0, power: None, current: None };
        let fan = Axis { name: "fan", cfg: &fc, cal: &fcal, free: true, base: op, zero_ok: true };
        let pump = Axis { name: "pump", cfg: &pc, cal: &pcal, free: false, base: Operating { duty: 50.0, rpm: 3400.0, ..op }, zero_ok: false };
        let mut hold = HashMap::new();
        fan.hold(fan_rpm, &mut hold);
        pump.hold(pump.base.rpm, &mut hold);
        m.forecast(&hold, ControlConfig::default().rollout_secs, horizon).unwrap().water
    }

    #[test]
    fn exported_step_model_is_identified_and_its_forecast_follows_the_fan_rpm() {
        let (m, water) = step_model_at_200_w();
        assert_eq!(m.identified(), (true, false));
        assert!(m.warm_secs() > 1800, "{}", m.warm_secs());
        let curve = fan_spread(&m);
        let old = fan_spread(&real());
        println!("step model {curve:?}
deployed   {old:?}");
        assert!(curve.windows(2).all(|w| w[0].1 >= w[1].1 - 0.02), "forecast must not rise with fan rpm: {curve:?}");
        let (stopped, slow, h) = (forecast_at(&m, 0.0, 120), forecast_at(&m, 735.0, 120), 120.0);
        let dead = 31.0 - 20.0 * (3400.0 - 2570.0) / (4800.0 - 2570.0);
        let plant = 23.8 * (735.0f64 / 1460.0).powf(1.61) * (1.0 + 0.03 * (3400.0 / 4800.0 - 1.0)) * (water - 25.0) * (h - dead) / 7100.0;
        let spread = stopped - slow;
        println!("water {water:.2} stopped {stopped:.3} 735 rpm {slow:.3} spread {spread:.3} plant {plant:.3}");
        assert!(spread / plant > 1.0 / 1.5 && spread / plant < 1.5, "spread {spread} plant {plant}");
        let per_min = stopped / ((h - dead) / 60.0);
        assert!(per_min > 1.4 / 1.5 && per_min < 1.4 * 1.5, "{per_min} K/min");
        assert!(old.windows(2).all(|w| w[0].1 >= w[1].1 - 0.02), "deployed forecast must not rise with fan rpm: {old:?}");
    }

    fn restart(g: &Golden, upto: usize, gap: usize, seed_secs: i64, name: &str) -> (XgbModel, i64) {
        use crate::hot::HotStore;
        use crate::lake::Tick;
        use crate::Row;
        let base = crate::now_micros() / 1_000_000 - 8000;
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("test-tmp").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut hot = HotStore::open(&dir, std::time::Duration::from_secs(1)).unwrap();
        for t in 0..upto {
            let rows = g.row(t).into_iter().map(|(c, v)| Row::new("lhm", c.clone(), c, "other", "", v)).collect();
            hot.append(&Tick { ts: (base + t as i64) * 1_000_000, rows, raw: None }).unwrap();
        }
        hot.rotate().unwrap();
        drop(hot);
        let mut m = g.model();
        let history = HotStore::recent(&dir, &|c| m.wants(c), seed_secs);
        let now = base + (upto + gap) as i64;
        println!("history {} first {} last {} expected last {}", history.len(), history.first().map_or(0, |h| h.0 - base), history.last().map_or(0, |h| h.0 - base), upto - 1);
        assert!(m.seed(&history, now, 300) > 0);
        (m, base)
    }

    fn worst(a: &XgbModel, b: &XgbModel) -> (f64, usize, String) {
        let (mut w, mut name) = (0.0f64, String::new());
        let mut nan = 0;
        for (i, (x, y)) in a.x.iter().zip(&b.x).enumerate() {
            if x.is_nan() != y.is_nan() {
                nan += 1;
            } else if !x.is_nan() {
                let d = (x - y).abs() / (1.0 + x.abs());
                if d > w {
                    w = d;
                    name = a.p.model.features[i].clone();
                }
            }
        }
        (w, nan, name)
    }

    fn compare(gap: usize, seed_secs: i64, name: &str) -> Vec<(usize, f64, usize, String)> {
        let g = Golden::load();
        let upto = 5400;
        let mut out = Vec::new();
        let (mut b, base) = restart(&g, upto, gap, seed_secs, name);
        let mut a = g.model();
        for t in (0..upto + gap).filter(|t| !g.row(*t).is_empty()) {
            a.observe(&g.row(t), base + t as i64);
        }
        for t in upto + gap..upto + gap + 700 {
            a.observe(&g.row(t), base + t as i64);
            b.observe(&g.row(t), base + t as i64);
            if [0, 1, 30, 120, 300, 600].contains(&(t - upto - gap)) {
                let (w, nan, f) = worst(&a, &b);
                out.push((t - upto - gap, w, nan, f));
            }
        }
        out
    }

    #[test]
    fn seeding_from_the_hot_store_matches_an_uninterrupted_run() {
        let exact = compare(0, 5400, "seed-exact");
        assert!(exact.iter().all(|(_, w, nan, _)| *w < 1e-9 && *nan == 0), "{exact:?}");
        let short = compare(0, 3600, "seed-3600");
        println!("history 3600 s, no gap: {short:?}");
        assert!(short.iter().all(|(_, w, nan, _)| *w < 0.1 && *nan == 0), "{short:?}");
        for gap in [60, 299] {
            let r = compare(gap, 3600, &format!("seed-gap{gap}"));
            println!("history 3600 s, gap {gap} s: {r:?}");
            assert!(r.iter().all(|(_, _, nan, _)| *nan == 0) && r.last().unwrap().1 < 0.1, "{r:?}");
        }
    }

    #[test]
    fn stale_history_and_a_running_model_are_not_seeded() {
        let g = Golden::load();
        let rows: Vec<(i64, HashMap<String, f64>)> = (0..100).map(|t| (1000 + t as i64, g.row(t))).collect();
        let mut m = g.model();
        assert_eq!(m.seed(&rows, 1099 + 301, 300), 0);
        assert_eq!(m.seed(&rows, 1000, 300), 0);
        assert_eq!(m.seed(&[], 1100, 300), 0);
        assert_eq!(m.seed(&rows, 1099 + 300, 300), 100);
        assert_eq!(m.seed(&rows, 1099 + 300, 300), 0);
    }
}

#[cfg(test)]
mod cost {
    use super::*;
    use crate::protocol::ControlKind;

    pub fn plan_ms(m: &dyn Model, reps: usize) -> (f64, f64) {
        let cfg = ControlConfig::default();
        let (fc, pc) = (ChannelConfig::defaults(ControlKind::Fan), ChannelConfig::defaults(ControlKind::Pump));
        let (fcal, pcal) = (fc.calibration(), pc.calibration());
        let op = Operating { duty: 80.0, rpm: 1200.0, power: None, current: None };
        let fan = Axis { name: "fan", cfg: &fc, cal: &fcal, free: true, base: op, zero_ok: true };
        let pump = Axis { name: "pump", cfg: &pc, cal: &pcal, free: true, base: Operating { duty: 50.0, rpm: 3400.0, ..op }, zero_ok: false };
        let temps = Temps { water: Some(31.0) };
        let planner = Planner { cfg: &cfg };
        let mut ts: Vec<f64> = (0..reps)
            .map(|_| {
                let t0 = std::time::Instant::now();
                planner.plan(m, &fan, &pump, &temps, 0.5, Some((1200.0, 3400.0))).unwrap();
                t0.elapsed().as_secs_f64() * 1e3
            })
            .collect();
        ts.sort_by(|a, b| a.total_cmp(b));
        (ts[ts.len() / 2], ts[ts.len() - 1])
    }

    fn warm(g: &Golden, mut m: XgbModel) -> XgbModel {
        for t in 3000..g.rows.len() {
            m.observe(&g.row(t), g.k0 + t as i64);
        }
        m
    }

    #[test]
    #[ignore]
    fn plan_cost_trees_and_linear() {
        let (gt, gl) = (Golden::load(), Golden::named("golden_linear.json"));
        for (name, g, m) in [("trees", &gt, gt.model()), ("linear", &gl, gl.linear())] {
            let m = warm(g, m);
            assert!(m.warm_secs() > 0, "{name} not warm");
            let (med, max) = plan_ms(&m, 40);
            let (roll, pred, obs) = m.parts(200);
            println!("COST {name}: plan median {med:.2} ms max {max:.2} ms; per forecast rollout {roll:.4} ms predict {pred:.4} ms; observe {obs:.4} ms; core share at 1 Hz {:.2} %, at 30 s {:.3} %", med / 10.0, med / 300.0);
        }
    }

    #[test]
    fn linear_model_loads_forecasts_and_the_planner_runs_it() {
        let g = Golden::named("golden_linear.json");
        let m = warm(&g, g.linear());
        assert_eq!(m.identified(), (false, false));
        assert!(m.warm_secs() > 1800, "{}", m.warm_secs());
        let hold: HashMap<String, f64> = [("d5next/fan/rpm".to_string(), 800.0), ("d5next/fan/duty".to_string(), 50.0)].into();
        let f = m.forecast(&hold, 120, 120).unwrap();
        assert!(f.water.is_finite());
        assert!(m.forecast(&hold, 120, 77).is_err());
        assert!(plan_ms(&m, 1).0 < 5000.0);
    }
}
