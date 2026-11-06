import { $, h, num, clamp } from './ui.js';
import { S, KEYS, value, isStale } from './state.js';

export const HEAT = {
  C: 11700,
  etaCpu: 0.96,
  etaGpu: 1,
  P0: 0,
  window: 120,
  gap: 3.5,
  delay: { lowRpm: 2500, lowS: 31, highRpm: 4800, highS: 11, maxS: 33 },
  fastWindow: 15,
  stepW: 100,
  transientS: 180,
  band: 25,
  bandOut: 40,
  scale: 200,
  limit: 46,
  floorC: 28,
  tauMin: 8,
  pumpFallback: 11,
  pumpPower: 'd5next/pump/power',
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
    let last = this.pts[this.pts.length - 1];
    if (last && Math.abs(t - last.t) > this.cfg.gap) (this.reset(), (last = null));
    if (last && t <= last.t) return;
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
    const removedRaw = produced - balance;
    return { building: false, now, delay: d, produced, fast, removed: Math.max(0, removedRaw), clamped: removedRaw < 0, balance, water: wm, dir: this.dir, transient };
  }
}

const sign = (v) => (v < 0 ? '−' : '+');
const span = (min) => (min < 1 ? 'unter 1 min' : `${Math.round(min)} min`);

export class Heat {
  constructor() {
    this.hb = new HeatBalance();
    const col = (cls, k, unit) => h('div', { class: 'hc ' + cls }, h('span', { class: 'k' }, k), h('b', {}, h('span', { class: 'v' }, '–'), h('small', {}, unit)), h('em', { class: 'sub' }));
    this.el = h('div', { class: 'heat off', id: 'heat', 'aria-live': 'off' },
      h('div', { class: 'hc-row' },
        col('in', 'Erzeugt', 'W'),
        h('div', { class: 'hc mid' }, h('span', { class: 'k verdict' }, 'Bilanz'), h('b', {}, h('span', { class: 'v' }, '–'), h('small', {}, 'W')), h('em', { class: 'sub' })),
        col('out', 'Abgeführt', 'W')),
      h('div', { class: 'beam' }, h('i', { class: 'fill' }), h('i', { class: 'tick' })),
      h('div', { class: 'credit' }, h('div', { class: 'cr-track' }, h('i', { class: 'cr-fill' })), h('span', { class: 'cr-l' }), h('span', { class: 'cr-r' })));
    $('#hero').append(this.el);
    new IntersectionObserver(([e]) => this.el.classList.toggle('away', !e.isIntersecting)).observe(this.el);
    const q = (s) => $(s, this.el);
    this.r = { inV: q('.in .v'), inS: q('.in .sub'), outV: q('.out .v'), outS: q('.out .sub'), midV: q('.mid .v'), midS: q('.mid .sub'), verdict: q('.verdict'), fill: q('.beam .fill'), cr: q('.cr-fill'), crL: q('.cr-l'), crR: q('.cr-r') };
  }

  missing() {
    const bad = (id) => value(id) == null || isStale(id);
    if (bad(KEYS.water)) return 'Keine aktuellen Werte';
    if (bad(KEYS.cpuPower)) return 'CPU-Leistung fehlt';
    if (bad(KEYS.gpuPower)) return 'GPU-Leistung fehlt';
    return null;
  }

  update(live) {
    const ts = S.snap?.ts_ms;
    const miss = live && S.snap ? (ts === this.deadTs ? 'Keine aktuellen Werte' : this.missing()) : 'Keine aktuellen Werte';
    if (!live) this.deadTs = ts;
    if (miss) {
      this.hb.reset();
      return this.paint(null, null, miss);
    }
    const rpm = S.ctl.get(KEYS.pump)?.rpm_channel;
    const pump = value(HEAT.pumpPower) == null || isStale(HEAT.pumpPower) ? HEAT.pumpFallback : value(HEAT.pumpPower);
    this.hb.push(ts / 1000, { w: value(KEYS.water), cpu: value(KEYS.cpuPower), gpu: value(KEYS.gpuPower), pump, rpm: (rpm && value(rpm)) ?? HEAT.delay.highRpm });
    this.paint(this.hb.compute(), value(KEYS.water));
  }

  paint(c, water, note = '') {
    const { r, el } = this;
    const txt = (n, t) => n.textContent !== t && (n.textContent = t);
    const limit = S.snap?.limits.find((l) => l.channel === KEYS.water)?.plan ?? HEAT.limit;
    let state = 'off';
    if (c?.building) state = 'building';
    else if (c) state = c.dir;
    const over = c && water >= limit;
    el.dataset.state = state;
    el.classList.toggle('off', state === 'off');
    el.classList.toggle('building', state === 'building');
    el.classList.toggle('over', !!over);
    el.classList.toggle('transient', !!c?.transient);
    el.classList.toggle('unsure', !!c?.clamped);
    if (!c || c.building) {
      for (const k of ['inV', 'outV', 'midV']) txt(r[k], '–');
      for (const k of ['inS', 'outS']) txt(r[k], '');
      txt(r.verdict, c ? 'Messfenster füllt sich' : note);
      txt(r.midS, c ? `${Math.round(c.progress * 100)} %` : '');
      txt(r.crL, '');
      txt(r.crR, '');
      r.fill.style.width = '0';
      r.cr.style.width = '0';
      el.style.setProperty('--prog', c ? c.progress : 0);
      return;
    }
    const b = c.balance;
    const credit = Math.max(0, (HEAT.C * (limit - water)) / 1000);
    const full = (HEAT.C * (limit - HEAT.floorC)) / 1000;
    const x = clamp(Math.abs(b) / HEAT.scale, 0, 1) * 50;
    r.cr.style.width = clamp(credit / full, 0, 1) * 100 + '%';
    r.fill.style.left = (b > 0 ? 50 - x : 50) + '%';
    r.fill.style.width = (c.dir === 'even' ? 0 : x) + '%';
    txt(r.inV, num(c.produced, 0));
    txt(r.outV, num(c.removed, 0));
    txt(r.midV, c.dir === 'even' ? '≈ 0' : sign(b) + num(Math.abs(b), 0));
    txt(r.inS, c.transient && Math.abs(c.fast - c.produced) > HEAT.stepW ? `jetzt ${num(c.fast, 0)} W` : '');
    txt(r.outS, c.clamped ? 'unsicher' : c.transient ? 'stellt sich ein' : '');
    const verdict = { use: 'Kredit wird aufgebraucht', build: 'Kredit baut sich auf', even: 'ausgeglichen' }[c.dir];
    txt(r.verdict, over ? (c.dir === 'build' ? 'Über dem Limit, kühlt ab' : 'Kredit aufgebraucht') : verdict);
    const minLeft = (HEAT.C * (limit - water)) / b / 60;
    const kj = num((Math.abs(b) * 60) / 1000, 1);
    let sub = 'Wasser hält die Temperatur';
    if (over) sub = `Wasser über ${num(limit, 0)} °C`;
    else if (c.dir === 'use') sub = minLeft <= HEAT.tauMin ? `bei gleicher Bilanz ${span(minLeft)} bis ${num(limit, 0)} °C` : `${kj} kJ pro Minute verbraucht`;
    else if (c.dir === 'build') sub = `${kj} kJ pro Minute frei`;
    txt(r.midS, sub);
    txt(r.crL, `Kredit ${num(credit, 0)} kJ bis ${num(limit, 0)} °C`);
    txt(r.crR, `${num(water, 1)} °C`);
  }
}
