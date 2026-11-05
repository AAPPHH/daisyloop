export const $ = (s, r = document) => r.querySelector(s);
export const $$ = (s, r = document) => [...r.querySelectorAll(s)];
export const clamp = (v, a, b) => Math.min(b, Math.max(a, v));
export const lerp = (a, b, t) => a + (b - a) * t;

export function h(tag, props, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(props || {})) {
    if (k === 'class') el.className = v;
    else if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
    else if (k === 'html') el.innerHTML = v;
    else el.setAttribute(k, v);
  }
  for (const kid of kids.flat()) if (kid != null) el.append(kid);
  return el;
}

const formats = {};
export function num(v, d = 1) {
  if (v == null || !isFinite(v)) return '–';
  formats[d] ||= new Intl.NumberFormat('de-DE', { minimumFractionDigits: d, maximumFractionDigits: d, useGrouping: false });
  return formats[d].format(v);
}
export const unitLabel = (u) => (u === 'RPM' ? 'U/min' : u);
export const decimals = (u) => ({ RPM: 0, MHz: 0, ns: 0, V: 3 })[u] ?? (['°C', 'W', '%', 'GB', 'MB', 'L/h', 'MB/s', 'KB/s'].includes(u) ? 1 : 2);
export const fmt = (v, u) => num(v, decimals(u)) + (u ? ' ' + unitLabel(u) : '');

const STOPS = [[0, [76, 201, 240]], [0.35, [94, 234, 212]], [0.6, [251, 212, 94]], [0.8, [255, 154, 60]], [1, [255, 95, 162]]];
export function heatRGB(t) {
  t = clamp(t, 0, 1);
  for (let i = 1; i < STOPS.length; i++) {
    if (t <= STOPS[i][0]) {
      const [t0, a] = STOPS[i - 1];
      const [t1, b] = STOPS[i];
      const k = (t - t0) / (t1 - t0);
      return a.map((x, j) => Math.round(lerp(x, b[j], k)));
    }
  }
  return STOPS[STOPS.length - 1][1];
}
export const heat = (t, a = 1) => `rgba(${heatRGB(t).join(',')},${a})`;
export const GRADIENT = 'linear-gradient(90deg,' + STOPS.map(([t, c]) => `rgb(${c.join(',')}) ${t * 100}%`).join(',') + ')';
export const WATER = [24, 46];
export const COMPONENT = [30, 100];
export const norm = (v, [a, b]) => (v - a) / (b - a);

export const motion = {
  reduced: matchMedia('(prefers-reduced-motion: reduce)').matches,
  paused: false,
  kick: () => {},
};
matchMedia('(prefers-reduced-motion: reduce)').addEventListener('change', (e) => (motion.reduced = e.matches));

const tweens = new Set();

export class Tween {
  constructor(el, d = 1) {
    this.el = el;
    this.d = d;
    this.cur = null;
    this.target = null;
    this.text = null;
  }
  write(t) {
    if (t !== this.text) {
      this.text = t;
      this.el.textContent = t;
    }
  }
  set(v, zero) {
    if (zero != null && v != null && v < 1) {
      this.cur = this.target = v;
      tweens.delete(this);
      return this.write(zero);
    }
    if (v == null) {
      this.cur = this.target = null;
      tweens.delete(this);
      return this.write('–');
    }
    this.target = v;
    if (this.cur == null || motion.reduced || motion.paused) {
      this.cur = v;
      tweens.delete(this);
      this.write(num(v, this.d));
    } else {
      tweens.add(this);
      motion.kick();
    }
  }
}

export function stepTweens(dt) {
  const k = 1 - Math.exp(-dt / 0.16);
  for (const t of tweens) {
    t.cur += (t.target - t.cur) * k;
    if (Math.abs(t.target - t.cur) < Math.pow(10, -t.d) / 2) {
      t.cur = t.target;
      tweens.delete(t);
    }
    t.write(num(t.cur, t.d));
  }
  return tweens.size > 0;
}

export function sparkPath(data, w, hgt, pad = 2) {
  if (data.length < 2) return '';
  let lo = Infinity, hi = -Infinity;
  for (const v of data) {
    if (v < lo) lo = v;
    if (v > hi) hi = v;
  }
  const span = Math.max(hi - lo, 0.5);
  const n = data.length - 1;
  let d = '';
  for (let i = 0; i <= n; i++) {
    const x = (i / n) * w;
    const y = hgt - pad - ((data[i] - lo) / span) * (hgt - pad * 2);
    d += (i ? 'L' : 'M') + x.toFixed(1) + ' ' + y.toFixed(1);
  }
  return d;
}

export function ageText(secs) {
  if (!isFinite(secs)) return '–';
  if (secs < 90) return `${Math.round(secs)} s`;
  if (secs < 5400) return `${Math.round(secs / 60)} min`;
  return `${(secs / 3600).toFixed(1).replace('.', ',')} h`;
}
