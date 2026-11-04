use serde::Deserialize;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

#[derive(Deserialize)]
struct RawNode {
    name: String,
    op: String,
    #[serde(rename = "in")]
    ins: Vec<String>,
    p: Value,
    out: bool,
}

#[derive(Deserialize)]
struct RawSpec {
    nodes: Vec<RawNode>,
}

#[derive(Clone)]
enum Op {
    Raw { ffill: i64, zero_nan: bool },
    Lin { coef: Vec<f64>, off: f64 },
    Scale(f64),
    Mul,
    Pow(f64),
    GMax,
    GMin,
    GMean,
    GSpread,
    Ewm { alpha: f64, warm: i64 },
    Diff(usize),
    Gt { c: f64, inv: bool },
    Mask,
    LeakyMin(f64),
    Tsc { thr: f64, n: usize, cap: i64 },
    HourSin,
    HourCos,
}

#[derive(Clone, Default)]
struct State {
    last: Option<f64>,
    age: i64,
    n: i64,
    y: f64,
    c: i64,
    init: bool,
}

#[derive(Clone)]
struct Node {
    op: Op,
    ins: Vec<usize>,
    raw_col: usize,
}

#[derive(Clone)]
pub struct Engine {
    nodes: Vec<Node>,
    state: Vec<State>,
    hist: Vec<VecDeque<f64>>,
    cap: Vec<usize>,
    vals: Vec<f64>,
    pub raw_channels: Vec<String>,
    pub outputs: Vec<String>,
    out_idx: Vec<usize>,
}

fn pf(v: &Value, k: &str) -> f64 {
    v[k].as_f64().unwrap()
}

