//! Edge cases: input the solver is most likely to get wrong.
//!
//! Organised by the kind of thing that goes wrong, because that is how you
//! read a failure. Each group states the invariant it is protecting.
//!
//! Run with `cargo test -p xirr-core --test edge_cases`.

use std::str::FromStr;

use xirr_core::{sign_changes, xirr, xirr_all_roots, xnpv, DateLike, RootPolicy};

const ALL_POLICIES: [RootPolicy; 4] = [
  RootPolicy::SpreadsheetCompat,
  RootPolicy::SpreadsheetThenRobust,
  RootPolicy::Lowest,
  RootPolicy::ClosestToGuess,
];

fn dates(iso: &[&str]) -> Vec<DateLike> {
  iso.iter().map(|s| DateLike::from_str(s).unwrap()).collect()
}

/// Annual dates starting 2020-01-01, one per amount.
fn annual(n: usize) -> Vec<DateLike> {
  (0..n)
    .map(|i| DateLike::from_str(&format!("{}-01-01", 2020 + i)).unwrap())
    .collect()
}

fn solve(d: &[DateLike], a: &[f64]) -> f64 {
  xirr(d, a, None, None, None).unwrap()
}

// ===========================================================================
// 1. Input validation - these must Err, not return NaN or panic
// ===========================================================================

#[test]
fn rejects_empty_input() {
  assert!(xirr(&[], &[], None, None, None).is_err());
  assert!(xirr_all_roots(&[], &[], None).is_err());
}

#[test]
fn rejects_a_single_payment() {
  let d = dates(&["2020-01-01"]);
  assert!(xirr(&d, &[-100.0], None, None, None).is_err());
}

#[test]
fn rejects_mismatched_lengths() {
  let d = dates(&["2020-01-01", "2021-01-01"]);
  assert!(xirr(&d, &[-100.0], None, None, None).is_err());
  assert!(xirr(&d, &[-100.0, 50.0, 50.0], None, None, None).is_err());
  assert!(xnpv(0.1, &d, &[-100.0], None).is_err());
}

#[test]
fn rejects_single_signed_cash_flows() {
  let d = annual(3);
  assert!(xirr(&d, &[100.0, 200.0, 300.0], None, None, None).is_err());
  assert!(xirr(&d, &[-100.0, -200.0, -300.0], None, None, None).is_err());
  assert!(xirr(&d, &[0.0, 0.0, 0.0], None, None, None).is_err());
}

#[test]
fn rejects_dates_before_the_first_under_every_policy() {
  // Spreadsheets raise #NUM! rather than reordering. Every policy must agree,
  // otherwise `Lowest` would quietly accept input `SpreadsheetCompat` rejects.
  let d = dates(&["2021-01-01", "2020-01-01", "2022-01-01"]);
  let a = [-100.0, 60.0, 60.0];
  for policy in ALL_POLICIES {
    assert!(
      xirr(&d, &a, None, None, Some(policy)).is_err(),
      "{policy:?} accepted out-of-order dates"
    );
  }
}

#[test]
fn rejects_guesses_outside_the_domain() {
  let d = annual(2);
  let a = [-100.0, 130.0];
  for bad in [
    -1.0,
    -1.5,
    -1e300,
    f64::NAN,
    f64::INFINITY,
    f64::NEG_INFINITY,
  ] {
    assert!(
      xirr(&d, &a, Some(bad), None, None).is_err(),
      "guess {bad} should be rejected"
    );
  }
  // ...and accepts anything strictly above -1, however extreme.
  for ok in [-0.999_999, 0.0, 1e6] {
    assert!(xirr(&d, &a, Some(ok), None, None).is_ok(), "guess {ok}");
  }
}

#[test]
fn non_finite_amounts_never_panic() {
  // Garbage in is allowed to produce NaN out, but must not panic or hang.
  let d = annual(3);
  for poison in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
    let a = [-1000.0, poison, 1300.0];
    let result = xirr(&d, &a, None, None, None);
    if let Ok(rate) = result {
      assert!(rate.is_nan() || rate.is_finite(), "{poison}: got {rate}");
    }
  }
}

// ===========================================================================
// 2. Degenerate schedules
// ===========================================================================

#[test]
fn all_payments_on_the_same_date_has_no_rate() {
  // Every year fraction is 0, so XNPV is constant and never crosses zero.
  let d = dates(&["2020-01-01", "2020-01-01", "2020-01-01"]);
  let rate = solve(&d, &[-1000.0, 400.0, 400.0]);
  assert!(rate.is_nan(), "expected no solution, got {rate}");
}

#[test]
fn duplicate_dates_are_additive() {
  // Two payments on one day must behave exactly like their sum.
  let split = dates(&["2020-01-01", "2020-01-01", "2022-01-01"]);
  let merged = dates(&["2020-01-01", "2022-01-01"]);
  let a = solve(&split, &[-500.0, -500.0, 1300.0]);
  let b = solve(&merged, &[-1000.0, 1300.0]);
  assert!((a - b).abs() < 1e-12, "{a} vs {b}");
}

