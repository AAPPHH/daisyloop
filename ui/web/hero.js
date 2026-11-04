import { $, Tween, heat, heatRGB, norm, WATER, COMPONENT, clamp, num, drag, frac, motion, fmt } from './ui.js';
import { S, KEYS, value, trend, send, alarmed } from './state.js';
import { editPid, effectivePid } from './controls.js';

const TAU = Math.PI * 2;
const smooth = (a, b, x) => {
  const t = clamp((x - a) / (b - a), 0, 1);
  return t * t * (3 - 2 * t);
};

export class Hero {
  constructor() {
    this.stage = $('#stage');
    this.cv = $('#loop');
    this.ctx = this.cv.getContext('2d');
    this.tw = {
      water: new Tween($('#water-v'), 1),
      cpuT: new Tween($('#cpu-t'), 0),
      cpuP: new Tween($('#cpu-p'), 0),
      gpuT: new Tween($('#gpu-t'), 0),
      gpuP: new Tween($('#gpu-p'), 0),
      pumpRpm: new Tween($('#pump-rpm'), 0),
      pumpDuty: new Tween($('#pump-duty'), 0),
      fanRpm: new Tween($('#fan-rpm'), 0),
      fanDuty: new Tween($('#fan-duty'), 0),
    };
    this.m = { water: 30, cpuP: 0, gpuP: 0, pumpRpm: 0, fanRpm: 0 };
    this.sm = { ...this.m };
    this.phase = 0;
    this.pumpAng = 0;
    this.fanAng = 0;
    this.setpoint = null;
    this.setupSetpoint();
    new ResizeObserver(() => this.layout()).observe(this.stage);
  }

  setupSetpoint() {
    const track = $('#hero-track');
    const knob = $('#hero-knob');
    const move = (ev) => {
      const ctl = S.ctl.get(KEYS.fan);
      if (!ctl) return;
      const v = Math.round((WATER[0] + frac(track, ev) * (WATER[1] - WATER[0])) * 2) / 2;
      this.setpoint = v;
      this.showSetpoint(v);
      editPid(ctl.id, { setpoint: v });
    };
    drag(track, move);
    knob.addEventListener('keydown', (e) => {
      const ctl = S.ctl.get(KEYS.fan);
      if (!ctl || !['ArrowLeft', 'ArrowRight'].includes(e.key)) return;
      const v = clamp((this.setpoint ?? ctl.pid.setpoint) + (e.key === 'ArrowRight' ? 0.5 : -0.5), WATER[0], WATER[1]);
      this.setpoint = v;
      this.showSetpoint(v);
      editPid(ctl.id, { setpoint: v });
    });
  }

  showSetpoint(v) {
    $('#hero-knob').style.left = norm(v, WATER) * 100 + '%';
    $('#hero-set-v').textContent = num(v, 1) + ' °C';
  }

  layout() {
    const r = this.stage.getBoundingClientRect();
    this.dpr = window.devicePixelRatio || 1;
    this.W = r.width;
    this.H = r.height;
    this.cv.width = Math.round(r.width * this.dpr);
    this.cv.height = Math.round(r.height * this.dpr);
    this.buildPath();
    this.draw();
  }

