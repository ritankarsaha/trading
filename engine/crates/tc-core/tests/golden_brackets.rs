//! Golden test: Rust bracket probabilities vs the skill's own Python (`weather_brackets.py`).
use tc_core::markets::*;

#[test]
fn rust_brackets_match_skill_python() {
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../../../tests/golden/brackets.json")).unwrap();
    assert!(rows.len() > 300);
    for r in &rows {
        match r["k"].as_str().unwrap() {
            "bracket" => {
                let got = bracket_p_yes_int(
                    r["floor"].as_i64().unwrap(),
                    r["cap"].as_i64().unwrap(),
                    r["mu"].as_f64().unwrap(),
                    r["sigma"].as_f64().unwrap(),
                );
                assert!(
                    (got - r["p"].as_f64().unwrap()).abs() < 1e-12,
                    "{r}: got {got}"
                );
            }
            "thr" => {
                let got = threshold_p_yes_int(
                    r["strike"].as_i64().unwrap(),
                    r["mu"].as_f64().unwrap(),
                    r["sigma"].as_f64().unwrap(),
                    r["greater"].as_bool().unwrap(),
                );
                assert!(
                    (got - r["p"].as_f64().unwrap()).abs() < 1e-12,
                    "{r}: got {got}"
                );
            }
            _ => {
                let (mu, sg) = mu_sigma_from_quantiles(
                    r["p10"].as_f64().unwrap(),
                    r["p50"].as_f64().unwrap(),
                    r["p90"].as_f64().unwrap(),
                    r["scale"].as_f64().unwrap(),
                );
                let o = r["out"].as_array().unwrap();
                assert!(
                    (mu - o[0].as_f64().unwrap()).abs() < 1e-12
                        && (sg - o[1].as_f64().unwrap()).abs() < 1e-12,
                    "{r}"
                );
            }
        }
    }
}
