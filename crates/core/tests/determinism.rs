//! Determinism: the answer must not depend on the machine.
//!
//! Run with `cargo test -p xirr-core --test determinism`. CI runs it on
//! glibc, macOS and MSVC — see `.github/workflows/determinism.yml`.
//!
//! # Why this is not a bit-for-bit test across platforms
//!
//! IEEE 754 specifies `+ - * /` and `sqrt` exactly. It does **not** specify
//! `exp`, `ln`, `expm1` or `powf`, and libm implementations are permitted to
//! differ by an ULP or so on them. The log-rate solver uses all four, so
//! demanding identical bit patterns on every platform would be asserting
//! something the language and the hardware do not promise, and the test would
//! fail for a reason nobody could fix.
//!
//! What is asserted instead, and what each part buys:
//!
//! 1. **Reference rates to `CROSS_PLATFORM_REL_TOL`.** A platform whose libm
//!    puts the answer further away than this has a real problem, not a
//!    rounding difference.
//! 2. **Bit-for-bit stability within a platform.** No RNG, no iteration-order
//!    dependence, no global state, no dependence on how many times the
//!    function has already been called. This *is* exact, because on one
//!    machine the same libm runs every time.
//! 3. **Order independence.** Reordering a cash flow that is already valid
//!    must not move the answer beyond the same tolerance.

use std::str::FromStr;

use xirr_core::{xirr, xirr_all_roots, xnpv, DateLike, RootPolicy};

/// How far a rate may move between two libm implementations.
///
/// Judgement call, not a measurement: at the time of writing the suite is only
/// exercised on one platform, so this is set from the error model rather than
/// from observed divergence. Brent stops when the bracket is narrower than
/// `2e-14 + 8.9e-16·|u|`, and one ULP of error in `G` moves the root by about
/// `ULP/|G'|`. Both are comfortably below `1e-12` for every case here, so
/// `1e-11` leaves two orders of magnitude of headroom while still being three
/// orders tighter than any reporting precision.
///
/// If a platform ever fails this, the correct response is to record the
/// observed divergence here with the platform named — not to widen the
/// tolerance until it passes.
const CROSS_PLATFORM_REL_TOL: f64 = 1e-11;

fn flow(rows: &[(&str, f64)]) -> (Vec<DateLike>, Vec<f64>) {
  (
    rows
      .iter()
      .map(|(d, _)| DateLike::from_str(d).unwrap())
      .collect(),
    rows.iter().map(|(_, a)| *a).collect(),
  )
}

/// One case per code path, so a divergence localises itself.
///
/// Every expected rate here was computed **outside this library**: by 60-digit
/// bisection on `G(u) = Σ aᵢ·exp(-u·δᵢ)`, cross-checked against a closed form
/// where one exists (`1.001^365 - 1` for the one-day case, `1000^(-365/366)-1`
/// for near-total-loss). A reference value taken from the implementation it is
/// meant to police tests nothing.
#[allow(clippy::type_complexity)]
fn reference_cases() -> Vec<(&'static str, (Vec<DateLike>, Vec<f64>), f64)> {
  vec![
    (
      "conventional, spreadsheet path",
      flow(&[
        ("2020-01-01", -1000.),
        ("2021-01-01", 750.),
        ("2022-01-01", 500.),
      ]),
      0.175_009_264_615_451,
    ),
    (
      "multiple roots, spreadsheet path",
      flow(&[
        ("2015-01-01", -1000.),
        ("2016-01-01", 3000.),
        ("2017-01-01", -2500.),
        ("2018-01-01", 600.),
      ]),
      -0.571_885_951_525_731,
    ),
    (
      "near total loss, robust path",
      flow(&[("2020-01-01", -1000.), ("2021-01-01", 1.)]),
      -0.998_980_947_118_578_1,
    ),
    (
      "century horizon, spreadsheet path",
      flow(&[("1925-01-01", -1000.), ("2025-01-01", 1_000_000.)]),
      0.071_468_643_922_357,
    ),
    (
      "one day horizon",
      flow(&[("2020-01-01", -1000.), ("2020-01-02", 1001.)]),
      0.440_251_313_429_578_35,
    ),
  ]
}

/// The reference series from the robust-solver suite, whose roots sit far
/// above the range any rate-space search can reach.
fn extreme_series(outflow: f64) -> (Vec<DateLike>, Vec<f64>) {
  let mut dates = vec![DateLike::from_str("2023-01-01").unwrap(); 2];
  let mut amounts = vec![-outflow, 20_000.0];
  for month in 2..=12 {
    dates.push(DateLike::from_str(&format!("2023-{month:02}-01")).unwrap());
    amounts.push(20_000.0);
  }
  dates.push(DateLike::from_str("2023-12-31").unwrap());
  amounts.push(0.0);
  (dates, amounts)
}

#[test]
fn reference_rates_hold_on_this_platform() {
  for (name, (dates, amounts), expected) in reference_cases() {
    let rate = xirr(&dates, &amounts, None, None, None).unwrap();
    let error = ((rate - expected) / expected).abs();
    println!("{name}: {rate:.17e} (rel {error:.2e})");
    assert!(
      error < CROSS_PLATFORM_REL_TOL,
      "{name}: got {rate:.17e}, want {expected:.17e}, rel {error:.2e}"
    );
  }
}