  buildPath() {
    const { W, H } = this;
    const L = 0.1 * W, R = 0.9 * W, T = 0.17 * H, B = 0.74 * H, r = 0.17 * H;
    const midY = 0.45 * H;
    const pts = [];
    const line = (x0, y0, x1, y1) => {
      const n = Math.max(2, Math.round(Math.hypot(x1 - x0, y1 - y0) / 5));
      for (let i = 0; i < n; i++) pts.push([x0 + ((x1 - x0) * i) / n, y0 + ((y1 - y0) * i) / n]);
    };
    const arc = (cx, cy, a0, a1) => {
      const n = 14;
      for (let i = 0; i < n; i++) {
        const a = a0 + ((a1 - a0) * i) / n;
        pts.push([cx + r * Math.cos(a), cy + r * Math.sin(a)]);
      }
    };
    line(L, midY, L, T + r);
    arc(L + r, T + r, Math.PI, Math.PI * 1.5);
    line(L + r, T, R - r, T);
    arc(R - r, T + r, -Math.PI / 2, 0);
    line(R, T + r, R, B - r);
    arc(R - r, B - r, 0, Math.PI / 2);
    line(R - r, B, L + r, B);
    arc(L + r, B - r, Math.PI / 2, Math.PI);
    pts.push([L, midY]);
    const cum = [0];
    for (let i = 1; i < pts.length; i++) cum.push(cum[i - 1] + Math.hypot(pts[i][0] - pts[i - 1][0], pts[i][1] - pts[i - 1][1]));
    this.pts = pts;
    this.cum = cum;
    this.total = cum[cum.length - 1];
    this.path = new Path2D();
    pts.forEach(([x, y], i) => (i ? this.path.lineTo(x, y) : this.path.moveTo(x, y)));
    const at = (x, top) => {
      let best = 0, d = 1e9;
      pts.forEach(([px, py], i) => {
        const e = Math.abs(px - x) + Math.abs(py - (top ? T : B)) * 4;
        if (e < d) { d = e; best = i; }
      });
      return cum[best] / this.total;
    };
    this.u = { cpu: at(0.36 * W, true), gpu: at(0.66 * W, true), rad: at(0.5 * W, false) };
    this.geo = { L, R, T, B, midY };
  }

  tempAt(u, tc, dcpu, dgpu) {
    const w = 0.05, { cpu, gpu, rad } = this.u;
    return tc + dcpu * smooth(cpu - w, cpu + w, u) + dgpu * smooth(gpu - w, gpu + w, u) - (dcpu + dgpu) * smooth(rad - 0.12, rad + 0.1, u);
  }

  update(live) {
    const v = (id) => (live ? value(id) : null);
    const ctl = (id) => S.ctl.get(id);
    const rpm = (c) => (c && live ? value(c.rpm_channel) : null);
    const water = v(KEYS.water), cpuT = v(KEYS.cpuTemp), gpuT = v(KEYS.gpuTemp);
    const set = (k, val) => this.tw[k].set(val);
    set('water', value(KEYS.water));
    set('cpuT', value(KEYS.cpuTemp));
    set('cpuP', value(KEYS.cpuPower));
    set('gpuT', value(KEYS.gpuTemp));
    set('gpuP', value(KEYS.gpuPower));
    const pump = ctl(KEYS.pump), fan = ctl(KEYS.fan);
    set('pumpRpm', pump ? value(pump.rpm_channel) : null);
    set('pumpDuty', pump ? pump.state.output : null);
    set('fanRpm', fan ? value(fan.rpm_channel) : null);
    set('fanDuty', fan ? fan.state.output : null);
    this.m = { water: water ?? this.m.water, cpuP: v(KEYS.cpuPower) ?? this.m.cpuP, gpuP: v(KEYS.gpuPower) ?? this.m.gpuP, pumpRpm: rpm(pump) ?? 0, fanRpm: rpm(fan) ?? 0 };
    const root = document.documentElement.style;
    if (water != null) root.setProperty('--heat', heat(norm(Math.round(water * 2) / 2, WATER)));
    const nodeHeat = (id, cT) => {
      if (cT != null) $(id).style.setProperty('--nodeheat', heat(norm(Math.round(cT / 2) * 2, COMPONENT)));
    };
    nodeHeat('#n-cpu', cpuT);
    nodeHeat('#n-gpu', gpuT);
    const tr = trend(KEYS.water, 60);
    $('#water-trend').innerHTML = !live ? '&nbsp;' : tr == null ? '&nbsp;' : `<b>${tr >= 0 ? '+' : '−'}${num(Math.abs(tr), 1)} K</b> pro Minute`;
    if (water != null) {
      const n = $('#hero-now');
      n.style.left = clamp(norm(water, WATER), 0, 1) * 100 + '%';
      n.dataset.v = 'Ist ' + num(water, 1);
    }
    if (fan) {
      this.setpoint = effectivePid(fan).setpoint;
      this.showSetpoint(this.setpoint);
    }
    const pill = $('#alarm-pill');
    const alarms = S.snap?.alarms ?? [];
    if (live && alarms.length) {
      const a = alarms[0];
      const c = S.ch.get(a.channel);
      pill.className = 'pill bad';
      pill.textContent = `${alarms.length} Alarm${alarms.length > 1 ? 'e' : ''}: ${c?.label ?? a.channel} ${fmt(a.value, c?.unit)} ${a.side === 'high' ? '>' : '<'} ${fmt(a.limit, c?.unit)}`;
    } else {
      pill.className = 'pill ' + (live ? 'ok' : '');
      pill.textContent = live ? 'Alle Werte im Rahmen' : 'Keine aktuellen Werte';
    }
    document.body.classList.toggle('alarm', live && alarms.length > 0);
    if (!this.animating()) this.draw();
  }

