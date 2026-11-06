import { $, h, num, heat, norm, sparkPath } from './ui.js';
import { S } from './state.js';

export const GLIDE_TAU_S = 2.5;
const SPARK_POINTS = 120;
const HOT = [30, 105];
const WATER_FLOOR = 24;
const ID = { cpu: '/intelcpu/0/', gpu: '/gpu-nvidia/0/', board: '/lpc/nct6687dr/0/' };

const re = (r) => (ix) => ix.ids.filter((i) => r.test(i));
const one = (...ids) => (ix) => ids.filter((i) => ix.has(i));
const cores = (kind, type) => (ix) => ix.ids.map((i) => ix.coreId(i, kind, type)).filter(Boolean);
const copies = (ix) => [8, 9, 10, 11, 12, 13].map((n) => ID.gpu + 'load/' + n).filter((i) => ix.has(i));

const TOP = [
  { label: 'Wasser', ids: one('d5next/water/temp'), unit: '°C', dec: 1, heat: 'water' },
  { label: 'Lüfter', ids: one('d5next/fan/rpm'), unit: 'U/min', dec: 0 },
  { label: 'Lüfter Duty', ids: one('d5next/fan/duty'), unit: '%', dec: 0 },
  { label: 'Pumpe', ids: one('d5next/pump/rpm'), unit: 'U/min', dec: 0 },
  { label: 'Pumpe Duty', ids: one('d5next/pump/duty'), unit: '%', dec: 0 },
];

const COLS = [
  { title: 'CPU', rows: [
    { label: 'P-Cores max', ids: cores('P', 'temperature'), agg: 'max', unit: '°C', dec: 0, heat: 'hot' },
    { label: 'E-Cores max', ids: cores('E', 'temperature'), agg: 'max', unit: '°C', dec: 0, heat: 'hot' },
    { label: 'Package', ids: one(ID.cpu + 'temperature/26'), unit: '°C', dec: 0, heat: 'hot' },
    { label: 'Package Leistung', ids: one(ID.cpu + 'power/0'), unit: 'W', dec: 1 },
    { label: 'IA Cores Leistung', ids: one(ID.cpu + 'power/1'), unit: 'W', dec: 1 },
    { label: 'Auslastung', ids: one(ID.cpu + 'load/0'), unit: '%', dec: 0 },
    { label: 'P-Cores Last', ids: cores('P', 'load'), agg: 'avg', unit: '%', dec: 0 },
    { label: 'E-Cores Last', ids: cores('E', 'load'), agg: 'avg', unit: '%', dec: 0 },
  ] },
  { title: 'GPU', rows: [
    { label: 'Temperatur', ids: one(ID.gpu + 'temperature/0'), unit: '°C', dec: 0, heat: 'hot' },
    { label: 'Hot Spot', ids: one(ID.gpu + 'temperature/2'), unit: '°C', dec: 0, heat: 'hot' },
    { label: 'Speichertemp.', ids: one(ID.gpu + 'temperature/3'), unit: '°C', dec: 0, heat: 'hot' },
    { label: 'Leistung', ids: one(ID.gpu + 'power/0'), unit: 'W', dec: 1 },
    { label: 'Power Limit', ids: one('nvml/0/power_limit'), unit: 'W', dec: 0 },
    { label: 'Auslastung', ids: one(ID.gpu + 'load/0'), unit: '%', dec: 0 },
    { label: 'Speicherlast', ids: one(ID.gpu + 'load/3'), unit: '%', dec: 0 },
    { label: 'Frame Buffer', ids: one(ID.gpu + 'load/1'), unit: '%', dec: 0 },
    { label: '3D', ids: one(ID.gpu + 'load/7'), unit: '%', dec: 0 },
    { label: 'Copy', ids: copies, agg: 'max', unit: '%', dec: 0 },
    { label: 'Video Engine', ids: one(ID.gpu + 'load/2'), unit: '%', dec: 0 },
    { label: 'VRAM dediziert', ids: one(ID.gpu + 'smalldata/3'), unit: 'MB', dec: 0 },
    { label: 'VRAM geteilt', ids: one(ID.gpu + 'smalldata/4'), unit: 'MB', dec: 0 },
    { label: 'PCIe Lesen', ids: one(ID.gpu + 'throughput/0'), unit: 'MB/s', dec: 0 },
  ] },
  { title: 'System', rows: [
    { label: 'Core', ids: one(ID.board + 'temperature/0'), unit: '°C', dec: 0 },
    { label: 'System', ids: one(ID.board + 'temperature/1'), unit: '°C', dec: 0 },
    { label: 'MOS', ids: one(ID.board + 'temperature/2'), unit: '°C', dec: 0 },
    { label: 'PCH', ids: one(ID.board + 'temperature/3'), unit: '°C', dec: 0 },
    { label: 'System 2 Lüfter', ids: one(ID.board + 'fan/11'), unit: 'U/min', dec: 0 },
    { label: 'RAM Last', ids: one('/ram/load/0'), unit: '%', dec: 0 },
    { label: 'RAM frei', ids: one('/ram/data/1'), unit: 'GB', dec: 1 },
    { label: 'Virtuell Last', ids: one('/vram/load/1'), unit: '%', dec: 0 },
    { label: 'Virtuell frei', ids: one('/vram/data/3'), unit: 'GB', dec: 1 },
    { label: 'DIMM', ids: re(/^\/memory\/dimm\/\d+\/temperature\/0$/), each: true, unit: '°C', dec: 1 },
  ] },
];