#[test]
fn extreme_rates_hold_on_this_platform() {
  // These exercise `exp`, `ln` and `expm1` at the far end of their range,
  // where libm implementations are most likely to differ.
  for (outflow, expected) in [
    (20_000.000_000_000_004_f64, 2.127_188_975_089_368_8e185),
    (20_001.0, 4.383_552_505_094_086e50),
    (20_382.0, 2.398_246_125_029_901_3e20),
    (30_000.0, 560_622.093_926_819_3),
  ] {
    let (dates, amounts) = extreme_series(outflow);
    let rate = xirr(&dates, &amounts, None, None, None).unwrap();
    let error = ((rate - expected) / expected).abs();
    println!("outflow {outflow}: {rate:.17e} (rel {error:.2e})");
    assert!(
      error < CROSS_PLATFORM_REL_TOL,
      "outflow {outflow}: got {rate:.17e}, want {expected:.17e}, rel {error:.2e}"
    );
  }
}

#[test]
fn repeated_calls_are_bit_identical_on_one_platform() {
  // Exact, unlike the cross-platform assertions: the same libm runs each time.
  let mut cases: Vec<(Vec<DateLike>, Vec<f64>)> =
    reference_cases().into_iter().map(|c| c.1).collect();
  cases.push(extreme_series(20_001.0));
  cases.push(extreme_series(11_000.0)); // provably rootless

  for (dates, amounts) in cases {
    for policy in [
      RootPolicy::SpreadsheetCompat,
      RootPolicy::SpreadsheetThenRobust,
      RootPolicy::Lowest,
      RootPolicy::ClosestToGuess,
    ] {
      let first = xirr(&dates, &amounts, None, None, Some(policy)).unwrap();
      let first_roots = xirr_all_roots(&dates, &amounts, None).unwrap();
      for _ in 0..25 {
        let again = xirr(&dates, &amounts, None, None, Some(policy)).unwrap();
        assert_eq!(again.to_bits(), first.to_bits(), "{policy:?} drifted");
        assert_eq!(
          xirr_all_roots(&dates, &amounts, None).unwrap(),
          first_roots,
          "{policy:?} enumeration drifted"
        );
      }
    }
  }
}

#[test]
fn interleaving_calls_does_not_change_any_of_them() {
  // Catches shared mutable state and lazily-initialised caches: solving A
  // between two solves of B must not move B.
  let a = flow(&[
    ("2020-01-01", -1000.),
    ("2021-01-01", 750.),
    ("2022-01-01", 500.),
  ]);
  let b = extreme_series(20_001.0);

  let a_alone = xirr(&a.0, &a.1, None, None, None).unwrap();
  let b_alone = xirr(&b.0, &b.1, None, None, None).unwrap();

  for _ in 0..20 {
    assert_eq!(
      xirr(&a.0, &a.1, None, None, None).unwrap().to_bits(),
      a_alone.to_bits()
    );
    assert_eq!(
      xirr(&b.0, &b.1, None, None, None).unwrap().to_bits(),
      b_alone.to_bits()
    );
  }
}

#[test]
fn reordering_a_valid_flow_does_not_move_the_answer() {
  // Dates before `dates[0]` are rejected by design, so the reference date is
  // held fixed and only the tail is permuted. The netted series is identical
  // under permutation, so only summation order differs.
  let base = [
    ("2020-01-01", -1000.0),
    ("2020-07-01", 200.0),
    ("2021-01-01", 300.0),
    ("2021-07-01", 250.0),
    ("2022-01-01", 400.0),
  ];
  let reference = {
    let (d, a) = flow(&base);
    xirr(&d, &a, None, None, None).unwrap()
  };

  // Every rotation of the tail, reference date left in place.
  for shift in 1..base.len() {
    let mut rows = vec![base[0]];
    for i in 0..base.len() - 1 {
      rows.push(base[1 + (i + shift) % (base.len() - 1)]);
    }
    let (d, a) = flow(&rows);
    let rate = xirr(&d, &a, None, None, None).unwrap();
    let error = ((rate - reference) / reference).abs();
    assert!(
      error < CROSS_PLATFORM_REL_TOL,
      "shift {shift}: got {rate:.17e}, want {reference:.17e}"
    );
  }
}

#[test]
fn the_audit_check_reproduces_on_this_platform() {
  // The reproducibility contract: a third party re-runs `xnpv` on a rate we
  // published and reaches the same verdict. If `powf` differs enough between
  // platforms to flip this, callers cannot verify our output and the contract
  // is broken.
  for (name, (dates, amounts), _) in reference_cases() {
    let rate = xirr(&dates, &amounts, None, None, None).unwrap();
    let absolute: Vec<f64> = amounts.iter().map(|a| a.abs()).collect();
    let gross = absolute.iter().sum::<f64>().max(1.0);
    let discounted = xnpv(rate, &dates, &absolute, None).unwrap();
    let residual = xnpv(rate, &dates, &amounts, None).unwrap().abs();

    let budget = 1e-9 * gross.max(discounted);
    println!("{name}: |XNPV| = {residual:.3e}, budget {budget:.3e}");
    assert!(residual <= budget, "{name}: {residual:.3e} > {budget:.3e}");
  }
}
