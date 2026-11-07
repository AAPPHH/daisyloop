use super::*;

const REST: [f64; 3] = [800.0, 3000.0, 150.0];
const HORIZONS: [u32; 4] = [30, 60, 90, 120];

fn water(t: i64) -> f64 {
    32.0 + 2.0 * (t as f64 / 300.0).sin() + 0.5 * (t as f64 / 47.0).sin()
}

fn response(t: i64) -> f64 {
    1.5 + (t as f64 / 500.0).sin()
}

struct Truth {
    bias: f64,
    scale: f64,
}

impl Truth {
    fn live(&self, t: i64) -> Vec<Live> {
        HORIZONS.iter().map(|h| Live { h: *h, pred: water(t + *h as i64) - water(t) - self.bias - self.scale * response(t), response: response(t) }).collect()
    }
}

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("daisyloop-corr-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run(c: &mut Corrector, truth: &Truth, from: i64, to: i64) {
    for t in from..to {
        c.feed(t, true, Some(water(t)), REST, &truth.live(t));
    }
}

fn cfg() -> CorrectionConfig {
    CorrectionConfig::default()
}

#[test]
fn rls_recovers_a_constant_bias_and_a_response_scale_error() {
    let d = dir("recover");
    let mut c = Corrector::new(&cfg(), &d, 1);
    run(&mut c, &Truth { bias: 0.3, scale: 0.2 }, 0, 6 * 3600);
    for h in HORIZONS {
        let r = c.state()[&h];
        assert!((r.theta[0] - 0.3).abs() < 0.03 && (r.theta[1] - 0.2).abs() < 0.03, "{h} {r:?}");
    }
    let shown = c.shown();
    assert!(shown.iter().all(|s| s.acting && (s.scale - 1.2).abs() < 0.03 && (s.bias - 0.3).abs() < 0.03));
    assert!((c.delta(120, 2.0) - (0.3 + 0.2 * 2.0)).abs() < 0.05);
}

#[test]
fn the_correction_acts_only_after_the_minimum_number_of_updates() {
    let d = dir("min");
    let mut c = Corrector::new(&cfg(), &d, 1);
    let truth = Truth { bias: 0.4, scale: 0.0 };
    run(&mut c, &truth, 0, 600);
    assert!(c.state()[&120].n < cfg().min_updates && c.delta(120, 1.0) == 0.0 && c.shown().iter().all(|s| !s.acting && s.delta == 0.0));
    run(&mut c, &truth, 600, 4000);
    assert!(c.state()[&120].n >= cfg().min_updates && c.delta(120, 1.0) > 0.2);
    assert_eq!(c.delta(7, 1.0), 0.0);
}

#[test]
fn bias_and_scale_stay_inside_their_bounds_for_any_truth() {
    let d = dir("bounds");
    let c0 = cfg();
    for (bias, scale) in [(3.0, 3.0), (-3.0, -3.0), (0.9, 0.9), (-1.2, 2.0)] {
        let mut c = Corrector::new(&c0, &d, 1);
        for t in 0..4 * 3600 {
            c.feed(t, true, Some(water(t)), REST, &Truth { bias, scale }.live(t));
            for r in c.state().values() {
                assert!(r.bounded(&c0), "{bias} {scale} {r:?}");
            }
        }
        assert!(c.shown().iter().all(|s| s.bias.abs() <= 1.0 && (0.5..=1.5).contains(&s.scale)));
    }
}

#[test]
fn skipped_seconds_never_update() {
    let d = dir("skip");
    let truth = Truth { bias: 0.5, scale: 0.0 };
    let n = |c: &Corrector| c.state().get(&120).map_or(0, |r| r.n);
    let mut c = Corrector::new(&cfg(), &d, 1);
    for t in 0..3000 {
        c.feed(t, false, Some(water(t)), REST, &truth.live(t));
    }
    assert_eq!(n(&c), 0);
    for t in 3000..6000 {
        c.feed(t, true, None, REST, &truth.live(t));
    }
    assert_eq!(n(&c), 0);
    for t in 6000..9000 {
        c.feed(t, true, Some(water(t)), REST, &[]);
    }
    assert_eq!(n(&c), 0);
    for t in 9000..12000 {
        c.feed(t, true, Some(31.0), REST, &truth.live(t));
    }
    assert_eq!(n(&c), 0);
    for t in 12000..12400 {
        c.feed(t, true, Some(water(t)), REST, &truth.live(t));
    }
    assert!(n(&c) > 0);
    let before = n(&c);
    c.feed(12400, false, Some(water(12400)), REST, &truth.live(12400));
    for t in 12401..12520 {
        c.feed(t, true, Some(water(t)), REST, &truth.live(t));
        assert_eq!(n(&c), before, "{t}");
    }
    c.feed(12600, true, Some(water(12600)), REST, &truth.live(12600));
    assert_eq!(n(&c), before);
    c.feed(12601, true, Some(water(12601) + 5.0), REST, &truth.live(12601));
    assert_eq!(n(&c), before);
}

