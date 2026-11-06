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
    check("golden.json", "feature_spec_deploy.json", "model_deploy.json", 1e-4);
}

#[test]
fn linear_matches_python_golden() {
    check("golden_linear.json", "feature_spec_linear.json", "model_linear.json", 1e-9);
    let p = Predictor::load(&models("feature_spec_linear.json"), &models("model_linear.json")).unwrap();
    assert!(p.model.is_linear() && p.model.target_names().contains(&"water_30"));
}

#[test]
fn linear_without_lhm_uses_the_fallback_and_stays_finite() {
    let mut p = Predictor::new(&models("feature_spec_linear.json"), &models("model_linear.json"));
    let g: Value = serde_json::from_str(&models("golden_linear.json")).unwrap();
    let chans: Vec<String> = g["channels"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    let rows = g["rows"].as_array().unwrap();
    let k0 = g["k0"].as_i64().unwrap();
    let lhm = |c: &str| c.starts_with("/intelcpu") || c.starts_with("/gpu-nvidia") || c.starts_with("/lpc") || c.starts_with("/nvme") || c.starts_with("/memory");
    let mut seen = 0;
    for (t, row) in rows.iter().enumerate() {
        let raw: Vec<f64> = row.as_array().unwrap().iter().zip(&chans).map(|(v, c)| if lhm(c) { f64::NAN } else { num(v) }).collect();
        let (x, preds) = p.step(&raw, k0 + t as i64);
        if t > 2400 && x.iter().filter(|v| v.is_nan()).count() > 0 {
            seen += 1;
            assert!(preds.iter().all(|(_, v)| v.is_finite() && v.abs() < 100.0), "tick {t}: {preds:?}");
        }
    }
    assert!(seen > 100);
}

fn check(golden: &str, spec: &str, model: &str, tol: f64) {
    let g: Value = serde_json::from_str(&models(golden)).unwrap();
    let mut p = Predictor::new(&models(spec), &models(model));
    let chans: Vec<String> = g["channels"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    assert_eq!(chans, *p.engine.raw_channels);
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
                if e.is_nan() || v.is_nan() {
                    assert!(e.is_nan() && v.is_nan(), "tick {t} {name}: expected {e} got {v}");
                } else {
                    worst_p = worst_p.max((e - v).abs());
                }
            }
            next += 1;
        }
    }
    println!("features finite={finite} nan={nan} worst_rel={worst_f:e} worst_pred_abs={worst_p:e}");
    assert!(finite > 10000 && nan > 100);
    assert!(worst_f < 1e-9, "feature mismatch {worst_f}");
    assert!(worst_p < tol, "prediction mismatch {worst_p}");
}

#[test]
fn shipped_model_is_not_identified_and_bad_files_are_errors() {
    let p = Predictor::load(&models("feature_spec_deploy.json"), &models("model_deploy.json")).unwrap();
    assert!(!p.model.identified.fan && !p.model.identified.pump);
    assert!(p.model.target_names().contains(&"water_120"));
    let bare = models("model_deploy.json").replace("\"identified\":{\"fan\":false,\"pump\":false},", "");
    assert!(!Predictor::load(&models("feature_spec_deploy.json"), &bare).unwrap().model.identified.fan);
    assert!(Predictor::load("{}", &models("model_deploy.json")).is_err());
}

#[test]
fn step_model_with_response_term_matches_python_golden() {
    check("fixtures/steptrain/golden_deploy.json", "fixtures/steptrain/feature_spec_deploy.json", "fixtures/steptrain/model_deploy.json", 1e-9);
    let p = Predictor::load(&models("fixtures/steptrain/feature_spec_deploy.json"), &models("fixtures/steptrain/model_deploy.json")).unwrap();
    assert!(p.model.is_linear() && p.model.identified.fan && !p.model.identified.pump);
}