impl Engine {
    pub fn from_json(text: &str) -> Engine {
        let spec: RawSpec = serde_json::from_str(text).unwrap();
        let index: HashMap<String, usize> = spec.nodes.iter().enumerate().map(|(i, n)| (n.name.clone(), i)).collect();
        let mut nodes = Vec::new();
        let mut raw_channels = Vec::new();
        let mut cap = vec![0usize; spec.nodes.len()];
        for n in &spec.nodes {
            let ins: Vec<usize> = n.ins.iter().map(|s| index[s]).collect();
            let mut raw_col = usize::MAX;
            let op = match n.op.as_str() {
                "raw" => {
                    raw_col = raw_channels.len();
                    raw_channels.push(n.p["channel"].as_str().unwrap().to_string());
                    Op::Raw { ffill: n.p["ffill"].as_i64().unwrap(), zero_nan: n.p["zero_nan"].as_bool().unwrap() }
                }
                "lin" => Op::Lin { coef: n.p["coef"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect(), off: pf(&n.p, "off") },
                "scale" => Op::Scale(pf(&n.p, "c")),
                "mul" => Op::Mul,
                "pow" => Op::Pow(pf(&n.p, "e")),
                "gmax" => Op::GMax,
                "gmin" => Op::GMin,
                "gmean" => Op::GMean,
                "gspread" => Op::GSpread,
                "ewm" => {
                    let tau = pf(&n.p, "tau");
                    Op::Ewm { alpha: 1.0 - (-1.0 / tau).exp(), warm: std::cmp::max(1, tau as i64) }
                }
                "diff" => {
                    let k = n.p["n"].as_u64().unwrap() as usize;
                    cap[ins[0]] = cap[ins[0]].max(k + 1);
                    Op::Diff(k)
                }
                "gt" => Op::Gt { c: pf(&n.p, "c"), inv: n.p.get("inv").and_then(|x| x.as_bool()).unwrap_or(false) },
                "mask" => Op::Mask,
                "leakymin" => Op::LeakyMin(pf(&n.p, "rise")),
                "tsc" => {
                    let k = n.p["n"].as_u64().unwrap() as usize;
                    cap[ins[0]] = cap[ins[0]].max(k + 1);
                    Op::Tsc { thr: pf(&n.p, "thr"), n: k, cap: n.p["cap"].as_i64().unwrap() }
                }
                "hour_sin" => Op::HourSin,
                "hour_cos" => Op::HourCos,
                other => panic!("unknown op {other}"),
            };
            nodes.push(Node { op, ins, raw_col });
        }
        let outputs: Vec<String> = spec.nodes.iter().filter(|n| n.out).map(|n| n.name.clone()).collect();
        let out_idx = outputs.iter().map(|s| index[s]).collect();
        let n = nodes.len();
        Engine {
            nodes,
            state: vec![State { age: 1_000_000_000, ..State::default() }; n],
            hist: cap.iter().map(|&c| VecDeque::with_capacity(c)).collect(),
            cap,
            vals: vec![f64::NAN; n],
            raw_channels,
            outputs,
            out_idx,
        }
    }

    pub fn step(&mut self, raw: &[f64], k: i64) -> Vec<f64> {
        for i in 0..self.nodes.len() {
            let node = &self.nodes[i];
            let a: Vec<f64> = node.ins.iter().map(|&j| self.vals[j]).collect();
            let s = &mut self.state[i];
            let y = match &node.op {
                Op::Raw { ffill, zero_nan } => {
                    let mut x = raw[node.raw_col];
                    if *zero_nan && x == 0.0 {
                        x = f64::NAN;
                    }
                    if !x.is_nan() {
                        s.last = Some(x);
                        s.age = 0;
                        x
                    } else {
                        s.age += 1;
                        match s.last {
                            Some(l) if s.age <= *ffill => l,
                            _ => f64::NAN,
                        }
                    }
                }
                Op::Lin { coef, off } => {
                    let mut y = *off;
                    for (c, x) in coef.iter().zip(&a) {
                        y += c * x;
                    }
                    y
                }
                Op::Scale(c) => a[0] * c,
                Op::Mul => a[0] * a[1],
                Op::Pow(e) => a[0].powf(*e),
                Op::GMax | Op::GMin | Op::GMean | Op::GSpread => {
                    if a.iter().any(|x| x.is_nan()) {
                        f64::NAN
                    } else {
                        let mx = a.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                        let mn = a.iter().cloned().fold(f64::INFINITY, f64::min);
                        match node.op {
                            Op::GMax => mx,
                            Op::GMin => mn,
                            Op::GMean => a.iter().sum::<f64>() / a.len() as f64,
                            _ => mx - mn,
                        }
                    }
                }
                Op::Ewm { alpha, warm } => {
                    let x = a[0];
                    if x.is_nan() {
                        s.n = 0;
                        s.y = f64::NAN;
                        f64::NAN
                    } else {
                        s.n += 1;
                        s.y = if s.n == 1 { x } else { alpha * x + (1.0 - alpha) * s.y };
                        if s.n >= *warm { s.y } else { f64::NAN }
                    }
                }
                Op::Diff(n) => {
                    let h = &self.hist[node.ins[0]];
                    if h.len() > *n { a[0] - h[h.len() - 1 - n] } else { f64::NAN }
                }
                Op::Gt { c, inv } => {
                    if a[0].is_nan() { f64::NAN } else if (*inv && a[0] < *c) || (!*inv && a[0] > *c) { 1.0 } else { 0.0 }
                }
                Op::Mask => if a[0].is_nan() { 0.0 } else { 1.0 },
                Op::LeakyMin(rise) => {
                    let mut m = if s.init { s.y } else { f64::NAN };
                    if a[0].is_nan() {
                        m = if m.is_nan() { f64::NAN } else { m + rise };
                    } else {
                        m = if m.is_nan() { a[0] } else { a[0].min(m + rise) };
                    }
                    s.y = m;
                    s.init = true;
                    m
                }
                Op::Tsc { thr, n, cap } => {
                    let h = &self.hist[node.ins[0]];
                    let c = if s.init { s.c } else { *cap };
                    let changed = h.len() > *n && !a[0].is_nan() && {
                        let b = h[h.len() - 1 - n];
                        !b.is_nan() && (a[0] - b).abs() > *thr
                    };
                    s.c = if changed { 0 } else { (c + 1).min(*cap) };
                    s.init = true;
                    s.c as f64
                }
                Op::HourSin | Op::HourCos => {
                    let ang = 2.0 * std::f64::consts::PI * ((k.rem_euclid(86400)) as f64 / 86400.0);
                    if matches!(node.op, Op::HourSin) { ang.sin() } else { ang.cos() }
                }
            };
            self.vals[i] = y;
            if self.cap[i] > 0 {
                let h = &mut self.hist[i];
                if h.len() == self.cap[i] {
                    h.pop_front();
                }
                h.push_back(y);
            }
        }
        self.out_idx.iter().map(|&i| self.vals[i]).collect()
    }
}

#[derive(Deserialize)]
struct Tree {
    f: Vec<i64>,
    t: Vec<f64>,
    l: Vec<i64>,
    r: Vec<i64>,
    m: Vec<u8>,
    v: Vec<f64>,
}

#[derive(Deserialize)]
struct Target {
    base: f64,
    trees: Vec<Tree>,
}

#[derive(Deserialize, Clone, Copy, Default, Debug)]
pub struct Identified {
    pub fan: bool,
    pub pump: bool,
}

#[derive(Deserialize)]
struct RawModel {
    features: Vec<String>,
    #[serde(default)]
    identified: Identified,
    targets: std::collections::BTreeMap<String, Target>,
}

pub struct Model {
    pub features: Vec<String>,
    pub identified: Identified,
    targets: Vec<(String, Target)>,
}

impl Model {
    pub fn from_json(text: &str) -> Model {
        let m: RawModel = serde_json::from_str(text).unwrap();
        Model { features: m.features, identified: m.identified, targets: m.targets.into_iter().collect() }
    }