class Index {
  constructor(channels) {
    this.map = new Map(channels.map((c) => [c.id, c]));
    this.ids = [...this.map.keys()];
  }
  has(id) {
    return this.map.has(id);
  }
  coreId(id, kind, type) {
    const m = /^\/intelcpu\/0\/temperature\/(\d+)$/.exec(id);
    const c = m && this.map.get(id);
    if (!c || !new RegExp(`(^|\\s)${kind}-Core #\\d+$`).test(c.label)) return null;
    const out = `${ID.cpu}${type}/${m[1]}`;
    return this.map.has(out) ? out : null;
  }
}

class Metric {
  constructor(spec, ids, label, key) {
    this.key = key;
    this.spec = spec;
    this.ids = ids;
    this.label = label;
    this.ema = null;
    this.raw = null;
    this.min = null;
    this.max = null;
    this.trail = [];
    this.dirty = true;
  }
  read(ix) {
    const t = this.ids.map((i) => ix.map.get(i)).filter((c) => c && c.value != null);
    if (!t.length) return null;
    const vals = t.map((c) => (c.unit === 'B/s' ? c.value / 1e6 : c.value));
    if (this.spec.agg === 'max') return Math.max(...vals);
    if (this.spec.agg === 'avg') return vals.reduce((a, b) => a + b, 0) / vals.length;
    return vals[0];
  }
  stale(ix) {
    return this.ids.some((i) => ix.map.get(i)?.stale);
  }
  ingest(ix, alpha, tick) {
    const v = this.read(ix);
    this.raw = v;
    this.dirty = true;
    if (v == null) return;
    this.min = this.min == null ? v : Math.min(this.min, v);
    this.max = this.max == null ? v : Math.max(this.max, v);
    this.ema = this.ema == null ? v : this.ema + (v - this.ema) * alpha;
    if (tick) {
      this.trail.push(this.ema);
      if (this.trail.length > SPARK_POINTS) this.trail.shift();
    }
  }
  reset() {
    this.ema = this.min = this.max = this.raw;
    this.dirty = true;
  }
}

