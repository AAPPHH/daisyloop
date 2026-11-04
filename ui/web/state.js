export const KEYS = {
  water: '/usbhid/0C70/F00E/00000-00000/temperature/0',
  cpuTemp: '/intelcpu/0/temperature/26',
  cpuPower: '/intelcpu/0/power/0',
  gpuTemp: '/gpu-nvidia/0/temperature/0',
  gpuPower: '/gpu-nvidia/0/power/0',
  pump: 'd5.pump',
  fan: 'd5.fan',
};

export const TRAIL = 600;
export const STALE_AFTER = 3.5;

export const S = {
  snap: null,
  ch: new Map(),
  ctl: new Map(),
  trail: new Map(),
  live: false,
  lastAt: 0,
  err: null,
  visible: true,
  selected: [KEYS.water, KEYS.cpuTemp, KEYS.gpuTemp, KEYS.cpuPower, KEYS.gpuPower, '/usbhid/0C70/F00E/00000-00000/fan/0', '/usbhid/0C70/F00E/00000-00000/fan/1'],
  hidden: new Set(),
  range: '1h',
  listeners: {},
  tauri: null,
};

export const on = (ev, fn) => (S.listeners[ev] ||= []).push(fn);
export const emit = (ev, ...a) => (S.listeners[ev] || []).forEach((f) => f(...a));
export const age = () => (S.lastAt ? (performance.now() - S.lastAt) / 1000 : Infinity);
export const send = (cmd) => S.tauri.core.invoke('send_command', { cmd });
export const requestHistory = (tag, query) => S.tauri.core.invoke('request_history', { tag, query });
export const channel = (id) => S.ch.get(id);
export const value = (id) => S.ch.get(id)?.value ?? null;
export const alarmed = (id) => !!S.snap?.alarms.some((a) => a.channel === id);

export function ingest(snap) {
  S.snap = snap;
  S.lastAt = performance.now();
  S.err = null;
  S.ch.clear();
  for (const c of snap.channels) {
    S.ch.set(c.id, c);
    if (c.value != null) {
      let t = S.trail.get(c.id);
      if (!t) S.trail.set(c.id, (t = []));
      t.push(c.value);
      if (t.length > TRAIL) t.shift();
    }
  }
  S.ctl.clear();
  for (const c of snap.controls) S.ctl.set(c.id, c);
}

export function trend(id, seconds = 60) {
  const t = S.trail.get(id);
  if (!t || t.length < 5) return null;
  const n = Math.min(seconds, t.length - 1);
  return ((t[t.length - 1] - t[t.length - 1 - n]) / n) * 60;
}
