use crate::control::{Calibration, ChannelConfig, ControlConfig};
use model_infer::Predictor;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug)]
pub struct Forecast {
    pub water: f64,
    pub cpu: f64,
    pub gpu: f64,
}

pub trait Model {
    fn observe(&mut self, row: &HashMap<String, f64>, epoch: i64);
    fn identified(&self) -> (bool, bool);
    fn warm_secs(&self) -> i64;
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
}

impl XgbModel {
    pub const KEY_CHANNEL: &'static str = "d5next/water/temp";
    pub const FILL_SECS: i64 = 10;

    pub fn from_text(spec: &str, model: &str) -> Result<XgbModel, String> {
        let p = Predictor::load(spec, model)?;
        let channels = p.engine.raw_channels.clone();
        if !channels.iter().any(|c| c == Self::KEY_CHANNEL) {
            return Err("feature spec has no water channel".into());
        }
        let n = channels.len();
        Ok(XgbModel { fresh: p.clone(), p, last: vec![f64::NAN; n], fill: vec![f64::NAN; n], fill_at: vec![0; n], x: vec![], channels, epoch: 0, since: 0 })
    }

    pub fn load(dirs: &[PathBuf], spec: &str, file: &str) -> Result<XgbModel, String> {
        let read = |name: &str| -> Result<String, String> {
            dirs.iter().find_map(|d| std::fs::read_to_string(d.join(name)).ok()).ok_or_else(|| format!("{name} not found in {}", dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(", ")))
        };
        XgbModel::from_text(&read(spec)?, &read(file)?)
    }

    pub fn search_dirs(data_dir: &Path, install_dir: Option<&Path>) -> Vec<PathBuf> {
        vec![install_dir.unwrap_or(data_dir).join("models")]
    }

    fn filled(&self) -> Vec<f64> {
        (0..self.channels.len()).map(|i| if self.epoch - self.fill_at[i] <= Self::FILL_SECS { self.fill[i] } else { f64::NAN }).collect()
    }
}