#[test]
fn interleaved_zero_amounts_do_not_change_the_rate() {
  let with_zeros = dates(&["2020-01-01", "2020-06-01", "2021-01-01", "2022-01-01"]);
  let without = dates(&["2020-01-01", "2022-01-01"]);
  let a = solve(&with_zeros, &[-1000.0, 0.0, 0.0, 1300.0]);
  let b = solve(&without, &[-1000.0, 1300.0]);
  assert!((a - b).abs() < 1e-12, "{a} vs {b}");
}

#[test]
fn trailing_zero_payment_is_harmless() {
  let d = annual(3);
  let rate = solve(&d, &[-1000.0, 1200.0, 0.0]);
  // Not 0.20: 2020 is a leap year, so the holding period is 366/365 of a
  // year and the annualised rate is correspondingly lower.
  assert!((rate - 0.199_402_373_269_093).abs() < 1e-12, "got {rate}");
}

// ===========================================================================
// 3. Numeric extremes
// ===========================================================================

#[test]
fn magnitude_does_not_change_the_answer() {
  // The bug this guards: absolute tolerances made 1e12 unsolvable while 1e9
  // solved fine. Multiplying every amount by k cannot change the IRR.
  let d = annual(4);
  let base = [-1000.0, 5000.0, -6000.0, 2500.0];
  let reference = solve(&d, &base);
  assert!(reference.is_finite());

  for exp in [-6i32, -3, 0, 3, 6, 9, 12, 15] {
    let k = 10f64.powi(exp);
    let scaled: Vec<f64> = base.iter().map(|a| a * k).collect();
    let rate = solve(&d, &scaled);
    assert!(rate.is_finite(), "1e{exp} returned no rate");
    assert!(
      (rate - reference).abs() < 1e-9,
      "1e{exp} gave {rate}, 1e0 gave {reference}"
    );
  }
}

#[test]
fn handles_a_century_long_horizon() {
  let d = dates(&["1925-01-01", "2025-01-01"]);
  let rate = solve(&d, &[-1000.0, 1_000_000.0]);
  assert!(rate.is_finite(), "got {rate}");
  // ACT/365F counts actual days, so this span is 36,525 days = 100.0685
  // "years", not 100. The rate is 1000^(365/36525) - 1, and expecting the
  // round-number 100-year answer here is a classic off-by-25-leap-days bug.
  assert!((rate - 0.071_468_643_922_357).abs() < 1e-12, "got {rate}");
}

#[test]
fn handles_a_one_day_horizon() {
  let d = dates(&["2020-01-01", "2020-01-02"]);
  let rate = solve(&d, &[-1000.0, 1001.0]);
  assert!(rate.is_finite() && rate > 0.0, "got {rate}");
  let residual = xnpv(rate, &d, &[-1000.0, 1001.0], None).unwrap();
  assert!(residual.abs() < 1e-6, "residual {residual}");
}

#[test]
fn handles_a_rate_at_almost_total_loss() {
  let d = annual(2);
  let a = [-1000.0, 1.0];
  let rate = solve(&d, &a);
  assert!(rate > -1.0, "rate must stay inside the domain, got {rate}");
  assert!((rate - -0.998_980_947_1).abs() < 1e-9, "got {rate}");
}

#[test]
fn handles_a_rate_at_almost_exactly_zero() {
  let d = annual(2);
  for delta in [1e-4, 1e-6, 0.0, -1e-6, -1e-4] {
    let a = [-1000.0, 1000.0 + delta];
    let rate = solve(&d, &a);
    assert!(rate.is_finite(), "delta {delta} returned no rate");
    assert!(rate.abs() < 1e-3, "delta {delta} gave {rate}");
  }
}

#[test]
fn handles_a_thousand_payments() {
  // Guards against quadratic behaviour and accumulated error.
  let n = 1000;
  let d: Vec<DateLike> = (0..n)
    .map(|i| {
      let year = 2000 + i / 12;
      let month = i % 12 + 1;
      DateLike::from_str(&format!("{year}-{month:02}-01")).unwrap()
    })
    .collect();
  let mut a = vec![50.0; n as usize];
  a[0] = -10_000.0;

  let rate = solve(&d, &a);
  assert!(rate.is_finite(), "got {rate}");
  let residual = xnpv(rate, &d, &a, None).unwrap();
  let gross: f64 = a.iter().map(|x| x.abs()).sum();
  assert!(residual.abs() < 1e-6 * gross, "residual {residual}");
}

// ===========================================================================
// 4. Policy invariants
// ===========================================================================

#[test]
fn every_policy_agrees_when_the_root_is_unique() {
  // Descartes: one sign change means one root, so policy cannot matter.
  let d = annual(4);
  let a = [-1000.0, 300.0, 400.0, 500.0];
  assert_eq!(sign_changes(&a), 1);

  let rates: Vec<f64> = ALL_POLICIES
    .iter()
    .map(|p| xirr(&d, &a, None, None, Some(*p)).unwrap())
    .collect();
  for (policy, rate) in ALL_POLICIES.iter().zip(&rates) {
    assert!((rate - rates[0]).abs() < 1e-12, "{policy:?} gave {rate}");
  }
}

