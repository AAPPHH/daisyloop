use crate::model::{Forecast, Live, Model};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};

#[derive(Deserialize, Clone, Debug)]
#[serde(default)]
pub struct CorrectionConfig {
    pub enabled: bool,
    pub half_life_secs: f64,
    pub update_secs: i64,
    pub min_updates: u32,
    pub bias_max: f64,
    pub scale_min: f64,
    pub scale_max: f64,
    pub restart_hold_secs: i64,
    pub resid_max: f64,
    pub jump_k: f64,
    pub p0_bias: f64,
    pub p0_scale: f64,
    pub save_secs: i64,
}

impl Default for CorrectionConfig {
    fn default() -> Self {
        CorrectionConfig { enabled: true, half_life_secs: 6.0 * 3600.0, update_secs: 10, min_updates: 180, bias_max: 1.0, scale_min: 0.5, scale_max: 1.5, restart_hold_secs: 600, resid_max: 4.0, jump_k: 0.5, p0_bias: 0.25, p0_scale: 0.09, save_secs: 60 }
    }
}

impl CorrectionConfig {
    pub fn validate(&self) -> Result<(), String> {
        let nums = [self.half_life_secs, self.bias_max, self.scale_min, self.scale_max, self.resid_max, self.jump_k, self.p0_bias, self.p0_scale];
        if nums.iter().any(|v| !v.is_finite() || *v <= 0.0) || self.update_secs < 1 || self.restart_hold_secs < 0 || self.save_secs < 1 {
            return Err("online correction value out of range".into());
        }
        if self.scale_min >= 1.0 || self.scale_max <= 1.0 || self.half_life_secs < 60.0 * self.update_secs as f64 {
            return Err("online correction scale bounds must straddle 1 and the half-life must span many updates".into());
        }
        Ok(())
    }

