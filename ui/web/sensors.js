import { $, $$, h, num, decimals, unitLabel, scrub, sparkPath } from './ui.js';
import { S, send, alarmed } from './state.js';

const limitEdits = new Map();
let limitTimer = null;
const SPARK = 90;

const speed = (u) => ({ RPM: 10, MHz: 10, '°C': 0.5, V: 0.01 })[u] ?? 1;
const limitable = (c) => !['data', 'other'].includes(c.kind);

function effectiveLimit(id) {
  const e = limitEdits.get(id);
  if (e) return e;
  const l = S.snap.limits.find((x) => x.channel === id);
  return { low: l?.low ?? null, high: l?.high ?? null };
}

function editLimit(id, patch) {
  const cur = effectiveLimit(id);
  limitEdits.set(id, { low: cur.low, high: cur.high, ...patch, dirty: performance.now(), sent: false });
  clearTimeout(limitTimer);
  limitTimer = setTimeout(() => {
    for (const [ch, e] of limitEdits) {
      if (!e.sent) {
        e.sent = true;
        e.sentAt = performance.now();
        send({ cmd: 'set_limit', channel: ch, low: e.low, high: e.high }).catch(() => limitEdits.delete(ch));
      }
    }
  }, 260);
}

function settleLimits() {
  const now = performance.now();
  for (const [id, e] of limitEdits) {
    const l = S.snap.limits.find((x) => x.channel === id);
    if (e.sent && (now - e.sentAt > 3000 || ((l?.low ?? null) === e.low && (l?.high ?? null) === e.high))) limitEdits.delete(id);
  }
}

class SensorRow {
  constructor(c, history) {
    this.id = c.id;
    this.c = c;
    this.history = history;
    this.v = h('span');
    this.mn = h('span', { class: 'mm' });
    this.mx = h('span', { class: 'mm' });
    this.svg = h('span', { class: 'sp', html: '<svg class="spark" viewBox="0 0 112 22" preserveAspectRatio="none"><path></path></svg>' });
    this.pathEl = this.svg.firstChild.firstChild;
    this.cells = { low: h('div', { class: 'lim' }), high: h('div', { class: 'lim' }) };
    this.cellState = { low: 'x', high: 'x' };
    this.scrubs = {};
    this.sel = h('button', { class: 'sel', title: 'Im Verlauf anzeigen', onclick: () => history.toggle(this.id) }, '∿');
    this.nm = h('span', { class: 'nm' }, c.label);
    this.el = h('div', { class: 'row', 'data-s': (c.label + ' ' + c.hardware).toLowerCase() },
      this.nm, h('span', { class: 'v' }, this.v, h('small', {}, unitLabel(c.unit))), this.mn, this.mx, this.svg, this.cells.low, this.cells.high, this.sel);
  }

  limitCell(side) {
    const cur = effectiveLimit(this.id)[side];
    const cell = this.cells[side];
    const state = cur == null ? 'none' : 'set';
    if (!limitable(this.c)) return;
    if (state !== this.cellState[side]) {
      this.cellState[side] = state;
      cell.replaceChildren();
      if (state === 'none') {
        cell.append(h('button', { class: 'ghost', onclick: () => {
          const v = this.c.value ?? 0;
          editLimit(this.id, { [side]: side === 'low' ? Math.floor(v * 0.9) : Math.max(Math.ceil(v * 1.1), v + 1) });
        } }, side === 'low' ? '+ unten' : '+ oben'));
      } else {
        const d = decimals(this.c.unit);
        const s = scrub({ value: cur, step: speed(this.c.unit), min: -1e6, max: 1e6, dec: d, onInput: (v) => editLimit(this.id, { [side]: v }) });
        this.scrubs[side] = s;
        cell.append(s.el, h('button', { class: 'x', title: 'Grenzwert entfernen', onclick: () => editLimit(this.id, { [side]: null }) }, '×'));
      }
    } else if (state === 'set') this.scrubs[side].set(cur);
  }

  render(c) {
    this.c = c;
    this.v.textContent = num(c.value, decimals(c.unit));
    this.mn.textContent = num(c.min, decimals(c.unit));
    this.mx.textContent = num(c.max, decimals(c.unit));
    this.el.classList.toggle('alarm', alarmed(c.id));
    this.pathEl.setAttribute('d', sparkPath((S.trail.get(c.id) ?? []).slice(-SPARK), 112, 22));
    this.limitCell('low');
    this.limitCell('high');
    this.sel.classList.toggle('on', S.selected.includes(c.id));
  }
}

