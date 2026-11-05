const base = await (await fetch(new URL('./snapshot.json', import.meta.url))).json();
const q = new URLSearchParams(location.search);
const listeners = {};
const state = JSON.parse(JSON.stringify(base));
const stale = new Set((q.get('stale') || '').split(',').filter(Boolean));
let seq = 0;
let down = false;
let test = null;
state.controls.forEach((c) => {
  c.allowed = q.get('writes') === '1';
  c.identified = q.get('identified') === '1';
  if (q.get('mode')) c.mode = q.get('mode');
  if (c.mode === 'active') Object.assign(c, { drive: 'duty', commanded_duty: c.duty, note: null });
});
if (q.get('fault')) {
  Object.assign(state.controls[0], { mode: 'active', drive: 'firmware', fallback: q.get('fault') });
}
const still = q.get('still') === '1';
const water = parseFloat(q.get('water'));
const wave = (i, t, p) => Math.sin(t / p + i * 1.7);

function snapshot() {
  const t = Date.now() / 1000;
  const s = JSON.parse(JSON.stringify(state));
  s.ts_ms = Date.now();
  s.seq = ++seq;
  s.channels.forEach((c, i) => {
    const b = base.channels[i].value;
    if (b != null) c.value = b * (1 + (c.kind === 'temperature' || c.kind === 'power' || c.kind === 'fan' ? 0.04 : 0.01) * (0.6 * wave(i, t, 9) + 0.4 * wave(i, t, 41)));
    if (still && /^d5next\/fan\/(rpm|duty|power|current)$/.test(c.id)) c.value = 0;
    if (c.id === 'd5next/water/temp' && isFinite(water)) c.value = water;
    if (stale.has(c.id)) Object.assign(c, { stale: true, age_ms: 42000, value: b });
  });
  s.alarms = [];
  for (const l of s.limits) {
    const c = s.channels.find((x) => x.id === l.channel);
    if (c.stale) s.alarms.push({ kind: 'stale', channel: c.id, message: `${c.label}: keine Daten seit 42 s`, value: null, limit: null, since_ms: Date.now() - 42000 });
    else if (l.high != null && c.value > l.high) s.alarms.push({ kind: 'water_high', channel: c.id, message: `${c.label} ${c.value.toFixed(1)} über Grenze ${l.high}`, value: c.value, limit: l.high, since_ms: Date.now() - 20000 });
  }
  if (still) s.controls[0].target_rpm = 0;
  state.controls.forEach((c, i) => {
    s.controls[i].actual_rpm = s.channels.find((x) => x.id === c.rpm_channel).value;
    s.controls[i].duty = s.channels.find((x) => x.id === c.duty_channel).value;
  });
  if (q.get('fault')) s.alarms.push({ kind: 'fallback', channel: state.controls[0].id, message: `${state.controls[0].label}: Rückfall auf Firmware-PID (${q.get('fault')})`, value: null, limit: null, since_ms: Date.now() - 90000 });
  if (test) s.step_test = { id: test, phase: 2, phases: 5, duty: 80, left_s: 412 };
  return s;
}

const emit = (name, payload) => (listeners[name] || []).forEach((cb) => cb({ payload }));
setInterval(() => !down && emit('snapshot', snapshot()), 1000);

function history(qy) {
  const s = snapshot();
  const n = Math.min(qy.max_points, 500);
  return {
    series: qy.channels.map((id) => {
      const idx = s.channels.findIndex((c) => c.id === id);
      const ts_ms = [], values = [];
      for (let k = 0; k <= n; k++) {
        const t = qy.from_ms + ((qy.to_ms - qy.from_ms) * k) / n;
        const hr = t / 3600000;
        ts_ms.push(t);
        values.push(base.channels[idx].value * (1 + 0.08 * Math.sin(hr * 1.3 + idx) + 0.05 * Math.sin(hr * 11 + idx * 3) + 0.01 * Math.sin(hr * 140)));
      }
      return { channel: id, ts_ms, values };
    }),
  };
}

function command(c) {
  const ctl = state.controls.find((x) => x.id === c.id);
  if (c.cmd === 'set_weight') state.weight = c.weight;
  if (c.cmd === 'start_step_test') test = c.id;
  if (c.cmd === 'stop_step_test') test = null;
  if (c.cmd !== 'set_mode') return;
  if (c.mode === 'active') {
    if (!ctl.allowed) throw new Error('Aktiv abgelehnt: writes_enabled ist in der Admin-Konfiguration aus.');
    if (!ctl.identified) throw new Error('Aktiv abgelehnt: das Modell kennt die Antwort dieses Kanals nicht (nicht identifiziert).');
    if (ctl.kind === 'pump' && !c.confirmed) throw new Error('Pumpe braucht eine Bestätigung.');
  }
  ctl.mode = c.mode;
  ctl.fallback = null;
  ctl.drive = c.mode === 'active' ? 'duty' : 'firmware';
  ctl.commanded_duty = c.mode === 'active' ? ctl.duty : null;
  ctl.target_rpm = c.mode === 'off' ? null : base.controls.find((x) => x.id === c.id).target_rpm;
  ctl.note = c.mode === 'shadow' ? 'nur berechnet' : null;
}

export default {
  event: { listen: (name, cb) => ((listeners[name] ||= []).push(cb), Promise.resolve(() => {})) },
  core: {
    invoke: async (cmd, args) => {
      if (cmd === 'last_snapshot') return snapshot();
      if (cmd === 'request_history') setTimeout(() => emit('history', { tag: args.tag, response: history(args.query) }), 150);
      if (cmd === 'send_command') {
        window.__sent = [...(window.__sent || []), args.cmd];
        command(args.cmd);
      }
    },
  },
};

window.__dev = {
  down: () => ((down = true), emit('link-down', 'Daemon nicht erreichbar (Demo)')),
  up: () => (down = false),
  emit,
};