#[test]
fn an_outlier_residual_is_rejected_and_counted() {
    let d = dir("outlier");
    let truth = Truth { bias: 0.2, scale: 0.0 };
    let mut c = Corrector::new(&cfg(), &d, 1);
    run(&mut c, &truth, 0, 2000);
    let wild = Truth { bias: 6.0, scale: 0.0 };
    run(&mut c, &wild, 2000, 2062);
    let (before, theta) = (c.state()[&60], c.state()[&60].theta);
    run(&mut c, &wild, 2062, 2130);
    let after = c.state()[&60];
    assert!(after.rejected > before.rejected && after.theta == theta && after.n == before.n);
}

#[test]
fn covariance_stays_capped_without_excitation_and_the_scale_is_not_thrown_by_the_next_one() {
    let d = dir("excite");
    let c0 = cfg();
    let mut c = Corrector::new(&c0, &d, 1);
    let flat = |t: i64| -> Vec<Live> { HORIZONS.iter().map(|h| Live { h: *h, pred: water(t + *h as i64) - water(t) - 0.1, response: 0.0 }).collect() };
    for t in 0..12 * 3600 {
        c.feed(t, true, Some(water(t)), REST, &flat(t));
    }
    let r = c.state()[&120];
    assert!(r.p[1][1] <= c0.p0_scale + 1e-12 && r.p[0][0] <= c0.p0_bias + 1e-12 && (r.theta[0] - 0.1).abs() < 0.02 && r.theta[1].abs() < 1e-9, "{r:?}");
    run(&mut c, &Truth { bias: 0.1, scale: 0.0 }, 12 * 3600, 12 * 3600 + 600);
    assert!(c.state()[&120].theta[1].abs() < 0.1);
}

#[test]
fn state_survives_a_restart_and_is_discarded_for_another_model_or_a_broken_file() {
    let d = dir("persist");
    let c0 = cfg();
    let mut c = Corrector::new(&c0, &d, 0xabc);
    run(&mut c, &Truth { bias: 0.3, scale: 0.1 }, 0, 4000);
    c.save(10_000, false);
    assert!(d.join(Corrector::FILE).exists() && !d.join("online_correction.json.tmp").exists());
    let same = Corrector::new(&c0, &d, 0xabc);
    assert_eq!(same.state().len(), c.state().len());
    for (h, r) in c.state() {
        let s = same.state()[h];
        assert!(s.n == r.n && (s.theta[0] - r.theta[0]).abs() < 1e-12 && (s.theta[1] - r.theta[1]).abs() < 1e-12 && (s.p[0][0] - r.p[0][0]).abs() < 1e-12);
    }
    assert!(same.delta(120, 1.0) > 0.2);
    assert!(Corrector::new(&c0, &d, 0xabd).state().is_empty());
    assert!(Corrector::new(&c0, &d, 0).state().is_empty());
    std::fs::write(d.join(Corrector::FILE), "{ not json").unwrap();
    assert!(Corrector::new(&c0, &d, 0xabc).state().is_empty());
    let bad = Saved { version: 1, digest: format!("{:016x}", 0xabcu64), horizons: BTreeMap::from([(120, Rls { theta: [9.0, 0.0], ..Rls::fresh(&c0) })]) };
    std::fs::write(d.join(Corrector::FILE), serde_json::to_string(&bad).unwrap()).unwrap();
    assert!(Corrector::new(&c0, &d, 0xabc).state().is_empty());
    let off = CorrectionConfig { enabled: false, ..c0 };
    let mut c = Corrector::new(&off, &d, 1);
    run(&mut c, &Truth { bias: 0.3, scale: 0.1 }, 0, 4000);
    assert!(c.state().is_empty() && c.delta(120, 1.0) == 0.0);
}

#[test]
fn saving_is_throttled_and_only_when_something_changed() {
    let d = dir("throttle");
    let mut c = Corrector::new(&cfg(), &d, 7);
    c.save(0, true);
    assert!(!d.join(Corrector::FILE).exists());
    run(&mut c, &Truth { bias: 0.3, scale: 0.1 }, 0, 400);
    c.save(1000, false);
    let first = std::fs::read_to_string(d.join(Corrector::FILE)).unwrap();
    run(&mut c, &Truth { bias: 0.3, scale: 0.1 }, 400, 800);
    c.save(1010, false);
    assert_eq!(std::fs::read_to_string(d.join(Corrector::FILE)).unwrap(), first);
    c.save(1010, true);
    assert_ne!(std::fs::read_to_string(d.join(Corrector::FILE)).unwrap(), first);
}