class View {
  constructor(metric, tile) {
    this.m = metric;
    this.cache = {};
    this.v = h('b');
    this.mn = h('span', { class: 'mm' });
    this.mx = h('span', { class: 'mm' });
    const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    svg.setAttribute('class', 'spark');
    svg.setAttribute('viewBox', '0 0 112 22');
    svg.setAttribute('preserveAspectRatio', 'none');
    this.path = document.createElementNS('http://www.w3.org/2000/svg', 'path');
    svg.append(this.path);
    const unit = h('small', {}, metric.spec.unit);
    const val = h('span', { class: 'v' }, this.v, unit);
    const nm = h('span', { class: 'nm', title: metric.label }, metric.label);
    this.el = tile
      ? h('div', { class: 'tile' }, nm, val, h('div', { class: 'tfoot' }, h('span', { class: 'tmm' }, h('span', {}, 'Min ', this.mn), h('span', {}, 'Max ', this.mx)), svg))
      : h('div', { class: 'gm-row' }, nm, svg, val, this.mn, this.mx);
  }
  set(key, fn, value) {
    if (this.cache[key] !== value) {
      this.cache[key] = value;
      fn(value);
    }
  }
  heatT() {
    const { spec } = this.m;
    const raw = this.m.raw;
    if (!spec.heat || raw == null) return null;
    if (spec.heat === 'water') {
      const lim = S.snap?.limits?.find((l) => l.channel === 'd5next/water/temp');
      return norm(Math.round(raw * 2) / 2, [WATER_FLOOR, lim?.top ?? 50]);
    }
    return norm(Math.round(raw), HOT);
  }
  paint(ix) {
    const m = this.m;
    const d = m.spec.dec;
    if (!m.dirty) return;
    m.dirty = false;
    const gone = m.raw == null;
    this.el.style.display = gone ? 'none' : '';
    if (gone) return;
    this.set('v', (t) => (this.v.textContent = t), num(m.ema, d));
    this.set('mn', (t) => (this.mn.textContent = t), num(m.min, d));
    this.set('mx', (t) => (this.mx.textContent = t), num(m.max, d));
    const t = this.heatT();
    this.set('c', (c) => (this.v.style.color = c), t == null ? '' : heat(t));
    this.set('over', (o) => this.el.classList.toggle('over', o), t != null && t >= 1);
    this.set('stale', (o) => this.el.classList.toggle('stale', o), m.stale(ix));
    this.set('d', (p) => this.path.setAttribute('d', p), sparkPath(m.trail, 112, 22));
  }
}

export class Gaming {
  constructor() {
    this.sig = '';
    this.metrics = [];
    this.views = [];
    this.lastTs = 0;
    this.ix = null;
    $('#gm-reset').addEventListener('click', () => {
      this.metrics.forEach((m) => m.reset());
      this.paint();
    });
  }
  build(ix) {
    const old = new Map(this.metrics.map((m) => [m.key, m]));
    this.metrics = [];
    this.views = [];
    const strip = $('#gm-strip');
    const cols = $('#gm-cols');
    strip.replaceChildren();
    cols.replaceChildren();
    const make = (spec, host, tile, group) => {
      const found = spec.ids(ix);
      const groups = spec.each ? found.map((i) => [[i], `${spec.label} ${/dimm\/(\d+)/.exec(i)[1]}`]) : [[found, spec.label]];
      for (const [ids, label] of groups) {
        const m = new Metric(spec, ids, label, group + '|' + label);
        const o = old.get(m.key);
        if (o) Object.assign(m, { ema: o.ema, raw: o.raw, min: o.min, max: o.max, trail: o.trail });
        const v = new View(m, tile);
        this.metrics.push(m);
        this.views.push(v);
        host.append(v.el);
      }
    };
    TOP.forEach((s) => make(s, strip, true, 'top'));
    for (const col of COLS) {
      const rows = h('div', { class: 'gm-rows' });
      cols.append(h('section', { class: 'gm-card' }, h('header', {}, h('h3', {}, col.title), h('span'), h('span', {}, 'Wert'), h('span', { class: 'mm' }, 'Min'), h('span', { class: 'mm' }, 'Max')), rows));
      col.rows.forEach((s) => make(s, rows, false, col.title));
    }
  }
  ingest() {
    const snap = S.snap;
    if (!snap) return;
    const ix = new Index(snap.channels);
    this.ix = ix;
    const sig = ix.ids.length + ':' + ix.ids[0] + ':' + ix.ids[ix.ids.length - 1];
    if (sig !== this.sig) {
      this.sig = sig;
      this.build(ix);
    }
    const tick = snap.ts_ms !== this.lastTs;
    const gap = (snap.ts_ms - this.lastTs) / 1000;
    const dt = this.lastTs && gap > 0 ? Math.min(5, gap) : 1;
    if (!tick) return;
    this.lastTs = snap.ts_ms;
    const alpha = 1 - Math.exp(-dt / GLIDE_TAU_S);
    this.metrics.forEach((m) => m.ingest(ix, alpha, true));
  }
  paint() {
    if (!this.ix) return;
    this.views.forEach((v) => v.paint(this.ix));
  }
}