    fn lambda(&self) -> f64 {
        0.5f64.powf(self.update_secs as f64 / self.half_life_secs)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Rls {
    pub theta: [f64; 2],
    pub p: [[f64; 2]; 2],
    pub n: u32,
    pub rejected: u32,
}

impl Rls {
    pub fn fresh(c: &CorrectionConfig) -> Rls {
        Rls { theta: [0.0, 0.0], p: [[c.p0_bias, 0.0], [0.0, c.p0_scale]], n: 0, rejected: 0 }
    }

    fn sane(&self) -> bool {
        self.theta.iter().chain(self.p.iter().flatten()).all(|v| v.is_finite()) && self.p[0][0] > 0.0 && self.p[1][1] > 0.0
    }

    pub fn bounded(&self, c: &CorrectionConfig) -> bool {
        self.sane() && self.theta[0].abs() <= c.bias_max && self.theta[1] >= c.scale_min - 1.0 && self.theta[1] <= c.scale_max - 1.0
    }

    pub fn update(&mut self, c: &CorrectionConfig, resid: f64, response: f64) {
        let before = *self;
        let phi = [1.0, response];
        let lambda = c.lambda();
        let pp = [self.p[0][0] * phi[0] + self.p[0][1] * phi[1], self.p[1][0] * phi[0] + self.p[1][1] * phi[1]];
        let denom = lambda + phi[0] * pp[0] + phi[1] * pp[1];
        let k = [pp[0] / denom, pp[1] / denom];
        let err = resid - self.theta[0] * phi[0] - self.theta[1] * phi[1];
        self.theta[0] += k[0] * err;
        self.theta[1] += k[1] * err;
        for i in 0..2 {
            for j in 0..2 {
                self.p[i][j] = (self.p[i][j] - k[i] * pp[j]) / lambda;
            }
        }
        let off = 0.5 * (self.p[0][1] + self.p[1][0]);
        self.p[0][1] = off;
        self.p[1][0] = off;
        let (s0, s1) = ((c.p0_bias / self.p[0][0]).min(1.0), (c.p0_scale / self.p[1][1]).min(1.0));
        self.p[0][0] *= s0;
        self.p[1][1] *= s1;
        self.p[0][1] *= (s0 * s1).sqrt();
        self.p[1][0] = self.p[0][1];
        if !self.sane() {
            *self = Rls { theta: if before.sane() { before.theta } else { [0.0, 0.0] }, n: before.n, rejected: before.rejected + 1, ..Rls::fresh(c) };
        }
        let clamped = [self.theta[0].clamp(-c.bias_max, c.bias_max), self.theta[1].clamp(c.scale_min - 1.0, c.scale_max - 1.0)];
        if clamped != self.theta {
            self.theta = clamped;
            self.p = Rls::fresh(c).p;
        }
        self.n += 1;
    }

    pub fn delta(&self, c: &CorrectionConfig, response: f64) -> f64 {
        if self.n >= c.min_updates { self.theta[0] + self.theta[1] * response } else { 0.0 }
    }
}

#[derive(Clone, Debug)]
struct Sample {
    epoch: i64,
    water: f64,
    live: Vec<Live>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Shown {
    pub h: u32,
    pub bias: f64,
    pub scale: f64,
    pub delta: f64,
    pub updates: u32,
    pub acting: bool,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    version: u32,
    digest: String,
    horizons: BTreeMap<u32, Rls>,
}

pub struct Corrector {
    cfg: CorrectionConfig,
    horizons: BTreeMap<u32, Rls>,
    ring: VecDeque<Sample>,
    last_epoch: Option<i64>,
    last_water: Option<f64>,
    changed: i64,
    bad: i64,
    next_at: i64,
    live: Vec<Live>,
    digest: u64,
    path: PathBuf,
    dirty: bool,
    saved_at: i64,
}

impl Corrector {
    pub const FILE: &'static str = "online_correction.json";
    const VERSION: u32 = 1;

    pub fn new(cfg: &CorrectionConfig, dir: &Path, digest: u64) -> Corrector {
        let mut c = Corrector { cfg: cfg.clone(), horizons: BTreeMap::new(), ring: VecDeque::new(), last_epoch: None, last_water: None, changed: 0, bad: i64::MIN / 2, next_at: 0, live: Vec::new(), digest, path: dir.join(Self::FILE), dirty: false, saved_at: i64::MIN / 2 };
        if cfg.enabled && digest != 0 {
            c.load();
        }
        c
    }

    fn load(&mut self) {
        let Some(s) = std::fs::read_to_string(&self.path).ok().and_then(|t| serde_json::from_str::<Saved>(&t).ok()) else { return };
        if s.version != Self::VERSION || s.digest != format!("{:016x}", self.digest) {
            log::info!("online correction: stored state belongs to another model or version, discarded");
            return;
        }
        self.horizons = s.horizons.into_iter().filter(|(_, r)| r.bounded(&self.cfg)).collect();
    }

    pub fn save(&mut self, wall: i64, force: bool) {
        if !self.cfg.enabled || self.digest == 0 || !self.dirty || (!force && wall - self.saved_at < self.cfg.save_secs) {
            return;
        }
        self.saved_at = wall;
        let text = serde_json::to_string(&Saved { version: Self::VERSION, digest: format!("{:016x}", self.digest), horizons: self.horizons.clone() }).unwrap_or_default();
        let tmp = self.path.with_extension("json.tmp");
        match std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, &self.path)) {
            Ok(()) => self.dirty = false,
            Err(e) => log::warn!("online correction: state not saved ({e})"),
        }
    }

    pub fn enabled(&self) -> bool {
        self.cfg.enabled
    }

    pub fn hold_secs(&self) -> i64 {
        self.cfg.restart_hold_secs
    }

    pub fn state(&self) -> &BTreeMap<u32, Rls> {
        &self.horizons
    }

    pub fn feed(&mut self, epoch: i64, ok: bool, water: Option<f64>, live: &[Live]) {
        if !self.cfg.enabled {
            return;
        }
        let water = water.filter(|w| w.is_finite());
        let gap = self.last_epoch.is_some_and(|p| epoch != p + 1);
        let jump = water.zip(self.last_water).is_some_and(|(a, b)| (a - b).abs() > self.cfg.jump_k);
        if water != self.last_water {
            self.changed = epoch;
        }
        (self.last_epoch, self.last_water) = (Some(epoch), water);
        self.live = live.to_vec();
        let Some(w) = water.filter(|_| ok && !gap && !jump && !live.is_empty()) else {
            self.bad = epoch;
            self.ring.clear();
            return;
        };
        self.ring.push_back(Sample { epoch, water: w, live: live.to_vec() });
        let keep = live.iter().map(|l| l.h as i64).max().unwrap_or(0) + 1;
        while self.ring.len() as i64 > keep {
            self.ring.pop_front();
        }
        if epoch < self.next_at {
            return;
        }
        let mut done = false;
        for l in live {
            let h = l.h as i64;
            let Some(front) = self.ring.front().map(|s| s.epoch) else { break };
            let at = epoch - h - front;
            if at < 0 || self.bad > epoch - h || self.changed <= epoch - h {
                continue;
            }
            let Some(old) = self.ring.get(at as usize).filter(|s| s.epoch == epoch - h) else { continue };
            let Some(pred) = old.live.iter().find(|x| x.h == l.h) else { continue };
            let resid = (w - old.water) - pred.pred;
            let r = self.horizons.entry(l.h).or_insert_with(|| Rls::fresh(&self.cfg));
            if resid.abs() > self.cfg.resid_max {
                r.rejected += 1;
                continue;
            }
            r.update(&self.cfg, resid, pred.response);
            done = true;
        }
        if done {
            self.next_at = epoch + self.cfg.update_secs;
            self.dirty = true;
        }
    }

    pub fn delta(&self, h: u32, response: f64) -> f64 {
        if !self.cfg.enabled {
            return 0.0;
        }
        self.horizons.get(&h).map_or(0.0, |r| r.delta(&self.cfg, response))
    }

    pub fn shown(&self) -> Vec<Shown> {
        self.horizons
            .iter()
            .map(|(h, r)| {
                let response = self.live.iter().find(|l| l.h == *h).map_or(0.0, |l| l.response);
                Shown { h: *h, bias: r.theta[0], scale: 1.0 + r.theta[1], delta: r.delta(&self.cfg, response), updates: r.n, acting: r.n >= self.cfg.min_updates }
            })
            .collect()
    }
}

pub struct Corrected<'a> {
    pub inner: &'a dyn Model,
    pub corr: &'a Corrector,
}

impl Model for Corrected<'_> {
    fn observe(&mut self, _: &HashMap<String, f64>, _: i64) {}
    fn identified(&self) -> (bool, bool) {
        self.inner.identified()
    }
    fn warm_secs(&self) -> i64 {
        self.inner.warm_secs()
    }
    fn forecast(&self, hold: &HashMap<String, f64>, rollout_s: u32, horizon_s: u32) -> Result<Forecast, String> {
        let f = self.inner.forecast(hold, rollout_s, horizon_s)?;
        Ok(Forecast { water: f.water + self.corr.delta(horizon_s, f.response), ..f })
    }
}

#[cfg(test)]
#[path = "correct_tests.rs"]
mod tests;