#[test]
fn the_config_validates_and_rejects_nonsense() {
    assert!(cfg().validate().is_ok());
    for bad in ["half_life_secs = 0.0", "half_life_secs = 100.0", "update_secs = 0", "bias_max = -1.0", "scale_min = 1.2", "scale_max = 0.9", "restart_hold_secs = -1", "jump_k = 0.0"] {
        assert!(toml::from_str::<CorrectionConfig>(bad).unwrap().validate().is_err(), "{bad}");
    }
    assert!(toml::from_str::<CorrectionConfig>("half_life_secs = 7200.0\nenabled = false").unwrap().validate().is_ok());
    assert!((cfg().lambda().powf(2160.0) - 0.5).abs() < 1e-9);
}

#[test]
fn only_windows_with_held_fan_and_pump_rpm_update() {
    let d = dir("held");
    let truth = Truth { bias: 0.4, scale: 0.0 };
    let mut c = Corrector::new(&cfg(), &d, 1);
    for t in 0..4000 {
        c.feed(t, true, Some(water(t)), [800.0 + 400.0 * ((t / 20) % 2) as f64, 3000.0, 150.0], &truth.live(t));
    }
    assert_eq!(c.state().get(&120).map_or(0, |r| r.n), 0);
    for t in 4000..8000 {
        c.feed(t, true, Some(water(t)), [800.0 + 10.0 * ((t / 20) % 2) as f64, 3000.0 + 30.0 * ((t / 7) % 2) as f64, 150.0], &truth.live(t));
    }
    assert!(c.state()[&120].n > 100);
    for t in 8000..8200 {
        c.feed(t, true, Some(water(t)), [800.0, 3000.0, 150.0 + 100.0 * ((t / 30) % 2) as f64], &truth.live(t));
    }
    let n = c.state()[&120].n;
    for t in 8200..12000 {
        c.feed(t, true, Some(water(t)), [800.0, 3000.0, 150.0 + 100.0 * ((t / 30) % 2) as f64], &truth.live(t));
    }
    assert_eq!(c.state()[&120].n, n);
}

#[test]
fn a_repeated_epoch_keeps_the_latest_values_and_does_not_clear_the_ring_but_a_backwards_jump_does() {
    let truth = Truth { bias: 0.3, scale: 0.1 };
    let n = |c: &Corrector| c.state().get(&120).map_or(0, |r| r.n);
    let (mut clean, mut rep) = (Corrector::new(&cfg(), &dir("rep-a"), 1), Corrector::new(&cfg(), &dir("rep-b"), 1));
    for t in 0..3 * 3600 {
        clean.feed(t, true, Some(water(t)), REST, &truth.live(t));
        rep.feed(t, true, Some(water(t) + if t % 83 == 0 { 0.05 } else { 0.0 }), REST, &truth.live(t));
        if t % 83 == 0 {
            rep.feed(t, true, Some(water(t)), REST, &truth.live(t));
        }
    }
    assert_eq!(n(&rep), n(&clean));
    assert!((0..2).all(|i| (rep.state()[&120].theta[i] - clean.state()[&120].theta[i]).abs() < 0.01));
    let before = n(&rep);
    rep.feed(3 * 3600 - 500, true, Some(water(3 * 3600 - 500)), REST, &truth.live(3 * 3600 - 500));
    for t in 3 * 3600 - 499..3 * 3600 - 400 {
        rep.feed(t, true, Some(water(t)), REST, &truth.live(t));
    }
    assert_eq!(n(&rep), before);
}

#[test]
fn sustained_saturation_raises_the_drift_flag_after_six_hours_and_the_covariance_is_shrunk_not_reset() {
    let c0 = cfg();
    let mut c = Corrector::new(&c0, &dir("drift"), 1);
    run(&mut c, &Truth { bias: 3.0, scale: 0.0 }, 0, 5 * 3600);
    let r = c.state()[&120];
    assert!(r.theta[0] > 0.95 * c0.bias_max && r.p[0][0] < c0.p0_bias && r.bounded(&c0));
    assert!(c.drifting().is_empty() && c.shown().iter().all(|s| !s.drift));
    run(&mut c, &Truth { bias: 3.0, scale: 0.0 }, 5 * 3600, 9 * 3600);
    assert!(c.drifting().contains(&120) && c.shown().iter().any(|s| s.drift));
    run(&mut c, &Truth { bias: 0.2, scale: 0.0 }, 9 * 3600, 22 * 3600);
    assert!(c.drifting().is_empty());
    let mut ok = Corrector::new(&c0, &dir("nodrift"), 1);
    run(&mut ok, &Truth { bias: 0.3, scale: 0.1 }, 0, 12 * 3600);
    assert!(ok.drifting().is_empty());
}