impl Model for XgbModel {
    fn observe(&mut self, row: &HashMap<String, f64>, epoch: i64) {
        let raw: Vec<f64> = self.channels.iter().map(|c| row.get(c).copied().filter(|v| v.is_finite()).unwrap_or(f64::NAN)).collect();
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

    fn identified(&self) -> (bool, bool) {
        (self.p.model.identified.fan, self.p.model.identified.pump)
    }

    fn warm_secs(&self) -> i64 {
        if !self.x.is_empty() && self.x.iter().all(|v| v.is_finite()) { self.since } else { 0 }
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
        Ok(Forecast { water: get("water")?, cpu: get("cpu")?, gpu: get("gpu")? })
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
}

impl Axis<'_> {
    pub fn rpm_range(&self) -> (f64, f64) {
        (self.cal.rpm(self.cfg.floor_duty), self.cal.rpm(self.cfg.max_duty))
    }

    fn candidates(&self, n: usize) -> Vec<f64> {
        if !self.free {
            return vec![self.base.rpm];
        }
        let (lo, hi) = self.rpm_range();
        (0..n).map(|i| if i + 1 == n { hi } else { lo + (hi - lo) * i as f64 / (n - 1) as f64 }).collect()
    }

    fn hold(&self, rpm: f64, out: &mut HashMap<String, f64>) {
        let rpm = if self.free { rpm.clamp(self.rpm_range().0, self.rpm_range().1) } else { self.base.rpm };
        let duty = if self.free { self.cal.duty(rpm) } else { self.base.duty };
        let r = if self.base.rpm > 1.0 { rpm / self.base.rpm } else { 1.0 };
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
    pub cpu: Option<f64>,
    pub gpu: Option<f64>,
}

impl Temps {
    pub fn all(&self) -> bool {
        self.water.is_some() && self.cpu.is_some() && self.gpu.is_some()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Plan {
    pub fan_rpm: f64,
    pub pump_rpm: f64,
    pub water: Option<f64>,
    pub cpu: Option<f64>,
    pub gpu: Option<f64>,
    pub limits: [f64; 3],
    pub feasible: bool,
}

pub struct Planner<'a> {
    pub cfg: &'a ControlConfig,
}

impl Planner<'_> {
    pub fn limits(cfg: &ControlConfig, weight: f64) -> [f64; 3] {
        let shift = weight.clamp(0.0, 1.0) * cfg.weight_span_k;
        [cfg.water_plan_limit - shift, cfg.cpu_plan_limit - shift, cfg.gpu_plan_limit - shift]
    }

    fn noise(&self, fan: &Axis, pump: &Axis, f: f64, p: f64) -> f64 {
        let n = |a: &Axis, r: f64| (r / a.rpm_range().1.max(1.0)).powi(3);
        n(fan, f) + self.cfg.pump_noise * n(pump, p)
    }

    fn within(plan: &Plan, margin: f64) -> bool {
        [plan.water, plan.cpu, plan.gpu].iter().zip(plan.limits).all(|(t, l)| t.is_some_and(|t| t <= l - margin))
    }

    pub fn plan(&self, model: &dyn Model, fan: &Axis, pump: &Axis, now: &Temps, weight: f64, prev: Option<(f64, f64)>) -> Result<Plan, String> {
        let limits = Self::limits(self.cfg, weight);
        let eval = |f: f64, p: f64| -> Result<Plan, String> {
            let mut hold = HashMap::new();
            fan.hold(f, &mut hold);
            pump.hold(p, &mut hold);
            let fc = model.forecast(&hold, self.cfg.rollout_secs, self.cfg.horizon_secs)?;
            Ok(Plan { fan_rpm: f, pump_rpm: p, water: now.water.map(|t| t + fc.water), cpu: now.cpu.map(|t| t + fc.cpu), gpu: now.gpu.map(|t| t + fc.gpu), limits, feasible: true })
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
            Some((f, p)) if now.all() => {
                let (f, p) = (if fan.free { f.clamp(fan.rpm_range().0, fan.rpm_range().1) } else { fan.base.rpm }, if pump.free { p.clamp(pump.rpm_range().0, pump.rpm_range().1) } else { pump.base.rpm });
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
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(Self::dir().join("golden.json")).unwrap()).unwrap();
        let channels = v["channels"].as_array().unwrap().iter().map(|c| c.as_str().unwrap().to_string()).collect();
        let rows = v["rows"].as_array().unwrap().iter().map(|r| r.as_array().unwrap().iter().map(|x| x.as_f64()).collect()).collect();
        Golden { channels, rows, k0: v["k0"].as_i64().unwrap() }
    }
    pub fn model(&self) -> XgbModel {
        XgbModel::load(&[Self::dir()], "feature_spec_deploy.json", "model_deploy.json").unwrap()
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
    fn shipped_model_loads_is_not_identified_and_forecasts_finite_values() {
        let m = real();
        assert_eq!(m.identified(), (false, false));
        assert!(m.warm_secs() > 1800, "{}", m.warm_secs());
        let hold: HashMap<String, f64> = [("d5next/fan/rpm".to_string(), 800.0), ("d5next/fan/duty".to_string(), 50.0)].into();
        let f = m.forecast(&hold, 120, 120).unwrap();
        assert!(f.water.is_finite() && f.cpu.is_finite() && f.gpu.is_finite());
        assert!(m.forecast(&hold, 120, 77).is_err());
        assert!(XgbModel::load(&[PathBuf::from("nowhere")], "a", "b").is_err());
    }

    #[test]
    fn planner_runs_the_shipped_model_over_the_grid_fast_enough_for_the_tick() {
        let m = real();
        let cfg = ControlConfig::default();
        let (fc, pc) = (ChannelConfig::defaults(ControlKind::Fan), ChannelConfig::defaults(ControlKind::Pump));
        let (fcal, pcal) = (fc.calibration(), pc.calibration());
        let op = Operating { duty: 80.0, rpm: 1200.0, power: None, current: None };
        let fan = Axis { name: "fan", cfg: &fc, cal: &fcal, free: true, base: op };
        let pump = Axis { name: "pump", cfg: &pc, cal: &pcal, free: true, base: Operating { duty: 50.0, rpm: 3400.0, ..op } };
        let t0 = std::time::Instant::now();
        let plan = Planner { cfg: &cfg }.plan(&m, &fan, &pump, &Temps { water: Some(31.0), cpu: Some(70.0), gpu: Some(55.0) }, 0.5, None).unwrap();
        assert!(t0.elapsed().as_secs_f64() < 5.0, "{:?}", t0.elapsed());
        assert!(plan.fan_rpm >= fan.rpm_range().0 && plan.fan_rpm <= fan.rpm_range().1 && plan.pump_rpm >= pump.rpm_range().0);
    }
}