#[test]
fn robust_never_contradicts_strict_compat() {
  // The central guarantee: where a spreadsheet has an answer, we return it.
  let cases: [(&[&str], &[f64]); 4] = [
    (
      &["2015-01-01", "2016-01-01", "2017-01-01", "2018-01-01"],
      &[-1000.0, 3000.0, -2500.0, 600.0],
    ),
    (
      &["2020-01-01", "2021-01-01", "2022-01-01"],
      &[-100.0, 230.0, -132.0],
    ),
    (
      &["2020-01-01", "2021-01-01", "2022-01-01"],
      &[-1000.0, 750.0, 500.0],
    ),
    (&["1990-01-01", "2020-01-01"], &[-1000.0, 50000.0]),
  ];

  for (iso, amounts) in cases {
    let d = dates(iso);
    let strict = xirr(&d, amounts, None, None, Some(RootPolicy::SpreadsheetCompat)).unwrap();
    let robust = xirr(&d, amounts, None, None, None).unwrap();
    if strict.is_finite() {
      assert!(
        (strict - robust).abs() < 1e-12,
        "{iso:?}: strict {strict} vs robust {robust}"
      );
    }
  }
}

#[test]
fn lowest_policy_returns_the_smallest_enumerated_root() {
  let d = annual(4);
  let a = [-1000.0, 3000.0, -2500.0, 600.0];
  let roots = xirr_all_roots(&d, &a, None).unwrap();
  assert!(roots.len() > 1, "test needs a multiple-root flow");

  let lowest = xirr(&d, &a, None, None, Some(RootPolicy::Lowest)).unwrap();
  assert!(
    (lowest - roots[0]).abs() < 1e-12,
    "{lowest} vs {}",
    roots[0]
  );
}

#[test]
fn guess_steers_closest_to_guess_but_not_the_default() {
  let d = annual(3);
  let a = [-100.0, 230.0, -132.0];
  let roots = xirr_all_roots(&d, &a, None).unwrap();
  assert_eq!(roots.len(), 2);

  for (guess, expected) in [(0.0, roots[0]), (0.5, roots[1])] {
    let picked = xirr(&d, &a, Some(guess), None, Some(RootPolicy::ClosestToGuess)).unwrap();
    assert!((picked - expected).abs() < 1e-9, "guess {guess}: {picked}");
  }
}

#[test]
fn all_roots_are_sorted_deduplicated_and_real() {
  let d = annual(4);
  let a = [-1000.0, 3000.0, -2500.0, 600.0];
  let roots = xirr_all_roots(&d, &a, None).unwrap();
  let gross: f64 = a.iter().map(|x| x.abs()).sum();

  assert!(
    roots.windows(2).all(|w| w[0] < w[1]),
    "not sorted: {roots:?}"
  );
  for r in &roots {
    assert!(*r > -1.0, "root {r} outside the domain");
    let residual = xnpv(*r, &d, &a, None).unwrap();
    assert!(
      residual.abs() < 1e-6 * gross,
      "root {r} residual {residual}"
    );
  }
}

// ===========================================================================
// 5. Determinism
// ===========================================================================

#[test]
fn repeated_calls_are_bit_identical() {
  // No RNG, no iteration-order dependence, no global state.
  let d = annual(4);
  let a = [-1000.0, 3000.0, -2500.0, 600.0];
  let first = solve(&d, &a);
  for _ in 0..50 {
    assert_eq!(solve(&d, &a).to_bits(), first.to_bits());
  }
}

// ===========================================================================
// 6. Day count conventions
//
// The spreadsheet-parity guarantee only covers the default (ACT/365F).
// Other conventions must still produce a valid root - just a different one.
// ===========================================================================

#[test]
fn non_default_day_counts_still_produce_true_roots() {
  use xirr_core::DayCount;

  let d = dates(&["2020-01-15", "2021-03-20", "2022-07-04"]);
  let a = [-1000.0, 400.0, 800.0];

  let conventions = [
    DayCount::ACT_365F,
    DayCount::ACT_360,
    DayCount::THIRTY_E_360,
    DayCount::NL_365,
  ];

  let mut seen = Vec::new();
  for dc in conventions {
    let rate = xirr(&d, &a, None, Some(dc), None).unwrap();
    assert!(rate.is_finite(), "{dc:?} returned no rate");
    let residual = xnpv(rate, &d, &a, Some(dc)).unwrap();
    assert!(residual.abs() < 1e-9 * 2200.0, "{dc:?} residual {residual}");
    seen.push(rate);
  }

  // ACT/360 discounts over a shorter year, so it must not coincide with
  // ACT/365F - if it did, the convention was being ignored.
  assert!(
    (seen[0] - seen[1]).abs() > 1e-6,
    "ACT/365F and ACT/360 should differ: {seen:?}"
  );
}
