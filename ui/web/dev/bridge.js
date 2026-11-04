const base = await (await fetch(new URL('./snapshot.json', import.meta.url))).json();
const listeners = {};
let seq = 0;
let down = false;
const state = JSON.parse(JSON.stringify(base));
const wave = (i, t, p) => Math.sin(t / p + i * 1.7);

function snapshot() {
  const t = Date.now() / 1000;
  const s = JSON.parse(JSON.stringify(state));
  s.ts_ms = Date.now();
  s.seq = ++seq;
  s.channels.forEach((c, i) => {
    const b = base.channels[i].value;
    if (b == null) return;
    const w = c.kind === 'temperature' || c.kind === 'power' || c.kind === 'fan' ? 0.04 : 0.01;
    c.value = b * (1 + w * (0.6 * wave(i, t, 9) + 0.4 * wave(i, t, 41)));
  });
  s.alarms = [];
  for (const l of s.limits) {
    const c = s.channels.find((x) => x.id === l.channel);
    if (l.high != null && c.value > l.high) s.alarms.push({ channel: c.id, side: 'high', limit: l.high, value: c.value, since_ms: Date.now() - 20000 });
    if (l.low != null && c.value < l.low) s.alarms.push({ channel: c.id, side: 'low', limit: l.low, value: c.value, since_ms: Date.now() - 20000 });
  }
  return s;
}

function emit(name, payload) {
  (listeners[name] || []).forEach((cb) => cb({ payload }));
}

setInterval(() => !down && emit('snapshot', snapshot()), 1000);

function history(q) {
  const s = snapshot();
  const n = Math.min(q.max_points, 500);
  return {
    series: q.channels.map((id) => {
      const idx = s.channels.findIndex((c) => c.id === id);
      const c = s.channels[idx];
      const ts_ms = [], values = [];
      for (let k = 0; k <= n; k++) {
        const t = q.from_ms + ((q.to_ms - q.from_ms) * k) / n;
        const hr = t / 3600000;
        ts_ms.push(t);
        values.push(base.channels[idx].value * (1 + 0.08 * Math.sin(hr * 1.3 + idx) + 0.05 * Math.sin(hr * 11 + idx * 3) + 0.01 * Math.sin(hr * 140)));
      }
      return { channel: id, ts_ms, values };
    }),
  };
}

export default {
  event: { listen: (name, cb) => ((listeners[name] ||= []).push(cb), Promise.resolve(() => {})) },
  core: {
    invoke: async (cmd, args) => {
      if (cmd === 'last_snapshot') return snapshot();
      if (cmd === 'request_history') setTimeout(() => emit('history', { tag: args.tag, response: history(args.query) }), 150);
      if (cmd === 'send_command') {
        const c = args.cmd;
        if (c.cmd === 'set_pid') state.controls.find((x) => x.id === c.id).pid = c.pid;
        if (c.cmd === 'set_limit') {
          state.limits = state.limits.filter((l) => l.channel !== c.channel);
          if (c.low != null || c.high != null) state.limits.push({ channel: c.channel, low: c.low, high: c.high });
        }
        window.__sent = [...(window.__sent || []), c];
      }
    },
  },
};

window.__dev = {
  down: () => ((down = true), emit('link-down', 'Daemon nicht erreichbar (Demo)')),
  up: () => (down = false),
  emit,
};