class Group {
  constructor(name, channels, history, rerender) {
    this.name = name;
    this.history = history;
    this.chs = channels;
    this.meta = channels.map((c) => (c.label + ' ' + c.hardware).toLowerCase());
    this.rows = [];
    this.open = false;
    this.count = h('span', { class: 'gcount' }, `${channels.length} Sensoren`);
    this.built = false;
    this.alarm = h('span', { class: 'gal' });
    this.head = h('span', { class: 'ghead' });
    const header = h('header', { onclick: () => (this.toggle(), rerender()) }, h('span', { class: 'gname' }, name), this.count, this.alarm, this.head);
    const cols = h('div', { class: 'row head' }, h('span', {}, 'Name'), h('span', { style: 'text-align:right' }, 'Wert'), h('span', { class: 'mm' }, 'Min'), h('span', { class: 'mm' }, 'Max'), h('span', {}, 'Verlauf'), h('span', {}, 'Untergrenze'), h('span', {}, 'Obergrenze'), h('span'));
    this.inner = h('div', {}, cols);
    this.el = h('div', { class: 'grp' }, header, h('div', { class: 'body' }, this.inner));
  }

  ensure() {
    if (this.built) return;
    this.built = true;
    this.rows = this.chs.map((c) => new SensorRow(c, this.history));
    this.inner.append(...this.rows.map((r) => r.el));
  }

  toggle(force) {
    this.open = force ?? !this.open;
    if (this.open) this.ensure();
    this.el.classList.toggle('open', this.open || this.forced);
  }

  render(channels, needle) {
    const byId = new Map(channels.map((c) => [c.id, c]));
    const matches = this.chs.map((c, k) => !needle || this.meta[k].includes(needle));
    const shown = matches.filter(Boolean).length;
    let alarms = 0;
    let top = null;
    for (const c of this.chs) {
      const cur = byId.get(c.id);
      if (alarmed(c.id)) alarms++;
      if (cur.kind === 'temperature' && cur.value != null && (top == null || cur.value > top.value)) top = cur;
    }
    this.forced = !!needle && shown > 0;
    this.el.style.display = shown ? '' : 'none';
    this.el.classList.toggle('open', this.open || this.forced);
    this.count.textContent = needle ? `${shown} von ${this.chs.length}` : `${this.chs.length} Sensoren`;
    this.alarm.textContent = alarms ? `${alarms} Alarm${alarms > 1 ? 'e' : ''}` : '';
    this.alarm.style.display = alarms ? '' : 'none';
    this.head.textContent = top ? `max. ${num(top.value, 1)} °C` : '';
    if (this.open || this.forced) {
      this.ensure();
      this.rows.forEach((r, k) => {
        r.el.style.display = matches[k] ? '' : 'none';
        if (matches[k]) r.render(byId.get(r.id));
      });
    }
  }
}

export class Sensors {
  constructor(history) {
    this.history = history;
    this.groups = [];
    this.sig = '';
    this.needle = '';
    $('#search').addEventListener('input', (e) => {
      this.needle = e.target.value.trim().toLowerCase();
      this.render();
    });
    $('#open-all').addEventListener('click', () => (this.groups.forEach((g) => g.toggle(true)), this.render()));
    $('#close-all').addEventListener('click', () => (this.groups.forEach((g) => g.toggle(false)), this.render()));
  }

  render() {
    const snap = S.snap;
    if (!snap) return;
    settleLimits();
    const sig = snap.channels.map((c) => c.id).join('|').length + ':' + snap.channels.length;
    if (sig !== this.sig) {
      this.sig = sig;
      const map = new Map();
      for (const c of snap.channels) {
        if (!map.has(c.hardware)) map.set(c.hardware, []);
        map.get(c.hardware).push(c);
      }
      const host = $('#groups');
      host.replaceChildren();
      this.groups = [...map].map(([name, chs]) => new Group(name, chs, this.history, () => this.render()));
      this.groups.forEach((g) => host.append(g.el));
      $('#sensor-count').textContent = snap.channels.length;
    }
    for (const g of this.groups) g.render(snap.channels, this.needle);
  }
}