    pub fn target_names(&self) -> Vec<&str> {
        self.targets.iter().map(|t| t.0.as_str()).collect()
    }

    pub fn predict(&self, x: &[f64]) -> Vec<(String, f64)> {
        let xf: Vec<f32> = x.iter().map(|&v| v as f32).collect();
        self.targets
            .iter()
            .map(|(name, t)| {
                let mut s = t.base;
                for tr in &t.trees {
                    let mut i = 0usize;
                    while tr.f[i] >= 0 {
                        let v = xf[tr.f[i] as usize];
                        let left = if v.is_nan() { tr.m[i] == 1 } else { v < tr.t[i] as f32 };
                        i = if left { tr.l[i] } else { tr.r[i] } as usize;
                    }
                    s += tr.v[i];
                }
                (name.clone(), s)
            })
            .collect()
    }
}

#[derive(Clone)]
pub struct Predictor {
    pub engine: Engine,
    pub model: Arc<Model>,
    order: Vec<usize>,
}

impl Predictor {
    pub fn new(spec_json: &str, model_json: &str) -> Predictor {
        let engine = Engine::from_json(spec_json);
        let model = Arc::new(Model::from_json(model_json));
        let pos: HashMap<&String, usize> = engine.outputs.iter().enumerate().map(|(i, s)| (s, i)).collect();
        let order = model.features.iter().map(|f| pos[f]).collect();
        Predictor { engine, model, order }
    }

    pub fn load(spec_json: &str, model_json: &str) -> Result<Predictor, String> {
        std::panic::catch_unwind(|| Predictor::new(spec_json, model_json)).map_err(|_| "feature spec or model file is malformed".to_string())
    }

    pub fn step(&mut self, raw: &[f64], k: i64) -> (Vec<f64>, Vec<(String, f64)>) {
        let out = self.engine.step(raw, k);
        let x: Vec<f64> = self.order.iter().map(|&i| out[i]).collect();
        let p = self.model.predict(&x);
        (x, p)
    }
}
