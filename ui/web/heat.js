import { clamp } from './ui.js';

export const HEAT = {
  C: 11700,
  etaCpu: 0.96,
  etaGpu: 1,
  P0: 0,
  window: 120,
  gap: 3.5,
  delay: { lowRpm: 2500, lowS: 31, highRpm: 4800, highS: 11, maxS: 33 },
  fastWindow: 15,
  stepW: 50,
  transientS: 240,
  band: 25,
  bandOut: 40,
  scale: 200,
  limit: 46,
  floorC: 28,
  horizonMin: 180,
};

export class HeatBalance {
  constructor(cfg = HEAT) {
    this.cfg = cfg;
    this.reset();
  }

  reset() {
    this.pts = [];
    this.t0 = null;
    this.dir = 'even';
    this.stepT = -1e9;
    this.inStep = false;
  }

  produced(s) {
    const c = this.cfg;
    return c.etaCpu * s.cpu + c.etaGpu * s.gpu + s.pump + c.P0;
  }

  delayFor(rpm) {
    const d = this.cfg.delay;
    return clamp(d.lowS + ((rpm - d.lowRpm) / (d.highRpm - d.lowRpm)) * (d.highS - d.lowS), d.highS, d.maxS);
  }

  push(t, s) {
    const last = this.pts[this.pts.length - 1];
    if (last && t <= last.t) return;
    if (last && t - last.t > this.cfg.gap) this.reset();
    if (this.t0 == null) this.t0 = t;
    this.pts.push({ t, w: s.w, p: this.produced(s), rpm: s.rpm });
    const keep = t - this.cfg.window - this.cfg.delay.maxS - 10;
    let i = 0;
    while (this.pts[i].t < keep) i++;
    if (i) this.pts.splice(0, i);
  }

  mean(a, b, key) {
    let s = 0, n = 0;
    for (const p of this.pts) if (p.t > a && p.t <= b) (s += p[key]), n++;
    return n ? s / n : null;
  }

  compute() {
    const c = this.cfg;
    const now = this.pts.length ? this.pts[this.pts.length - 1].t : null;
    if (now == null) return { building: true, progress: 0 };
    const d = this.delayFor(this.mean(now - c.window, now, 'rpm') ?? c.delay.highRpm);
    const need = c.window + d;
    if (now - this.t0 < need - 1) return { building: true, progress: clamp((now - this.t0) / need, 0, 1) };
    const win = this.pts.filter((p) => p.t > now - c.window);
    const n = win.length;
    if (n < 30) return { building: true, progress: 0.99 };
    const tm = win.reduce((s, p) => s + p.t, 0) / n;
    const wm = win.reduce((s, p) => s + p.w, 0) / n;
    let sxy = 0, sxx = 0;
    for (const p of win) (sxy += (p.t - tm) * (p.w - wm)), (sxx += (p.t - tm) ** 2);
    const balance = c.C * (sxy / sxx);
    const produced = this.mean(now - c.window - d, now - d, 'p');
    const fast = this.mean(now - c.fastWindow, now, 'p');
    const off = Math.abs(fast - produced) > c.stepW;
    if (off && !this.inStep) this.stepT = now;
    this.inStep = off;
    const transient = now - this.stepT < c.transientS;
    const a = Math.abs(balance);
    if (this.dir === 'even') {
      if (a > c.bandOut) this.dir = balance > 0 ? 'use' : 'build';
    } else if (a < c.band) this.dir = 'even';
    else this.dir = balance > 0 ? 'use' : 'build';
    return { building: false, now, delay: d, produced, fast, removed: produced - balance, balance, water: wm, dir: this.dir, transient };
  }
}
