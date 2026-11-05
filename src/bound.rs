use super::*;

impl Control {
    pub(super) fn curve_bound(&self, i: usize, now: i64) -> Option<f64> {
        let w = self.temps(now).water?;
        let c = &self.chans[i];
        let raw = if c.spec.kind == ControlKind::Fan && w < self.cfg.fan_start_water { 0.0 } else { c.min.at(w) };
        Some(if raw <= 0.0 { 0.0 } else { c.cfg.commandable(raw, false) })
    }

    pub(super) fn bound_duty(&self, i: usize, now: i64) -> Option<f64> {
        let raw = self.curve_bound(i, now)?;
        if self.writes && self.frozen_hit(now).is_some() {
            return Some(self.chans[i].cfg.max_duty);
        }
        Some(self.chans[i].latch.as_ref().map_or(raw, |l| l.duty.max(raw)))
    }

    pub(super) fn track_bounds(&mut self, now: i64) {
        let Some(w) = self.temps(now).water else { return };
        let (fall, hold, step) = (self.cfg.bound_drop_k, (self.cfg.bound_hold_secs * 1000.0) as i64, self.cfg.bound_raise_step);
        for i in 0..self.chans.len() {
            let Some(raw) = self.curve_bound(i, now) else { continue };
            let c = &mut self.chans[i];
            let (floor, max) = (c.cfg.floor_duty, c.cfg.max_duty);
            c.seen.push_back((now, raw));
            while c.seen.front().is_some_and(|x| now - x.0 > hold) {
                c.seen.pop_front();
            }
            let fresh = |duty: f64| Latch { duty, water: w, low_since: None };
            let lifted = |raw: f64, running: bool| if raw > floor && running { (raw + step).min(max) } else { raw };
            c.latch = Some(match c.latch.take() {
                None => fresh(raw),
                Some(l) if raw > l.duty => fresh(lifted(raw, l.duty > 0.0)),
                Some(l) => {
                    let low_since = if raw < l.duty && w <= l.water - fall { l.low_since.or(Some(now)) } else { None };
                    if low_since.is_some_and(|s| now - s >= hold) {
                        let duty = c.seen.iter().fold(raw, |m, x| m.max(x.1));
                        Latch { duty, water: l.water, low_since: (duty > raw).then_some(now) }
                    } else {
                        Latch { water: l.water.max(w), low_since, ..l }
                    }
                }
            });
        }
    }

    pub(super) fn raise_open(c: &mut Chan, now: i64, cap: u32) -> bool {
        while c.raises.front().is_some_and(|t| now - t >= Self::RAISE_WINDOW_MS) {
            c.raises.pop_front();
        }
        c.raises.len() < cap as usize
    }

    pub(super) fn dwelling(&self, i: usize, now: i64) -> bool {
        let c = &self.chans[i];
        c.run_since.is_some_and(|s| ((now - s) as f64) < c.cfg.min_run_secs * 1000.0)
    }

    pub(super) fn enforce_bound(&mut self, now: i64) {
        let frozen = self.writes && self.frozen_hit(now).is_some();
        for i in 0..self.chans.len() {
            let Some(bound) = self.bound_duty(i, now) else { continue };
            let open = Self::raise_open(&mut self.chans[i], now, self.cfg.bound_raise_cap);
            let c = &self.chans[i];
            let Some(cmd) = c.duty_cmd.or((frozen && c.mode == Mode::Active).then_some(0.0)) else { continue };
            let behind = bound - cmd >= c.cfg.deadband_duty.max(f64::EPSILON) && now - c.wrote_ms >= Self::BOUND_GAP_MS;
            if (c.driving() || frozen && c.mode == Mode::Active) && !c.release_due && bound > cmd && (cmd <= 0.0 || behind) {
                let kind = c.spec.kind;
                let exempt = if open || frozen { vec![kind] } else { vec![] };
                self.restarted |= self.write(&[(kind, Setting::Duty(bound))], now, Drive::Duty, &exempt) && cmd <= 0.0;
            }
        }
    }

    pub(super) fn frozen_hit(&self, now: i64) -> Option<(f64, i64, &'static str)> {
        let f = self.frozen.as_ref()?;
        let held = now - f.since;
        if held >= Self::FROZEN_MS {
            Some((f.water, f.since, "bei jeder Last"))
        } else if held >= Self::HEAT_MIN_MS && f.heat_k >= Self::HEAT_MARGIN_K {
            Some((f.water, f.since, "trotz Heizleistung"))
        } else {
            None
        }
    }

    pub(super) fn frozen_fault(&self, now: i64) -> Option<(String, bool)> {
        let (w, since, why) = self.frozen_hit(now)?;
        Some((format!("Wasserwert {w:.2} °C seit {} s unverändert {why}, Messung unglaubwürdig", (now - since) / 1000), true))
    }
}
