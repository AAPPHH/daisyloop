import { $, h, Tween, clamp, num, ageText } from './ui.js';
import { S, KEYS, send, channel, isStale, ageMs, value } from './state.js';

const MODES = [
  ['pid', 'PID'],
  ['model', 'Modell'],
];
const ARM_MS = 8000;
const rows = new Map();
let built = '';
let toastTimer = 0;

export function toast(text) {
  const el = $('#toast');
  el.textContent = text;
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (el.hidden = true), 7000);
}

export async function command(cmd, row) {
  try {
    await send(cmd);
    row?.refuse(null);
    return true;
  } catch (e) {
    const msg = String(e?.message ?? e);
    row ? row.refuse(msg) : toast(msg);
    return false;
  }
}

const driveText = (c) => {
  if (c.drive === 'duty') return `Direkte Duty ${num(c.commanded_duty, 0)} %`;
  if (c.drive === 'test') return `Stufentest, Duty ${num(c.commanded_duty, 0)} %`;
  return 'Firmware-PID regelt';
};

class Row {
  constructor(c) {
    this.id = c.id;
    this.kind = c.kind;
    this.armed = { mode: null, test: null };
    this.refusal = null;
    this.el = h('article', { class: 'ch' });
    this.rpm = h('span', { class: 'rpm' });
    this.rpmT = new Tween(this.rpm, 0);
    this.duty = h('b');
    this.age = h('span', { class: 'age' });
    this.target = h('div', { class: 'tgt-line' });
    this.state = h('span', { class: 'state' });
    this.fallback = h('div', { class: 'fallback' });
    this.thumb = h('i');
    this.btns = MODES.map(([m, label]) => h('button', { 'data-m': m, onclick: () => this.pick(m) }, label));
    this.seg = h('div', { class: 'seg modes' }, this.thumb, this.btns);
    this.confirm = h('div', { class: 'confirm' });
    this.floorEl = h('b', { class: 'floor' });
    this.fill = h('b', { class: 'fill' });
    this.liveDot = h('b', { class: 'live' });
    this.tgtDot = h('b', { class: 'tgt' });
    this.scale = h('div', { class: 'scale' }, h('span'), h('span'));
    this.track = h('div', { class: 'track out rpmtrack' }, this.floorEl, this.fill, this.liveDot, this.tgtDot);
    this.notes = h('div', { class: 'notes' });
    this.test = h('div', { class: 'steptest' });
    this.el.append(
      h('div', { class: 'who' }, h('h3', {}, c.label), h('div', { class: 'reading' }, this.rpm, h('small', {}, 'U/min'), h('span', { class: 'duty' }, this.duty, ' % Duty')), this.age, this.target, this.state, this.fallback),
      h('div', { class: 'ctl' },
        h('span', { class: 'k' }, 'Modus'), h('div', {}, this.seg, this.confirm),
        h('span', { class: 'k' }, 'Drehzahl'), h('div', {}, this.track, this.scale),
        h('span', { class: 'k' }, 'Hinweis'), this.notes,
        h('span', { class: 'k' }, 'Stufentest'), this.test,
      ),
    );
  }

  refuse(msg) {
    this.refusal = msg && { msg, at: performance.now() };
    msg && toast(msg);
    this.paint();
  }

  arm(what, value) {
    this.armed[what] = value && { value, at: performance.now() };
    this.paint();
  }

  pick(mode) {
    const c = S.ctl.get(this.id);
    if (!c || ((c.chosen ?? c.mode) === mode && !c.fallback)) return;
    if (mode === 'model' && c.kind === 'pump') return this.arm('mode', 'model');
    this.arm('mode', null);
    command({ cmd: 'set_mode', id: this.id, mode, confirmed: false }, this);
  }

  async setActivePump() {
    this.arm('mode', null);
    await command({ cmd: 'set_mode', id: this.id, mode: 'model', confirmed: true }, this);
  }

  async startTest() {
    this.arm('test', null);
    await command({ cmd: 'start_step_test', id: this.id, confirmed: true }, this);
  }

