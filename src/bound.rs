use super::*;

impl Control {
    pub(super) fn bound_duty(&self, i: usize, now: i64) -> Option<f64> {
        let w = self.temps(now).water?;
        let c = &self.chans[i];
        let raw = if c.spec.kind == ControlKind::Fan && w < self.cfg.fan_start_water { 0.0 } else { c.min.at(w) };
        Some(if raw <= 0.0 { 0.0 } else { c.cfg.commandable(raw, false) })
    }

    pub(super) fn dwelling(&self, i: usize, now: i64) -> bool {
        let c = &self.chans[i];
        c.run_since.is_some_and(|s| ((now - s) as f64) < c.cfg.min_run_secs * 1000.0)
    }

    pub(super) fn enforce_bound(&mut self, now: i64) {
        for i in 0..self.chans.len() {
            let c = &self.chans[i];
            let (Some(cmd), Some(bound)) = (c.duty_cmd, self.bound_duty(i, now)) else { continue };
            let behind = bound - cmd >= c.cfg.deadband_duty.max(f64::EPSILON) && now - c.wrote_ms >= Self::BOUND_GAP_MS;
            if c.driving() && !c.release_due && bound > cmd && (cmd <= 0.0 || behind) {
                let (kind, drive) = (c.spec.kind, c.drive);
                self.restarted |= self.write(&[(kind, Setting::Duty(bound))], now, drive, &[kind]) && cmd <= 0.0;
            }
        }
    }

    pub(super) fn frozen_hit(&self, now: i64) -> Option<(f64, i64)> {
        self.frozen.as_ref().filter(|f| now - f.since >= Self::FROZEN_MS && f.cpu_hi - f.cpu_lo >= Self::FROZEN_CPU_SPAN_K).map(|f| (f.water, f.since))
    }

    pub(super) fn frozen_fault(&self, now: i64) -> Option<(String, bool)> {
        let (w, since) = self.frozen_hit(now)?;
        let fan = &self.chans[0];
        (fan.driving() && fan.duty_cmd.is_some_and(|d| d <= self.cfg.frozen_low_duty)).then(|| (format!("Wasserwert {w:.2} °C seit {} min unverändert trotz wechselnder Last, Lüfter steht oder läuft niedrig", (now - since) / 60_000), true))
    }
}