  animating() {
    return S.live && S.visible && !motion.reduced;
  }

  tick(dt) {
    const k = 1 - Math.exp(-dt / 0.6);
    for (const key of Object.keys(this.sm)) this.sm[key] += (this.m[key] - this.sm[key]) * k;
    const s = this.W / 900;
    this.phase += (this.sm.pumpRpm / 4800) * 150 * s * dt;
    this.pumpAng += (this.sm.pumpRpm / 4800) * 9 * dt;
    this.fanAng += (this.sm.fanRpm / 1500) * 11 * dt;
    this.draw();
  }

  paintStatic(ctx, live) {
    const { W, H } = this;
    const m = this.sm;
    const dcpu = m.cpuP * 0.011, dgpu = m.gpuP * 0.011;
    const tc = m.water - (dcpu + dgpu) * 0.5;
    const lw = Math.max(8, H * 0.034);
    const { pts, cum, total, geo } = this;
    ctx.clearRect(0, 0, W, H);
    ctx.lineCap = 'round';
    ctx.lineJoin = 'round';
    ctx.lineWidth = lw * 2.4;
    ctx.strokeStyle = live ? heat(norm(m.water, WATER), 0.07) : 'rgba(255,255,255,0.03)';
    ctx.stroke(this.path);
    ctx.lineWidth = lw * 1.45;
    ctx.strokeStyle = '#0d1218';
    ctx.stroke(this.path);
    ctx.lineWidth = lw;
    for (let i = 1; i < pts.length; i++) {
      const t = this.tempAt(cum[i] / total, tc, dcpu, dgpu);
      ctx.strokeStyle = live ? heat(norm(t, WATER)) : '#2a323e';
      ctx.beginPath();
      ctx.moveTo(pts[i - 1][0], pts[i - 1][1]);
      ctx.lineTo(pts[i][0], pts[i][1]);
      ctx.stroke();
    }
    ctx.lineWidth = lw * 0.22;
    ctx.strokeStyle = 'rgba(255,255,255,0.16)';
    ctx.stroke(this.path);
    const glow = (x, y, power, t) => {
      const r = H * (0.14 + 0.16 * clamp(power / 300, 0, 1));
      const g = ctx.createRadialGradient(x, y, 0, x, y, r);
      g.addColorStop(0, heat(norm(t, COMPONENT), live ? 0.32 : 0));
      g.addColorStop(1, 'rgba(0,0,0,0)');
      ctx.fillStyle = g;
      ctx.fillRect(x - r, y - r, r * 2, r * 2);
    };
    glow(0.36 * W, geo.T, m.cpuP, value(KEYS.cpuTemp) ?? 40);
    glow(0.66 * W, geo.T, m.gpuP, value(KEYS.gpuTemp) ?? 40);
  }