  paint() {
    const c = S.ctl.get(this.id);
    if (!c) return;
    const now = performance.now();
    for (const k of Object.keys(this.armed)) if (this.armed[k] && now - this.armed[k].at > ARM_MS) this.armed[k] = null;
    if (this.refusal && now - this.refusal.at > ARM_MS) this.refusal = null;
    this.btns.forEach((b, i) => {
      const on = b.dataset.m === (c.chosen ?? c.mode);
      b.classList.toggle('on', on);
      b.classList.toggle('locked', b.dataset.m === 'model' && (!c.allowed || !c.identified));
      if (on) {
        this.thumb.style.width = b.offsetWidth + 'px';
        this.thumb.style.transform = `translateX(${b.offsetLeft - 3}px)`;
      }
    });
    this.confirm.replaceChildren();
    if (this.armed.mode) {
      this.confirm.append(
        h('span', {}, 'Pumpe direkt über das Modell steuern?'),
        h('button', { class: 'go', onclick: () => this.setActivePump() }, 'Ja, aktivieren'),
        h('button', { class: 'no', onclick: () => this.arm('mode', null) }, 'Abbrechen'),
      );
    }
    this.confirm.hidden = !this.armed.mode;
    const step = S.snap.step_test?.id === this.id ? S.snap.step_test : null;
    this.test.replaceChildren();
    if (step) {
      this.test.append(h('span', { class: 'prog' }, `Stufe ${step.phase} von ${step.phases} · ${num(step.duty, 0)} % Duty${step.overridden ? ' (vom Mindestwert angehoben)' : ''} · noch ${step.left_s} s`), h('button', { class: 'no', onclick: () => command({ cmd: 'stop_step_test' }, this) }, 'Stopp'));
    } else if (this.armed.test) {
      this.test.append(h('span', {}, 'Fährt feste Duty-Stufen. Nur mit dir am Rechner.'), h('button', { class: 'go', onclick: () => this.startTest() }, 'Ja, starten'), h('button', { class: 'no', onclick: () => this.arm('test', null) }, 'Abbrechen'));
    } else {
      this.test.append(h('button', { class: 'ghost', onclick: () => this.arm('test', true) }, 'Stufentest starten …'));
    }
    const locks = [];
    if (!c.allowed) locks.push('Schreiben ist deaktiviert (writes_enabled)');
    if (!c.identified) locks.push('das Modell kennt die Antwort dieses Kanals noch nicht (nicht identifiziert)');
    const lock = locks.length ? 'Modell gesperrt: ' + locks.join('; ') + '.' : null;
    const lines = [];
    if (c.mode !== 'model' && lock) lines.push(h('p', { class: 'lock' }, lock));
    if (c.note) lines.push(h('p', { class: 'note' }, c.note));
    if (this.refusal) lines.push(h('p', { class: 'refusal' }, 'Abgelehnt: ' + this.refusal.msg));
    this.notes.replaceChildren(...lines);
  }

  render(c) {
    const stale = isStale(c.rpm_channel);
    const ch = channel(c.rpm_channel);
    this.rpmT.set(ch?.value ?? null, this.kind === 'fan' ? 'steht' : undefined);
    this.el.classList.toggle('still', this.kind === 'fan' && ch?.value != null && ch.value < 1 && !stale);
    const dch = channel(c.duty_channel);
    this.duty.textContent = num(dch?.value ?? null, 0);
    this.el.classList.toggle('stale', stale || isStale(c.duty_channel));
    this.age.textContent = stale ? `Messwert seit ${ageText(ageMs(c.rpm_channel) / 1000)} veraltet` : '';
    const shadow = c.mode === 'pid';
    this.target.replaceChildren();
    if (c.target_rpm != null) {
      this.target.append(h('span', { class: 'k' }, shadow || !c.identified ? `Soll (Modell rechnet mit${c.identified ? '' : ', nicht identifiziert'})` : 'Soll'), h('b', {}, num(c.target_rpm, 0)), h('small', {}, ' U/min'));
      this.target.classList.toggle('shadow', shadow);
    } else this.target.append(h('span', { class: 'k' }, 'Noch kein Soll'));
    this.state.className = 'state' + (c.drive === 'firmware' ? '' : ' drive');
    this.state.textContent = driveText(c);
    this.fallback.hidden = !c.fallback;
    this.fallback.textContent = c.fallback ? `Rückfall auf Firmware-PID: ${c.fallback}` : '';
    const max = c.max_rpm;
    const pos = (v) => clamp(v / max, 0, 1) * 100 + '%';
    this.floorEl.style.width = pos(c.min_rpm);
    if (ch?.value != null) this.liveDot.style.left = pos(ch.value);
    this.liveDot.hidden = ch?.value == null;
    this.tgtDot.hidden = c.target_rpm == null;
    this.tgtDot.classList.toggle('shadow', shadow);
    if (c.target_rpm != null) this.tgtDot.style.left = pos(c.target_rpm);
    this.fill.style.width = pos(c.target_rpm ?? 0);
    this.scale.children[0].textContent = c.min_rpm <= 0 && c.floor_duty <= 0 ? (c.start_duty > 0 ? `darf stehen, läuft ab ${num(c.start_duty, 0)} % Duty` : 'darf stehen') : `Boden ${num(c.min_rpm, 0)} (${num(c.floor_duty, 0)} % Duty)`;
    this.scale.children[1].textContent = `${num(max, 0)} U/min`;
    this.paint();
  }
}

