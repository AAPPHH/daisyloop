use model_infer::Engine;
use std::collections::HashMap;

fn spec() -> String {
    std::fs::read_to_string(format!("{}/../../models/feature_spec_deploy.json", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn check(e: &mut Engine, raw: &[f64], k: i64, steps: usize) -> usize {
    let names = e.outputs.clone();
    let mut ewms = 0;
    for _ in 0..steps {
        let out = e.step(raw, k);
        let m: HashMap<&str, f64> = names.iter().map(|s| s.as_str()).zip(out.iter().cloned()).collect();
        for (n, v) in &m {
            let ewm = n.rsplit_once(".e").filter(|(b, t)| m.contains_key(b) && t.chars().all(|c| c.is_ascii_digit()));
            if let Some((b, _)) = ewm {
                assert!((v - m[b]).abs() < 1e-9, "{n}={v} base={}", m[b]);
                ewms += 1;
            }
            if n.contains(".d") && !n.contains('*') && n.starts_with("water.e") {
                assert!(v.is_nan() || v.abs() < 1e-9, "{n}={v}");
            }
        }
    }
    ewms
}

fn engine() -> Engine {
    let mut e = Engine::from_json(&spec());
    e.set_eager(true);
    e
}

#[test]
fn eager_ewm_starts_from_the_first_value_and_derivatives_stay_flat() {
    let mut e = engine();
    let raw = vec![31.0; e.raw_channels.len()];
    assert!(check(&mut e, &raw, 1, 200) > 1000);
}

#[test]
fn eager_ewm_restarts_from_the_first_value_after_a_gap_above_the_hold() {
    let mut e = engine();
    let raw = vec![31.0; e.raw_channels.len()];
    check(&mut e, &raw, 1, 5);
    let gap = vec![f64::NAN; raw.len()];
    for _ in 0..1000 {
        e.step(&gap, 1);
    }
    let raw2 = vec![44.0; raw.len()];
    assert!(check(&mut e, &raw2, 1, 200) > 1000);
}