  draw() {
    const { ctx, W, H, dpr } = this;
    if (!this.path) return;
    const live = S.live;
    const m = this.sm;
    const key = [W, H, dpr, live, Math.round(m.water * 4), Math.round(m.cpuP / 4), Math.round(m.gpuP / 4), Math.round(value(KEYS.cpuTemp) ?? 0), Math.round(value(KEYS.gpuTemp) ?? 0)].join();
    if (!this.cache || this.cache.key !== key) {
      if (!this.cache || this.cache.canvas.width !== this.cv.width || this.cache.canvas.height !== this.cv.height) {
        const c = document.createElement('canvas');
        c.width = this.cv.width;
        c.height = this.cv.height;
        this.cache = { canvas: c };
      }
      this.cache.key = key;
      const cctx = this.cache.canvas.getContext('2d');
      cctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      this.paintStatic(cctx, live);
    }
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, this.cv.width, this.cv.height);
    ctx.drawImage(this.cache.canvas, 0, 0);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    if (live) {
      const lw = Math.max(8, H * 0.034);
      ctx.save();
      ctx.lineCap = 'round';
      ctx.lineWidth = lw * 0.34;
      ctx.strokeStyle = 'rgba(255,255,255,0.8)';
      ctx.setLineDash([lw * 0.9, lw * 3.4]);
      ctx.lineDashOffset = -this.phase;
      ctx.stroke(this.path);
      ctx.restore();
    }
    this.drawPump(this.geo.L, this.geo.midY, H * 0.07, live);
    this.drawRadiator(0.5 * W, this.geo.B, W * 0.34, H * 0.085, live);
  }

  drawPump(x, y, r, live) {
    const { ctx } = this;
    ctx.save();
    ctx.translate(x, y);
    ctx.fillStyle = '#0b0f14';
    ctx.strokeStyle = live ? heat(norm(this.sm.water, WATER), 0.9) : '#2a323e';
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.arc(0, 0, r, 0, TAU);
    ctx.fill();
    ctx.stroke();
    ctx.rotate(this.pumpAng);
    ctx.fillStyle = live ? 'rgba(255,255,255,0.82)' : '#3a4350';
    for (let i = 0; i < 5; i++) {
      ctx.rotate(TAU / 5);
      ctx.beginPath();
      ctx.moveTo(0, 0);
      ctx.quadraticCurveTo(r * 0.55, -r * 0.1, r * 0.78, r * 0.42);
      ctx.quadraticCurveTo(r * 0.3, r * 0.3, 0, 0);
      ctx.fill();
    }
    ctx.restore();
  }

  drawRadiator(cx, cy, w, h, live) {
    const { ctx, H } = this;
    ctx.save();
    ctx.translate(cx, cy);
    ctx.fillStyle = '#10161d';
    ctx.strokeStyle = 'rgba(255,255,255,0.1)';
    ctx.lineWidth = 1;
    ctx.beginPath();
    ctx.roundRect(-w / 2, -h / 2, w, h, h * 0.3);
    ctx.fill();
    ctx.stroke();
    ctx.strokeStyle = 'rgba(255,255,255,0.07)';
    for (let x = -w / 2 + h * 0.5; x < w / 2 - h * 0.4; x += h * 0.22) {
      ctx.beginPath();
      ctx.moveTo(x, -h / 2 + 3);
      ctx.lineTo(x, h / 2 - 3);
      ctx.stroke();
    }
    const fr = H * 0.085;
    for (const dx of [-w * 0.26, w * 0.26]) {
      ctx.save();
      ctx.translate(dx, h * 0.5 + fr * 1.25);
      ctx.strokeStyle = 'rgba(255,255,255,0.12)';
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      ctx.roundRect(-fr * 1.1, -fr * 1.1, fr * 2.2, fr * 2.2, fr * 0.35);
      ctx.stroke();
      ctx.rotate(this.fanAng);
      ctx.fillStyle = live ? 'rgba(200,225,255,0.55)' : '#2c3440';
      for (let i = 0; i < 7; i++) {
        ctx.rotate(TAU / 7);
        ctx.beginPath();
        ctx.moveTo(fr * 0.18, 0);
        ctx.bezierCurveTo(fr * 0.4, -fr * 0.55, fr * 0.95, -fr * 0.5, fr * 0.98, -fr * 0.08);
        ctx.bezierCurveTo(fr * 0.7, -fr * 0.2, fr * 0.4, -fr * 0.12, fr * 0.18, 0);
        ctx.fill();
      }
      ctx.fillStyle = live ? '#e7eef9' : '#3b4452';
      ctx.beginPath();
      ctx.arc(0, 0, fr * 0.17, 0, TAU);
      ctx.fill();
      ctx.restore();
    }
    ctx.restore();
  }
}
