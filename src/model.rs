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
    channels: Vec<String>,
    key: usize,
    last: Vec<f64>,
    epoch: i64,
    streak: i64,
}

impl XgbModel {
    pub const KEY_CHANNEL: &'static str = "d5next/water/temp";

    pub fn from_text(spec: &str, model: &str) -> Result<XgbModel, String> {
        let p = Predictor::load(spec, model)?;
        let channels = p.engine.raw_channels.clone();
        let key = channels.iter().position(|c| c == Self::KEY_CHANNEL).ok_or("feature spec has no water channel")?;
        Ok(XgbModel { last: vec![f64::NAN; channels.len()], p, channels, key, epoch: 0, streak: 0 })
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
}

impl Model for XgbModel {
    fn observe(&mut self, row: &HashMap<String, f64>, epoch: i64) {
        let raw: Vec<f64> = self.channels.iter().map(|c| row.get(c).copied().filter(|v| v.is_finite()).unwrap_or(f64::NAN)).collect();
        if (self.epoch != 0 && epoch - self.epoch > 10) || raw[self.key].is_nan() {
            self.streak = 0;
        }
        self.streak += 1;
        self.epoch = epoch;
        self.p.features(&raw, epoch);
        self.last = raw;
    }

    fn identified(&self) -> (bool, bool) {
        (self.p.model.identified.fan, self.p.model.identified.pump)
    }

    fn warm_secs(&self) -> i64 {
        self.streak
    }

    fn forecast(&self, hold: &HashMap<String, f64>, rollout_s: u32, horizon_s: u32) -> Result<Forecast, String> {
        if self.epoch == 0 {
            return Err("model has seen no data".into());
        }
        let mut raw = self.last.clone();
        for (i, c) in self.channels.iter().enumerate() {
            if let Some(v) = hold.get(c) {
                raw[i] = *v;
            }
        }
        let mut p = self.p.clone();
        let mut k = self.epoch;
        for _ in 0..rollout_s {
            k += 1;
            p.features(&raw, k);
        }
        let x = p.features(&raw, k + 1);
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
        (0..n).map(|i| lo + (hi - lo) * i as f64 / (n - 1) as f64).collect()
    }

    fn hold(&self, rpm: f64, out: &mut HashMap<String, f64>) {
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

#[derive(Clone, Copy, Debug)]
pub struct Plan {
    pub fan_rpm: f64,
    pub pump_rpm: f64,
    pub water: Option<f64>,
    pub cpu: Option<f64>,
    pub gpu: Option<f64>,
    pub feasible: bool,
}

pub struct Planner<'a> {
    pub cfg: &'a ControlConfig,
}

impl Planner<'_> {
    fn noise(&self, fan: &Axis, pump: &Axis, f: f64, p: f64) -> f64 {
        let n = |a: &Axis, r: f64| (r / a.rpm_range().1.max(1.0)).powi(3);
        n(fan, f) + self.cfg.pump_noise * n(pump, p)
    }

    fn predicted(t: Option<f64>, delta: f64) -> Option<f64> {
        t.map(|t| t + delta)
    }

    pub fn plan(&self, model: &dyn Model, fan: &Axis, pump: &Axis, now: &Temps, weight: f64) -> Result<Plan, String> {
        let shift = weight.clamp(0.0, 1.0) * self.cfg.weight_span_k;
        let limits = [self.cfg.water_plan_limit - shift, self.cfg.cpu_plan_limit - shift, self.cfg.gpu_plan_limit - shift];
        let mut best: Option<(f64, Plan)> = None;
        let mut hottest: Option<Plan> = None;
        for &f in &fan.candidates(self.cfg.fan_candidates) {
            for &p in &pump.candidates(self.cfg.pump_candidates) {
                let mut hold = HashMap::new();
                fan.hold(f, &mut hold);
                pump.hold(p, &mut hold);
                let fc = model.forecast(&hold, self.cfg.rollout_secs, self.cfg.horizon_secs)?;
                let plan = Plan { fan_rpm: f, pump_rpm: p, water: Self::predicted(now.water, fc.water), cpu: Self::predicted(now.cpu, fc.cpu), gpu: Self::predicted(now.gpu, fc.gpu), feasible: true };
                let ok = [plan.water, plan.cpu, plan.gpu].iter().zip(limits).all(|(t, l)| t.is_none_or(|t| t <= l));
                let cost = self.noise(fan, pump, f, p);
                if ok && best.as_ref().is_none_or(|b| cost < b.0) {
                    best = Some((cost, plan));
                }
                let max_f = !fan.free || f >= fan.rpm_range().1;
                let max_p = !pump.free || p >= pump.rpm_range().1;
                if max_f && max_p {
                    hottest = Some(Plan { feasible: false, ..plan });
                }
            }
        }
        best.map(|b| b.1).or(hottest).ok_or_else(|| "no candidates".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ControlKind;

    fn real() -> XgbModel {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models");
        let mut m = XgbModel::load(&[dir], "feature_spec_deploy.json", "model_deploy.json").unwrap();
        let row: HashMap<String, f64> = [("d5next/water/temp", 31.0), ("d5next/fan/duty", 80.0), ("d5next/fan/rpm", 1200.0), ("d5next/pump/power", 6.0), ("/intelcpu/0/power/0", 120.0), ("nvml/0/power", 150.0), ("nvml/0/temp", 55.0), ("/intelcpu/0/temperature/0", 70.0)].iter().map(|(k, v)| (k.to_string(), *v)).collect();
        for k in 0..60 {
            m.observe(&row, 1_800_000_000 + k);
        }
        m
    }

    #[test]
    fn shipped_model_loads_is_not_identified_and_forecasts_finite_values() {
        let m = real();
        assert_eq!(m.identified(), (false, false));
        assert_eq!(m.warm_secs(), 60);
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
        let plan = Planner { cfg: &cfg }.plan(&m, &fan, &pump, &Temps { water: Some(31.0), cpu: Some(70.0), gpu: Some(55.0) }, 0.5).unwrap();
        assert!(t0.elapsed().as_secs_f64() < 5.0, "{:?}", t0.elapsed());
        assert!(plan.fan_rpm >= fan.rpm_range().0 && plan.fan_rpm <= fan.rpm_range().1 && plan.pump_rpm >= pump.rpm_range().0);
    }
}
