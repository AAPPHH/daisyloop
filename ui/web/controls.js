import { $, h, Tween, heat, norm, WATER, COMPONENT, clamp, num, drag, frac, scrub } from './ui.js';
import { S, KEYS, send, value, alarmed } from './state.js';

const edits = new Map();
let timer = null;

export const domainFor = (source) => (source === KEYS.water ? WATER : COMPONENT);

export function effectivePid(c) {
  return edits.get(c.id)?.pid ?? c.pid;
}

export function isEditing(id) {
  return edits.has(id);
}

export function editPid(id, patch) {
  const c = S.ctl.get(id);
  if (!c) return;
  const pid = { ...effectivePid(c), ...patch };
  pid.min_duty = clamp(pid.min_duty, c.floor, 100);
  pid.max_duty = clamp(pid.max_duty, pid.min_duty, 100);
  edits.set(id, { pid, dirty: performance.now(), sent: false });
  clearTimeout(timer);
  timer = setTimeout(flush, 260);
  const row = rows.get(id);
  row && row.render(c, true);
}

function flush() {
  const now = performance.now();
  for (const [id, e] of edits) {
    if (!e.sent && now - e.dirty >= 250) {
      e.sent = true;
      e.sentAt = now;
      send({ cmd: 'set_pid', id, pid: e.pid }).catch(() => edits.delete(id));
    }
  }
}

function settle() {
  const now = performance.now();
  for (const [id, e] of edits) {
    const c = S.ctl.get(id);
    if (e.sent && (now - e.sentAt > 3000 || (c && JSON.stringify(c.pid) === JSON.stringify(e.pid)))) edits.delete(id);
  }
}

const rows = new Map();
let built = '';

class Row {
  constructor(c, temps) {
    this.id = c.id;
    this.floor = c.floor;
    this.el = h('article', { class: 'ch' });
    this.rpm = h('span', { class: 'rpm' });
    this.duty = h('b');
    this.state = h('span', { class: 'state' });
    this.rpmT = new Tween(this.rpm, 0);
    this.dutyT = new Tween(this.duty, 0);
    this.select = h('select', { onchange: () => editPid(this.id, { source: this.select.value }) });
    for (const t of temps) this.select.append(h('option', { value: t.id }, `${t.hardware} · ${t.label}`));
    this.setNow = h('b', { class: 'now' });
    this.setKnob = h('b', { class: 'knob', tabindex: '0' });
    this.setTag = h('span', { class: 'tag' });
    this.setTrack = h('div', { class: 'track heat-track' }, this.setNow, this.setTag, this.setKnob);
    drag(this.setTrack, (ev) => {
      const d = domainFor(effectivePid(S.ctl.get(this.id)).source);
      editPid(this.id, { setpoint: Math.round((d[0] + frac(this.setTrack, ev) * (d[1] - d[0])) * 2) / 2 });
    });
    this.floorEl = h('b', { class: 'floor' });
    this.fill = h('b', { class: 'fill' });
    this.liveDot = h('b', { class: 'live' });
    this.minKnob = h('b', { class: 'knob sm', tabindex: '0' });
    this.maxKnob = h('b', { class: 'knob sm', tabindex: '0' });
    this.outTrack = h('div', { class: 'track out' }, this.floorEl, this.fill, this.liveDot, this.minKnob, this.maxKnob);
    const outDrag = (which) => (ev) => {
      const p = effectivePid(S.ctl.get(this.id));
      const v = Math.round(frac(this.outTrack, ev) * 100);
      editPid(this.id, which === 'min' ? { min_duty: Math.min(v, p.max_duty - 1) } : { max_duty: Math.max(v, p.min_duty + 1) });
    };
    drag(this.minKnob, outDrag('min'));
    drag(this.maxKnob, outDrag('max'));
    this.nums = {
      kp: scrub({ value: c.pid.kp, step: 0.05, min: 0, max: 100, dec: 2, onInput: (v) => editPid(this.id, { kp: v }) }),
      ki: scrub({ value: c.pid.ki, step: 0.005, min: 0, max: 10, dec: 3, onInput: (v) => editPid(this.id, { ki: v }) }),
      kd: scrub({ value: c.pid.kd, step: 0.05, min: 0, max: 200, dec: 2, onInput: (v) => editPid(this.id, { kd: v }) }),
      min: scrub({ value: c.pid.min_duty, step: 1, min: c.floor, max: 100, dec: 0, suffix: ' %', onInput: (v) => editPid(this.id, { min_duty: v }) }),
      max: scrub({ value: c.pid.max_duty, step: 1, min: c.floor, max: 100, dec: 0, suffix: ' %', onInput: (v) => editPid(this.id, { max_duty: v }) }),
    };
    this.readout = h('span', { class: 'readout' });
    const lab = (t, el) => h('label', {}, t, el);
    this.el.append(
      h('div', { class: 'who' }, h('h3', {}, c.label), h('div', { class: 'reading' }, this.rpm, h('small', {}, 'U/min'), h('span', { class: 'duty' }, this.duty, ' %')), this.state),
      h('div', { class: 'ctl' },
        h('span', { class: 'k' }, 'Quelle'), this.select,
        h('span', { class: 'k' }, 'Sollwert'), this.setTrack,
        h('span', { class: 'k' }, c.kind === 'pump' ? 'Ausgang · Boden' : 'Ausgang'), this.outTrack,
        h('span', { class: 'k' }, 'Regler'),
        h('div', { class: 'pid' }, lab('P', this.nums.kp.el), lab('I', this.nums.ki.el), lab('D', this.nums.kd.el), lab('Min', this.nums.min.el), lab('Max', this.nums.max.el), this.readout),
      ),
    );
  }

