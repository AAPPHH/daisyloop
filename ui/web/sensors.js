import { $, $$, h, num, decimals, unitLabel, sparkPath, ageText } from './ui.js';
import { S, alarmed } from './state.js';

const SPARK = 90;

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
    this.sel = h('button', { class: 'sel', title: 'Im Verlauf anzeigen', onclick: () => history.toggle(this.id) }, '∿');
    this.nm = h('span', { class: 'nm' }, c.label);
    this.el = h('div', { class: 'row', 'data-s': (c.label + ' ' + c.hardware).toLowerCase() },
      this.nm, h('span', { class: 'v' }, this.v, h('small', {}, unitLabel(c.unit))), this.mn, this.mx, this.svg, this.sel);
  }

  render(c) {
    this.c = c;
    this.v.textContent = num(c.value, decimals(c.unit));
    this.el.classList.toggle('stale', !!c.stale);
    this.el.title = c.stale ? `Wert seit ${ageText(c.age_ms / 1000)} veraltet` : '';
    this.mn.textContent = num(c.min, decimals(c.unit));
    this.mx.textContent = num(c.max, decimals(c.unit));
    this.el.classList.toggle('alarm', alarmed(c.id));
    this.pathEl.setAttribute('d', sparkPath((S.trail.get(c.id) ?? []).slice(-SPARK), 112, 22));
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
    const cols = h('div', { class: 'row head' }, h('span', {}, 'Name'), h('span', { style: 'text-align:right' }, 'Wert'), h('span', { class: 'mm' }, 'Min'), h('span', { class: 'mm' }, 'Max'), h('span', {}, 'Verlauf'), h('span'));
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
