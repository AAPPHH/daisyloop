import { $, $$, h, fmt, unitLabel, num, motion } from './ui.js';
import { S, requestHistory, channel, age } from './state.js';

const COLORS = ['#67d4f5', '#fbd45e', '#7ee787', '#c4a1ff', '#ff9a3c', '#5eead4', '#f08fc8', '#cddc7a'];
const RANGES = { '1h': 3600e3, '1d': 86400e3, '1w': 7 * 86400e3 };
const REFRESH = { '1h': 10e3, '1d': 60e3, '1w': 300e3 };
export const colorOf = (id) => COLORS[Math.max(0, S.selected.indexOf(id)) % COLORS.length];

function monotone(ctx, xs, ys, move = true) {
  const n = xs.length;
  if (n < 2) return;
  const d = [], m = [];
  for (let i = 0; i < n - 1; i++) d.push((ys[i + 1] - ys[i]) / (xs[i + 1] - xs[i] || 1));
  m.push(d[0]);
  for (let i = 1; i < n - 1; i++) m.push(d[i - 1] * d[i] <= 0 ? 0 : (d[i - 1] + d[i]) / 2);
  m.push(d[n - 2]);
  for (let i = 0; i < n - 1; i++) {
    if (d[i] === 0) m[i] = m[i + 1] = 0;
    else {
      const a = m[i] / d[i], b = m[i + 1] / d[i], s = a * a + b * b;
      if (s > 9) {
        const t = 3 / Math.sqrt(s);
        m[i] = t * a * d[i];
        m[i + 1] = t * b * d[i];
      }
    }
  }
  move ? ctx.moveTo(xs[0], ys[0]) : ctx.lineTo(xs[0], ys[0]);
  for (let i = 0; i < n - 1; i++) {
    const dx = (xs[i + 1] - xs[i]) / 3;
    ctx.bezierCurveTo(xs[i] + dx, ys[i] + m[i] * dx, xs[i + 1] - dx, ys[i + 1] - m[i + 1] * dx, xs[i + 1], ys[i + 1]);
  }
}

function niceStep(span, target) {
  const raw = span / target;
  const mag = Math.pow(10, Math.floor(Math.log10(raw)));
  return [1, 2, 5, 10].map((m) => m * mag).find((s) => s >= raw) ?? raw;
}

class Lane {
  constructor(unit, owner) {
    this.unit = unit;
    this.owner = owner;
    this.cv = h('canvas');
    this.tip = h('div', { class: 'tip' });
    this.el = h('div', { class: 'lane' }, h('span', { class: 'u' }, unitLabel(unit)), this.cv, this.tip);
    this.series = [];
    new ResizeObserver(() => this.draw()).observe(this.el);
    this.el.addEventListener('pointermove', (e) => owner.hover(e.clientX, this));
    this.el.addEventListener('pointerleave', () => owner.hover(null));
  }

  geometry() {
    const rem = parseFloat(getComputedStyle(document.documentElement).fontSize);
    const r = this.el.getBoundingClientRect();
    return { W: r.width, H: r.height, left: r.left, pl: 3.6 * rem, pr: 1.1 * rem, pt: 2.3 * rem, pb: 1.7 * rem, rem };
  }

