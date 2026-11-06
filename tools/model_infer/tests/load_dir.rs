use model_infer::Predictor;
use std::fs;

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[test]
fn trainer_output_dir_loads_and_the_old_spec_does_not() {
    let Ok(dir) = std::env::var("STEPTRAIN_OUT") else { return };
    let (spec, model) = (read(&format!("{dir}/feature_spec_deploy.json")), read(&format!("{dir}/model_deploy.json")));
    let p = Predictor::load(&spec, &model).unwrap();
    assert!(p.model.is_linear() && p.model.target_names().contains(&"water_120"));
    let old = read(&format!("{}/../../models/feature_spec_deploy.json", env!("CARGO_MANIFEST_DIR")));
    assert!(Predictor::load(&old, &model).is_err());
}
