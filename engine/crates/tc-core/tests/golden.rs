//! Golden test: the Rust sizing port must agree with the original Python `sizing.py` on ~2.8k grid points.
use tc_core::sizing::*;

#[test]
fn rust_sizing_matches_python_original() {
    let raw = include_str!("../../../tests/golden/sizing.json");
    let rows: Vec<serde_json::Value> = serde_json::from_str(raw).unwrap();
    assert!(rows.len() > 2000);
    for r in &rows {
        let (p, ask, bank) = (
            r["p"].as_f64().unwrap(),
            r["ask"].as_f64().unwrap(),
            r["bank"].as_f64().unwrap(),
        );
        let ctx = format!("p={p} ask={ask} bank={bank}");
        assert!(
            (kalshi_fee(ask) - r["fee"].as_f64().unwrap()).abs() < 1e-12,
            "fee {ctx}"
        );
        assert!(
            (net_edge(p, ask, true) - r["edge"].as_f64().unwrap()).abs() < 1e-12,
            "edge {ctx}"
        );
        assert_eq!(
            theta_for_account(bank),
            r["theta"].as_f64().unwrap(),
            "theta {ctx}"
        );
        assert_eq!(
            limit_price_cents(p, theta_for_account(bank)),
            r["limit_c"].as_i64().unwrap(),
            "limit {ctx}"
        );
        assert!(
            (kelly_contracts(p, ask, bank, 0.25, 0.015) - r["kelly"].as_f64().unwrap()).abs()
                < 1e-9,
            "kelly {ctx}"
        );
        assert_eq!(
            slippage_ok(net_edge(p, ask, true), 0.03, 0.5),
            r["slip_ok"].as_bool().unwrap(),
            "slip {ctx}"
        );
    }
}
