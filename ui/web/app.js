import { $, h, motion, stepTweens, ageText } from './ui.js';
import { S, ingest, age, STALE_AFTER } from './state.js';
import { Hero } from './hero.js';
import { updateControls, initControls, toast } from './controls.js';
import { History } from './history.js';
import { Sensors } from './sensors.js';
import { Gaming } from './gaming.js';

const tauri = window.__TAURI__ ?? (await import('./dev/bridge.js')).default;
S.tauri = tauri;

const hero = new Hero();
const history = new History();
const sensors = new Sensors(history);
const gaming = new Gaming();
const vis = new Set();
let alarmSig = '';
let alarmAges = [];
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
  if (!raf && S.visible && !(vis.size === 1 && vis.has('history'))) {
    last = performance.now();
    raf = requestAnimationFrame(frame);
  }
};

function setLive(live) {
  const was = S.live;
  S.live = live;
  document.body.classList.toggle('stale', !live);
  const st = $('#status');
  st.className = 'status ' + (live ? 'live' : 'down');
  const a = age();
  st.lastElementChild.textContent = live ? 'Live' : S.snap ? `Daemon nicht erreichbar · letzte Werte vor ${ageText(a)}` : 'Warte auf Daemon …';
  if (was !== live) {
    if (vis.has('hero')) hero.update(live);
    motion.kick();
  }
}

function alarmAge() {
  for (const [el, a] of alarmAges) el.textContent = ageText((Date.now() - a.since_ms) / 1000);
}

function alarmBar() {
  const bar = $('#alarm-bar');
  const alarms = S.snap?.alarms ?? [];
  const sig = alarms.map((a) => a.kind + a.channel + a.message).join('|');
  if (sig !== alarmSig) {
    alarmSig = sig;
    bar.hidden = !alarms.length;
    alarmAges = alarms.map((a) => [h('span'), a]);
    bar.replaceChildren(...alarms.map((a, i) => h('div', { class: 'al ' + a.kind }, h('b', {}, a.message), alarmAges[i][0])));
  }
  alarmAge();
}

new ResizeObserver(() => document.documentElement.style.setProperty('--alarm-h', ($('#alarm-bar').hidden ? 0 : $('#alarm-bar').getBoundingClientRect().height) + 'px')).observe($('#alarm-bar'));

function render() {
  if (!S.visible) return;
  setLive(true);
  gaming.ingest();
  alarmBar();
  paintPage();
}

function paintSection(id) {
  if (id === 'hero') hero.update(S.live);
  else if (id === 'history') {
    gaming.paint();
    if (history.open) history.fetch();
  } else if (id === 'controls') updateControls();
  else if (id === 'sensors') sensors.render();
}

let primed = false;

function paintPage() {
  if (!primed) {
    primed = true;
    ['hero', 'history', 'controls', 'sensors'].forEach((id) => vis.has(id) || paintSection(id));
  }
  vis.forEach(paintSection);
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
const sections = links.map((a) => document.querySelector(a.getAttribute('href')));
let pin = null;
let located = 0;

function highlight(id) {
  links.forEach((a) => a.classList.toggle('on', a.getAttribute('href') === '#' + id));
}

function locate() {
  located = 0;
  if (pin) return highlight(pin);
  const atEnd = scrollY + innerHeight >= document.documentElement.scrollHeight - 2;
  let id = sections[0].id;
  for (const s of sections) if (atEnd || s.getBoundingClientRect().top <= innerHeight * 0.45) id = s.id;
  highlight(id);
}

const queueLocate = () => located || (located = requestAnimationFrame(locate));
const unpin = () => ((pin = null), queueLocate());
addEventListener('scroll', queueLocate, { passive: true });
addEventListener('resize', queueLocate);
addEventListener('wheel', unpin, { passive: true });
addEventListener('touchstart', unpin, { passive: true });
addEventListener('keydown', unpin);
addEventListener('pointerdown', (e) => e.target === document.documentElement && unpin());
document.querySelector('.cue').addEventListener('click', unpin);
links.forEach((a) => a.addEventListener('click', () => ((pin = a.getAttribute('href').slice(1)), highlight(pin))));

const spy = new IntersectionObserver((entries) => {
  const fresh = [];
  for (const e of entries) {
    if (e.isIntersecting) {
      if (!vis.has(e.target.id)) fresh.push(e.target.id);
      vis.add(e.target.id);
    } else vis.delete(e.target.id);
  }
  hero.inView = vis.has('hero');
  if (S.snap && S.visible) fresh.forEach(paintSection);
  motion.kick();
  queueLocate();
}, { rootMargin: '-2px 0px' });
sections.forEach((s) => spy.observe(s));

setInterval(() => {
  if (!S.visible) return;
  alarmAge();
  const a = age();
  if (S.snap && a > STALE_AFTER) setLive(false);
  else if (!S.live && S.snap && a <= STALE_AFTER) setLive(true);
}, 1000);

const first = await tauri.core.invoke('last_snapshot');
if (first) {
  ingest(first);
  render();
  const target = location.hash && document.querySelector(location.hash);
  if (target) target.scrollIntoView({ behavior: 'instant' });
} else setLive(false);