  render(c, local = false) {
    const p = effectivePid(c);
    const d = domainFor(p.source);
    if (this.select.value !== p.source) this.select.value = p.source;
    const sp = norm(p.setpoint, d) * 100;
    this.setKnob.style.left = clamp(sp, 0, 100) + '%';
    this.setTag.style.left = clamp(sp, 4, 96) + '%';
    this.setTag.textContent = num(p.setpoint, 1) + ' °C';
    this.setTag.style.color = heat(norm(p.setpoint, d));
    const src = value(p.source);
    if (src != null) {
      this.setNow.style.left = clamp(norm(src, d), 0, 1) * 100 + '%';
      this.setNow.dataset.v = 'Ist ' + num(src, 1);
    }
    this.floorEl.style.width = c.floor + '%';
    this.fill.style.left = p.min_duty + '%';
    this.fill.style.width = p.max_duty - p.min_duty + '%';
    this.minKnob.style.left = p.min_duty + '%';
    this.maxKnob.style.left = p.max_duty + '%';
    this.nums.kp.set(p.kp);
    this.nums.ki.set(p.ki);
    this.nums.kd.set(p.kd);
    this.nums.min.set(p.min_duty);
    this.nums.max.set(p.max_duty);
    this.nums.min.bounds(c.floor, p.max_duty - 1);
    this.nums.max.bounds(p.min_duty + 1, 100);
    if (local) return;
    const st = c.state;
    this.liveDot.style.left = st.output + '%';
    this.rpmT.set(value(c.rpm_channel));
    this.dutyT.set(st.output);
    let txt = 'Regelt', cls = 'state';
    if (c.mode !== 'pid') (txt = 'Modus nicht aktiv'), (cls = 'state off');
    else if (st.saturated && Math.abs(st.output - p.max_duty) < 0.05) (txt = 'Begrenzt: Max'), (cls = 'state warn');
    else if (st.saturated || Math.abs(st.output - p.min_duty) < 0.05) (txt = c.kind === 'pump' && p.min_duty <= c.floor + 0.05 ? 'Begrenzt: Boden' : 'Begrenzt: Min'), (cls = 'state warn');
    this.state.className = cls;
    this.state.textContent = txt;
    this.readout.innerHTML = `Fehler <b>${st.error >= 0 ? '+' : '−'}${num(Math.abs(st.error), 1)} K</b> · P <b>${num(st.p, 1)}</b> · I <b>${num(st.i, 1)}</b> · D <b>${num(st.d, 1)}</b>`;
  }
}

export function updateControls() {
  settle();
  const snap = S.snap;
  if (!snap) return;
  const sig = snap.controls.map((c) => c.id).join();
  if (sig !== built) {
    built = sig;
    rows.clear();
    const host = $('#ctl-list');
    host.replaceChildren();
    const temps = snap.channels.filter((c) => c.kind === 'temperature');
    const board = h('details', { class: 'board' });
    const summary = h('summary');
    board.append(summary);
    for (const c of snap.controls) {
      const r = new Row(c, temps);
      rows.set(c.id, r);
      (c.id.startsWith('d5.') ? host : board).append(r.el);
    }
    board.summary = summary;
    host.append(board);
    rows.board = board;
    board.open = innerWidth > 2200;
  }
  for (const c of snap.controls) rows.get(c.id)?.render(c);
  const boardCtl = snap.controls.filter((c) => !c.id.startsWith('d5.'));
  const tach = boardCtl.filter((c) => (value(c.rpm_channel) ?? 0) > 0);
  rows.board.summary.innerHTML = `<b>Mainboard-Anschlüsse</b> ${boardCtl.length} Kanäle` + (tach.length ? ' · ' + tach.map((c) => `${c.label} ${num(value(c.rpm_channel), 0)} U/min · ${num(c.state.output, 0)} %`).join(' · ') : '');
}
