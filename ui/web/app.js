import { $, h, motion, stepTweens, ageText } from './ui.js';
import { S, ingest, age, STALE_AFTER } from './state.js';
import { Hero } from './hero.js';
import { Heat } from './heat.js';
import { updateControls, initControls, toast } from './controls.js';
import { History } from './history.js';
import { Sensors } from './sensors.js';

const tauri = window.__TAURI__ ?? (await import('./dev/bridge.js')).default;
S.tauri = tauri;

const hero = new Hero();
const heat = new Heat();
const history = new History();
const sensors = new Sensors(history);
let raf = 0;
let last = 0;
let heroDt = 0;
const HERO_FPS = 20;

function frame(t) {
  raf = 0;
  const dt = Math.min(0.1, (t - last) / 1000 || 0.016);
  last = t;
  const tweening = stepTweens(dt);
  const animate = hero.animating();
  heroDt += dt;
  if (animate && heroDt >= 1 / HERO_FPS) {
    hero.tick(heroDt);
    heroDt = 0;
  }
  else if (tweening) hero.draw();
  if (animate || tweening) raf = requestAnimationFrame(frame);
}

motion.kick = () => {
  if (!raf && S.visible) {
    last = performance.now();
    raf = requestAnimationFrame(frame);
  }
};

function setLive(live) {
  const was = S.live;
  S.live = live;
  heat.update(live);
  document.body.classList.toggle('stale', !live);
  const st = $('#status');
  st.className = 'status ' + (live ? 'live' : 'down');
  const a = age();
  st.lastElementChild.textContent = live ? 'Live' : S.snap ? `Daemon nicht erreichbar · letzte Werte vor ${ageText(a)}` : 'Warte auf Daemon …';
  if (was !== live) {
    hero.update(live);
    motion.kick();
  }
}

function alarmBar() {
  const bar = $('#alarm-bar');
  const alarms = S.snap?.alarms ?? [];
  bar.hidden = !alarms.length;
  bar.replaceChildren(...alarms.map((a) => h('div', { class: 'al ' + a.kind }, h('b', {}, a.message), h('span', {}, ageText((Date.now() - a.since_ms) / 1000)))));
}

function render() {
  if (!S.visible) return;
  setLive(true);
  hero.update(true);
  alarmBar();
  updateControls();
  sensors.render();
  history.chips();
  history.fetch();
  motion.kick();
}

function setVisible(v) {
  S.visible = v;
  motion.paused = !v;
  document.body.classList.toggle('paused', !v);
  if (v) {
    S.snap && render();
    motion.kick();
  } else {
    cancelAnimationFrame(raf);
    raf = 0;
  }
}

tauri.event.listen('snapshot', (e) => {
  ingest(e.payload);
  render();
});
tauri.event.listen('history', (e) => history.onHistory(e.payload));
tauri.event.listen('link-down', (e) => {
  S.err = e.payload;
  setLive(false);
});
tauri.event.listen('link-error', (e) => ((S.err = e.payload), toast(String(e.payload))));
tauri.event.listen('visibility', (e) => setVisible(e.payload));
document.addEventListener('visibilitychange', () => setVisible(!document.hidden));
initControls();

const links = [...document.querySelectorAll('.index a')];
const spy = new IntersectionObserver((entries) => {
  for (const e of entries) if (e.isIntersecting) links.forEach((a) => a.classList.toggle('on', a.getAttribute('href') === '#' + e.target.id));
}, { rootMargin: '-45% 0px -50% 0px' });
links.forEach((a) => spy.observe(document.querySelector(a.getAttribute('href'))));

setInterval(() => {
  if (!S.visible) return;
  const a = age();
  if (S.snap && a > STALE_AFTER) setLive(false);
  else if (!S.live && S.snap && a <= STALE_AFTER) setLive(true);
}, 1000);

const first = await tauri.core.invoke('last_snapshot');
if (first) {
  ingest(first);
  history.chips();
  render();
} else setLive(false);