  draw() {
    const g = this.geometry();
    if (!g.W) return;
    const dpr = devicePixelRatio || 1;
    this.cv.width = Math.round(g.W * dpr);
    this.cv.height = Math.round(g.H * dpr);
    const ctx = this.cv.getContext('2d');
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    const { t0, t1 } = this.owner.window;
    const px0 = g.pl, px1 = g.W - g.pr, py0 = g.pt, py1 = g.H - g.pb;
    let lo = Infinity, hi = -Infinity;
    for (const s of this.series) for (const v of s.values) (lo = Math.min(lo, v)), (hi = Math.max(hi, v));
    if (lo > hi) (lo = 0), (hi = 1);
    if (this.unit === '%') (lo = 0), (hi = 100);
    else if (this.unit === 'RPM') (lo = 0), (hi = Math.max(100, hi * 1.1));
    else {
      const pad = Math.max((hi - lo) * 0.12, 0.6);
      lo -= pad;
      hi += pad;
    }
    this.map = { lo, hi, px0, px1, py0, py1, t0, t1 };
    const X = (t) => px0 + ((t - t0) / (t1 - t0)) * (px1 - px0);
    const Y = (v) => py1 - ((v - lo) / (hi - lo)) * (py1 - py0);
    ctx.font = `${Math.round(g.rem * 0.74)}px 'Inter Tight', sans-serif`;
    ctx.textBaseline = 'middle';
    const step = niceStep(hi - lo, Math.max(2, (py1 - py0) / (g.rem * 2.6)));
    ctx.lineWidth = 1;
    for (let v = Math.ceil(lo / step) * step; v <= hi; v += step) {
      const y = Math.round(Y(v)) + 0.5;
      ctx.strokeStyle = 'rgba(255,255,255,0.05)';
      ctx.beginPath();
      ctx.moveTo(px0, y);
      ctx.lineTo(px1, y);
      ctx.stroke();
      ctx.fillStyle = '#566071';
      ctx.textAlign = 'right';
      ctx.fillText(num(v, step < 1 ? 1 : 0), px0 - 10, y);
    }
    const fmtT = new Intl.DateTimeFormat('de-DE', t1 - t0 > 2 * 86400e3 ? { weekday: 'short', hour: '2-digit', minute: '2-digit' } : { hour: '2-digit', minute: '2-digit' });
    const ticks = Math.max(2, Math.floor((px1 - px0) / (g.rem * 7)));
    ctx.textBaseline = 'top';
    for (let k = 0; k <= ticks; k++) {
      const t = t0 + ((t1 - t0) * k) / ticks;
      ctx.fillStyle = '#566071';
      ctx.textAlign = k === 0 ? 'left' : k === ticks ? 'right' : 'center';
      ctx.fillText(fmtT.format(t), X(t), py1 + g.rem * 0.55);
    }
    for (const s of this.series) {
      if (s.ts.length < 2) continue;
      const xs = s.ts.map(X), ys = s.values.map(Y);
      const grad = ctx.createLinearGradient(0, py0, 0, py1);
      grad.addColorStop(0, s.color + '40');
      grad.addColorStop(1, s.color + '00');
      ctx.beginPath();
      monotone(ctx, xs, ys);
      ctx.lineTo(xs[xs.length - 1], py1);
      ctx.lineTo(xs[0], py1);
      ctx.closePath();
      ctx.fillStyle = grad;
      ctx.fill();
      ctx.beginPath();
      monotone(ctx, xs, ys);
      ctx.lineWidth = 2;
      ctx.lineJoin = 'round';
      ctx.strokeStyle = s.color;
      ctx.stroke();
    }
    const ht = this.owner.hoverT;
    if (ht != null && ht >= t0 && ht <= t1) {
      const x = X(ht);
      const lg = ctx.createLinearGradient(0, py0, 0, py1);
      lg.addColorStop(0, 'rgba(255,255,255,0)');
      lg.addColorStop(0.5, 'rgba(255,255,255,0.35)');
      lg.addColorStop(1, 'rgba(255,255,255,0)');
      ctx.fillStyle = lg;
      ctx.fillRect(x - 0.5, py0, 1, py1 - py0);
      const rows = [];
      for (const s of this.series) {
        let i = s.ts.findIndex((t) => t >= ht);
        if (i < 0) i = s.ts.length - 1;
        if (i > 0 && ht - s.ts[i - 1] < s.ts[i] - ht) i--;
        const px = X(s.ts[i]), py = Y(s.values[i]);
        ctx.beginPath();
        ctx.arc(px, py, 4.5, 0, Math.PI * 2);
        ctx.fillStyle = '#0b0f14';
        ctx.fill();
        ctx.lineWidth = 2;
        ctx.strokeStyle = s.color;
        ctx.stroke();
        rows.push(`<div class="r" style="--c:${s.color}"><span><i></i>${s.label}</span><b>${fmt(s.values[i], this.unit)}</b></div>`);
      }
      const dt = new Intl.DateTimeFormat('de-DE', { weekday: 'short', day: '2-digit', month: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit' }).format(ht);
      this.tip.innerHTML = `<div class="t">${dt}</div>${rows.join('')}`;
      this.tip.style.opacity = 1;
      const tw = this.tip.offsetWidth;
      this.tip.style.left = Math.min(Math.max(x + 14, 8), g.W - tw - 8) + 'px';
      if (x + 14 + tw > g.W - 8) this.tip.style.left = Math.max(8, x - tw - 14) + 'px';
    } else this.tip.style.opacity = 0;
  }
}

export class History {
  constructor() {
    this.lanes = new Map();
    this.data = new Map();
    this.tag = 0;
    this.key = '';
    this.lastFetch = 0;
    this.window = { t0: 0, t1: 1 };
    this.hoverT = null;
    this.raf = 0;
    this.open = false;
    $('#gm-lanes').addEventListener('click', () => this.show(!this.open));
    $('#lanes-close').addEventListener('click', () => this.show(false));
    addEventListener('keydown', (e) => e.key === 'Escape' && this.open && this.show(false));
    this.count();
    $$('#range button').forEach((b) =>
      b.addEventListener('click', () => {
        if (S.range === b.dataset.r) return;
        S.range = b.dataset.r;
        this.thumb();
        this.fetch(true);
      }),
    );
    addEventListener('resize', () => this.thumb());
    this.thumb();
  }

  show(on) {
    this.open = on;
    $('#lane-panel').hidden = !on;
    $('#gm-lanes').classList.toggle('on', on);
    $('#gm-lanes').setAttribute('aria-expanded', on);
    if (!on) return;
    this.chips();
    this.thumb();
    this.fetch(true);
  }

  count() {
    $('#gm-count').textContent = S.selected.length || '';
  }

  thumb() {
    const seg = $('#range');
    const b = $(`#range button[data-r="${S.range}"]`);
    $$('#range button').forEach((x) => x.classList.toggle('on', x === b));
    const thumb = $('#range i');
    thumb.style.width = b.offsetWidth + 'px';
    thumb.style.transform = `translateX(${b.offsetLeft - 3}px)`;
  }

  toggle(id) {
    const i = S.selected.indexOf(id);
    if (i >= 0) {
      S.selected.splice(i, 1);
      S.hidden.delete(id);
      this.data.delete(id);
    } else S.selected.push(id);
    this.count();
    this.chips();
    this.fetch(true);
  }

  chips() {
    if (!this.open) return;
    const host = $('#chips');
    host.replaceChildren();
    for (const id of S.selected) {
      const c = channel(id);
      if (!c) continue;
      host.append(
        h('span', { class: 'chip' + (S.hidden.has(id) ? ' off' : ''), style: `--c:${colorOf(id)}` },
          h('i'),
          h('span', { class: 'name', onclick: () => { S.hidden.has(id) ? S.hidden.delete(id) : S.hidden.add(id); this.chips(); this.render(); } }, `${c.label} (${unitLabel(c.unit)})`),
          h('button', { class: 'x', title: 'Aus Verlauf entfernen', onclick: () => this.toggle(id) }, '×')),
      );
    }
    if (!S.selected.length) host.append(h('span', { class: 'empty' }, 'Kanäle über ∿ in der Sensorliste hinzufügen.'));
  }

  fetch(force = false) {
    if (!this.open || !S.live || !S.snap) return;
    const key = S.range + '|' + S.selected.join();
    if (!force && key === this.key && performance.now() - this.lastFetch < REFRESH[S.range]) return;
    if (key !== this.key) $('#lanes').classList.add('loading');
    this.key = key;
    this.lastFetch = performance.now();
    const to = S.snap.ts_ms;
    this.tag++;
    this.pending = { tag: this.tag, t0: to - RANGES[S.range], t1: to };
    requestHistory(this.tag, { channels: [...S.selected], from_ms: to - RANGES[S.range], to_ms: to, max_points: 700 });
  }

  onHistory({ tag, response }) {
    if (!this.pending || tag !== this.pending.tag) return;
    this.window = { t0: this.pending.t0, t1: this.pending.t1 };
    this.data.clear();
    for (const s of response.series) this.data.set(s.channel, s);
    $('#lanes').classList.remove('loading');
    this.render();
  }

  render() {
    const host = $('#lanes');
    const byUnit = new Map();
    for (const id of S.selected) {
      const s = this.data.get(id), c = channel(id);
      if (!s || !c || S.hidden.has(id)) continue;
      if (!byUnit.has(c.unit)) byUnit.set(c.unit, []);
      byUnit.get(c.unit).push({ label: c.label, color: colorOf(id), ts: s.ts_ms, values: s.values });
    }
    for (const [u, l] of this.lanes) if (!byUnit.has(u)) (l.el.remove(), this.lanes.delete(u));
    for (const [u, series] of byUnit) {
      let l = this.lanes.get(u);
      if (!l) {
        l = new Lane(u, this);
        this.lanes.set(u, l);
      }
      l.series = series;
      host.append(l.el);
    }
    host.classList.toggle('two', this.lanes.size > 1);
    if (!this.lanes.size && !host.firstChild) host.append(h('p', { class: 'empty' }, 'Keine Verlaufsdaten.'));
    this.lanes.forEach((l) => l.draw());
  }

  hover(clientX, lane) {
    if (clientX == null) this.hoverT = null;
    else {
      const m = lane.map;
      if (!m) return;
      const g = lane.geometry();
      const f = (clientX - g.left - m.px0) / (m.px1 - m.px0);
      this.hoverT = f < 0 || f > 1 ? null : m.t0 + f * (m.t1 - m.t0);
    }
    cancelAnimationFrame(this.raf);
    this.raf = requestAnimationFrame(() => this.lanes.forEach((l) => l.draw()));
  }
}
