use model_infer::Predictor;
use serde_json::Value;
use std::fs;

fn models(name: &str) -> String {
    fs::read_to_string(format!("{}/../../models/{}", env!("CARGO_MANIFEST_DIR"), name)).unwrap()
}

fn num(v: &Value) -> f64 {
    v.as_f64().unwrap_or(f64::NAN)
}

#[test]
fn matches_python_golden() {
    let g: Value = serde_json::from_str(&models("golden.json")).unwrap();
    let mut p = Predictor::new(&models("feature_spec_deploy.json"), &models("model_deploy.json"));
    let chans: Vec<String> = g["channels"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    assert_eq!(chans, p.engine.raw_channels);
    let k0 = g["k0"].as_i64().unwrap();
    let ticks: Vec<usize> = g["ticks"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as usize).collect();
    let rows = g["rows"].as_array().unwrap();
    let ef = g["expected_features"].as_array().unwrap();
    let ep = g["expected_predictions"].as_array().unwrap();
    let mut next = 0;
    let (mut worst_f, mut worst_p, mut finite, mut nan) = (0.0f64, 0.0f64, 0usize, 0usize);
    for (t, row) in rows.iter().enumerate() {
        let raw: Vec<f64> = row.as_array().unwrap().iter().map(num).collect();
        let (x, preds) = p.step(&raw, k0 + t as i64);
        if next < ticks.len() && ticks[next] == t {
            for (j, v) in ef[next].as_array().unwrap().iter().enumerate() {
                let e = num(v);
                if e.is_nan() || x[j].is_nan() {
                    assert!(e.is_nan() && x[j].is_nan(), "tick {t} feature {j}: expected {e} got {}", x[j]);
                    nan += 1;
                } else {
                    let d = (e - x[j]).abs() / (1.0 + e.abs());
                    worst_f = worst_f.max(d);
                    finite += 1;
                }
            }
            for (name, v) in &preds {
                let e = num(&ep[next][name]);
                worst_p = worst_p.max((e - v).abs());
            }
            next += 1;
        }
    }
    println!("features finite={finite} nan={nan} worst_rel={worst_f:e} worst_pred_abs={worst_p:e}");
    assert!(finite > 10000 && nan > 100);
    assert!(worst_f < 1e-9, "feature mismatch {worst_f}");
    assert!(worst_p < 1e-4, "prediction mismatch {worst_p}");
}