function modelLine(snap) {
  const m = snap.model;
  const bits = [];
  bits.push(!m.loaded ? `Modell nicht geladen${m.error ? ': ' + m.error : ''}` : 'Modell geladen');
  if (m.loaded) bits.push(`Lüfter-Antwort ${m.fan_identified ? 'identifiziert' : 'nicht identifiziert'}`, `Pumpen-Antwort ${m.pump_identified ? 'identifiziert' : 'nicht identifiziert'}`);
  const p = snap.predicted;
  if (p && !p.feasible) bits.push('kein Kandidat hält die Grenzen, Maximum gewählt');
  return bits;
}

const STEP = 0.5;
const TARGET = { pending: null, timer: 0, until: 0 };

function targetShown(snap) {
  return TARGET.pending != null && performance.now() < TARGET.until ? TARGET.pending : snap.target;
}

export function updateTarget(snap) {
  if (snap.target == null) return;
  const t = targetShown(snap);
  const w = value(KEYS.water);
  $('#wt-v').textContent = num(t, 1) + ' °C';
  const now = w == null || isStale(KEYS.water) ? '' : ` · Wasser jetzt ${num(w, 1)} °C`;
  $('#wt-now').textContent = `Bereich ${num(snap.target_min, 0)} bis ${num(snap.target_max, 0)} °C${now}`;
  $('#wt-dn').disabled = t <= snap.target_min + 1e-9;
  $('#wt-up').disabled = t >= snap.target_max - 1e-9;
}

function nudge(dir) {
  const snap = S.snap;
  if (!snap || snap.target == null) return;
  const next = clamp(Math.round((targetShown(snap) + dir * STEP) / STEP) * STEP, snap.target_min, snap.target_max);
  TARGET.pending = next;
  TARGET.until = performance.now() + 2500;
  updateTarget(snap);
  clearTimeout(TARGET.timer);
  TARGET.timer = setTimeout(async () => {
    await command({ cmd: 'set_water_target', celsius: next });
    TARGET.until = performance.now() + 600;
  }, 200);
}

export function initControls() {
  $('#wt-dn').addEventListener('click', () => nudge(-1));
  $('#wt-up').addEventListener('click', () => nudge(1));
}

export function updateControls() {
  const snap = S.snap;
  if (!snap) return;
  updateTarget(snap);
  const sig = snap.controls.map((c) => c.id).join();
  if (sig !== built) {
    built = sig;
    rows.clear();
    const host = $('#ctl-list');
    host.replaceChildren();
    for (const c of snap.controls) {
      const r = new Row(c);
      rows.set(c.id, r);
      host.append(r.el);
    }
  }
  $('#model-line').replaceChildren(...modelLine(snap).map((t, i) => h('span', { class: 'mi' + (/nicht|kein/.test(t) ? ' warn' : '') }, t)));
  for (const c of snap.controls) rows.get(c.id)?.render(c);
}

export const controlRow = (id) => rows.get(id);
