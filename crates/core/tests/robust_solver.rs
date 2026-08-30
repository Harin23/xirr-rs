//! The robust (log-rate space) solver: existence, reachability, typed
//! outcomes, and the parity regression gate.
//!
//! Run with `cargo test -p xirr-core --test robust_solver`.
//!
//! # The series these tests are built on
//!
//! ```text
//! 2023-01-01  -outflow          <- varied per case
//! 2023-01-01  +20000            <- nets with the line above
//! 2023-02-01 .. 2023-12-01  +20000 each (11 payments, on the 1st)
//! 2023-12-31       0
//! ```
//!
//! Netting the two day-zero lines is the whole point: the *netted* day-zero
//! position is `20000 - outflow`, and its sign - not the sign of either raw
//! amount - decides whether a root exists at all. Every reference rate below
//! was computed independently at 80 decimal digits (Newton on
//! `G(u) = Σ aᵢ·exp(-u·δᵢ)`) and then rounded to `f64`, so these constants
//! test the library rather than restating it.

use std::{collections::BTreeMap, fs, path::PathBuf, str::FromStr, time::Instant};

use xirr_core::{
  sign_changes, xirr, xirr_all_roots, xirr_outcome, xnpv, DateLike, DayCount, RootPolicy,
  XirrOutcome, DISTINCT_ROOT_TOL, MAX_SEARCHED_LOG_RATE, MIN_SEARCHED_LOG_RATE,
};

// ---------------------------------------------------------------------------
// The series, and its exact roots
// ---------------------------------------------------------------------------

fn series(outflow: f64) -> (Vec<DateLike>, Vec<f64>) {
  let mut dates = vec![
    DateLike::from_str("2023-01-01").unwrap(),
    DateLike::from_str("2023-01-01").unwrap(),
  ];
  let mut amounts = vec![-outflow, 20_000.0];
  for month in 2..=12 {
    dates.push(DateLike::from_str(&format!("2023-{month:02}-01")).unwrap());
    amounts.push(20_000.0);
  }
  dates.push(DateLike::from_str("2023-12-31").unwrap());
  amounts.push(0.0);
  (dates, amounts)
}

/// `nextafter(20000.0, +inf)`: the smallest `f64` outflow that leaves the
/// netted day-zero position strictly negative, and therefore the smallest one
/// for which a root exists at all. Asserted rather than assumed below.
const SMALLEST_SOLVABLE_OUTFLOW: f64 = 20_000.000_000_000_004;

/// `(outflow, exact root)`. Reference values from the 80-digit solve.
const SOLVABLE: [(f64, f64); 10] = [
  (SMALLEST_SOLVABLE_OUTFLOW, 2.127_188_975_089_368_8e185),
  (20_001.0, 4.383_552_505_094_086e50),
  (20_382.0, 2.398_246_125_029_901_3e20),
  (20_383.0, 2.327_295_108_821_615_2e20),
  (20_500.0, 1.099_593_027_181_703_6e19),
  (21_000.0, 4_428_343_029_082_674.5),
  (25_000.0, 232_604_535.368_946_94),
  (30_000.0, 560_622.093_926_819_3),
  (40_000.0, 4_378.314_245_452_634),
  (100_000.0, 10.344_994_309_393_813),
];

fn relative_error(got: f64, want: f64) -> f64 {
  ((got - want) / want).abs()
}

/// The audit condition exactly as the brief states it: `RESIDUAL_REL_TOL` x
/// the gross size of the cash flow. Correct for every non-negative rate,
/// because discounting can only shrink the terms there.
fn strict_residual_budget(amounts: &[f64]) -> f64 {
  1e-9 * amounts.iter().map(|a| a.abs()).sum::<f64>().max(1.0)
}

/// The general audit condition, which also covers rates near total loss.
///
/// At `r = -0.998` over nine years the discount factors reach ~1e26, so
/// `XNPV` is a difference of terms around 1e30 and the smallest residual an
/// `f64` can express is about 1e14. Judging that against the *undiscounted*
/// gross size demands a precision the representation does not have, and no
/// solver can meet it. The scale that matters is the size of the sum being
/// formed, which `xnpv` over the absolute amounts reports directly.
fn residual_budget(rate: f64, dates: &[DateLike], amounts: &[f64]) -> f64 {
  let absolute: Vec<f64> = amounts.iter().map(|a| a.abs()).collect();
  let discounted = xnpv(rate, dates, &absolute, None).unwrap();
  strict_residual_budget(amounts).max(1e-9 * discounted)
}

// ===========================================================================
// 1-2. No root exists, and it is said out loud
// ===========================================================================

#[test]
fn an_outflow_smaller_than_the_day_zero_inflow_has_no_root() {
  // Acceptance 1. Netted day zero is +9000 and every later flow is positive,
  // so XNPV >= 9000 everywhere. `validate_positive_negative` accepts this
  // input because the *raw* amounts contain both signs; the netted existence
  // test is what catches it.
  let (dates, amounts) = series(11_000.0);
  assert_eq!(sign_changes(&dates, &amounts, None).unwrap(), 0);

  assert_eq!(
    xirr_outcome(&dates, &amounts, None, None, None).unwrap(),
    XirrOutcome::NoRootExists
  );
  assert!(xirr_all_roots(&dates, &amounts, None).unwrap().is_empty());

  // XNPV really is bounded away from zero across the whole domain.
  for u in [-30.0f64, -5.0, 0.0, 5.0, 50.0, 400.0, 700.0] {
    let rate = u.exp_m1();
    assert!(
      xnpv(rate, &dates, &amounts, None).unwrap() >= 9_000.0,
      "u={u} gave a residual below the netted day-zero position"
    );
  }
}

#[test]
fn a_netted_day_zero_of_exactly_zero_has_no_root_and_no_underflow_artifact() {
  // Acceptance 2. This is the case that produces a spurious root in a naive
  // log-space search: at large u every discount factor underflows to exactly
  // 0.0, the sum evaluates to 0.0, and bisection reads that as a root.
  let (dates, amounts) = series(20_000.0);
  assert_eq!(sign_changes(&dates, &amounts, None).unwrap(), 0);

  assert_eq!(
    xirr_outcome(&dates, &amounts, None, None, None).unwrap(),
    XirrOutcome::NoRootExists
  );
  assert!(xirr_all_roots(&dates, &amounts, None).unwrap().is_empty());

  for policy in [
    RootPolicy::SpreadsheetCompat,
    RootPolicy::SpreadsheetThenRobust,
    RootPolicy::Lowest,
    RootPolicy::ClosestToGuess,
  ] {
    let rate = xirr(&dates, &amounts, None, None, Some(policy)).unwrap();
    assert!(rate.is_nan(), "{policy:?} invented a rate: {rate}");
  }

  // The artifact would land somewhere past u ~ 8800, which is far outside the
  // searched domain in any case; assert the underlying objective never
  // actually reaches zero for a representable rate.
  for u in [100.0f64, 400.0, MAX_SEARCHED_LOG_RATE] {
    let residual = xnpv(u.exp_m1(), &dates, &amounts, None).unwrap();
    assert!(residual > 0.0, "u={u} residual {residual} reached zero");
  }
}

// ===========================================================================
// 3-4. Roots far above the old MAX_SEARCHED_RATE ceiling
// ===========================================================================

#[test]
fn the_boundary_outflow_is_the_one_this_test_thinks_it_is() {
  // Guards the constant above: if `nextafter` moved, the "smallest solvable
  // outflow" case would silently become an ordinary one.
  assert_eq!(SMALLEST_SOLVABLE_OUTFLOW.to_bits(), 0x40d3_8800_0000_0001);
  assert!(SMALLEST_SOLVABLE_OUTFLOW > 20_000.0);
  assert_eq!(
    20_000.0f64.to_bits() + 1,
    SMALLEST_SOLVABLE_OUTFLOW.to_bits()
  );
}

#[test]
fn every_solvable_outflow_returns_its_root() {
  // Acceptance 3 and 4. Rows from 20382 up were already reachable through the
  // spreadsheet path; 20001 and the nextafter row are the ones the bounded
  // rate search could not express at any setting, because 1e12 is u ~ 27.6
  // and these roots live at u = 116.6 and u = 426.7.
  for (outflow, expected) in SOLVABLE {
    let (dates, amounts) = series(outflow);

    assert_eq!(
      sign_changes(&dates, &amounts, None).unwrap(),
      1,
      "outflow {outflow}: expected exactly one netted sign change"
    );

    let rate = xirr(&dates, &amounts, None, None, None).unwrap();
    assert!(
      relative_error(rate, expected) < 1e-9,
      "outflow {outflow}: got {rate:e}, want {expected:e} (rel {:e})",
      relative_error(rate, expected)
    );

    // The typed surface must agree with the plain one, exactly.
    let outcome = xirr_outcome(&dates, &amounts, None, None, None).unwrap();
    assert_eq!(outcome, XirrOutcome::Root(rate));
    assert_eq!(outcome.root_count(), 1);
    assert!(!outcome.is_ambiguous());

    // Descartes says one sign change means one root; enumeration must agree.
    // Not bit-for-bit: `xirr` may have come from the spreadsheet's Newton
    // iteration in rate space and the enumeration from Brent in log-rate
    // space, and two correct algorithms land on adjacent f64s.
    let roots = xirr_all_roots(&dates, &amounts, None).unwrap();
    assert_eq!(roots.len(), 1, "outflow {outflow}: {roots:?}");
    assert!(
      relative_error(roots[0], expected) < 1e-9,
      "outflow {outflow}: enumeration gave {:e}, want {expected:e}",
      roots[0]
    );
  }
}

#[test]
fn xirr_and_xirr_all_roots_never_disagree() {
  // Before the log-space rewrite these two functions used different search
  // spaces with different ceilings, so `xirr` returned 2.4e20 for an outflow
  // of 20382 while `xirr_all_roots` returned an empty vector for the same
  // input. Any rate `xirr` produces must appear in the enumeration.
  for (outflow, _) in SOLVABLE {
    let (dates, amounts) = series(outflow);
    let rate = xirr(&dates, &amounts, None, None, None).unwrap();
    let roots = xirr_all_roots(&dates, &amounts, None).unwrap();
    assert!(
      roots.iter().any(|r| relative_error(*r, rate) < 1e-9),
      "outflow {outflow}: xirr returned {rate:e}, enumeration returned {roots:?}"
    );
  }
}

// ===========================================================================
// 5. Every returned rate is verifiable by the caller
// ===========================================================================

#[test]
fn every_returned_rate_passes_the_documented_audit_check() {
  // Acceptance 5. This is the reproducibility contract: a caller re-runs
  // `xnpv` on the rate we handed them and gets a residual inside the
  // published tolerance. It must hold at 1e185 as well as at 10%.
  for (outflow, _) in SOLVABLE {
    let (dates, amounts) = series(outflow);
    let rate = xirr(&dates, &amounts, None, None, None).unwrap();
    let residual = xnpv(rate, &dates, &amounts, None).unwrap().abs();
    let budget = strict_residual_budget(&amounts);
    assert!(
      residual <= budget,
      "outflow {outflow}: |XNPV({rate:e})| = {residual:e} exceeds {budget:e}"
    );
  }
}

// ===========================================================================
// 6. Denomination independence
// ===========================================================================

#[test]
fn rescaling_by_a_power_of_two_is_exactly_invariant() {
  // Acceptance 6, the half that can be stated as an equality.
  //
  // Multiplying a f64 by a power of two is exact, so every product, every
  // partial sum and the gross size all scale by exactly the same factor.
  // Brent's decisions depend on signs and on abscissae in u, neither of which
  // moves. The answer is therefore not merely close - it is the same f64, and
  // asserting anything weaker would let a real regression through.
  let (dates, amounts) = series(100_000.0);
  let reference = xirr(&dates, &amounts, None, None, None).unwrap();

  for exponent in [-10i32, -4, 0, 4, 10, 20] {
    let k = 2f64.powi(exponent);
    let scaled: Vec<f64> = amounts.iter().map(|a| a * k).collect();
    let rate = xirr(&dates, &scaled, None, None, None).unwrap();
    assert_eq!(
      rate.to_bits(),
      reference.to_bits(),
      "2^{exponent} gave {rate}, 2^0 gave {reference}"
    );
  }
}

#[test]
fn rescaling_by_a_decimal_factor_is_invariant_to_within_rounding() {
  // Acceptance 6, the half that cannot.
  //
  // A decimal factor is not exact in binary, so `-100000e-9 + 20000e-9` is
  // -8.000000000000001e-05 where `-80000 * 1e-9` is -8e-05. The netted input
  // differs by one ULP *before any solving happens*, so bit-identical output
  // is unattainable by any solver in any space. What must hold is that the
  // difference stays at rounding level rather than growing.
  let (dates, amounts) = series(100_000.0);
  let reference = xirr(&dates, &amounts, None, None, None).unwrap();

  for exponent in [-9i32, -6, -3, 0, 3, 6, 9, 12] {
    let k = 10f64.powi(exponent);
    let scaled: Vec<f64> = amounts.iter().map(|a| a * k).collect();
    let rate = xirr(&dates, &scaled, None, None, None).unwrap();
    assert!(
      relative_error(rate, reference) < 1e-12,
      "1e{exponent} gave {rate}, 1e0 gave {reference}"
    );
  }
}

#[test]
fn a_cancelling_flow_is_stable_to_its_own_conditioning_and_no_better() {
  // Acceptance 6, the honest half.
  //
  // For outflow 20001 the netted day-zero position is `-20001 + 20000 = -1`,
  // a subtraction of two numbers agreeing in four significant figures. Scale
  // the *inputs* by 1e-6 first and that sum becomes -9.999999999975306e-07,
  // not -1e-6: the input has changed by 2.5e-12 relative before any solving
  // happens. Since the root satisfies roughly `ln|b1| = -u.d2 + const`, that
  // perturbation shows up in the rate divided by d2 = 0.0849, i.e. ~3e-11.
  //
  // Bit-identical output is therefore not attainable here by any solver, in
  // any space, at any tolerance. Asserting it would be asserting something
  // false about floating point. What we can and do require is that the error
  // stays at the level the input conditioning dictates.
  const CONDITIONING_LIMIT: f64 = 1e-9;

  let (dates, amounts) = series(20_001.0);
  let reference = xirr(&dates, &amounts, None, None, None).unwrap();

  for exponent in [-6i32, -3, 0, 3, 9] {
    let k = 10f64.powi(exponent);
    let scaled: Vec<f64> = amounts.iter().map(|a| a * k).collect();
    let rate = xirr(&dates, &scaled, None, None, None).unwrap();
    assert!(
      relative_error(rate, reference) < CONDITIONING_LIMIT,
      "1e{exponent} gave {rate:e}, 1e0 gave {reference:e}"
    );
  }
}

// ===========================================================================
// 7. The parity regression gate
// ===========================================================================

/// The platform the snapshot was recorded on, where bit-identity is required.
///
/// `target_env` matters: musl ships its own libm, so an x86-64 Linux musl
/// build is no more the recording platform than macOS is.
const SNAPSHOT_PLATFORM: bool = cfg!(all(
  target_arch = "x86_64",
  target_os = "linux",
  target_env = "gnu"
));

/// How far the compat rate may move between libm implementations before it
/// stops being a rounding difference and becomes a parity break.
///
/// `exp`, `ln`, `expm1` and `powf` are not bit-specified by IEEE 754, so the
/// final Brent iterate is free to land a few ULP apart on different libms.
/// Observed across the 445 snapshot rows, recorded per the instruction in
/// `determinism.rs` - name the platform, do not widen until it passes:
///
/// | platform             | max divergence |
/// | -------------------- | -------------- |
/// | linux x86-64 glibc   | 0 ULP (exact)  |
/// | macos aarch64        | 5 ULP          |
/// | windows x86-64 msvc  | 0 ULP (exact)  |
///
/// 16 ULP is about `2e-15` relative at these magnitudes: four orders tighter
/// than `CROSS_PLATFORM_REL_TOL` in `determinism.rs`, and far below the
/// orders of magnitude a real parity break - a different root selected, a
/// changed policy - moves the answer. Bit-identity is still required on the
/// recording platform, so this budget only applies where libm genuinely
/// differs.
const PARITY_ULP_BUDGET: u64 = 16;

/// Number of representable `f64` values between `a` and `b`.
fn ulp_distance(a: f64, b: f64) -> u64 {
  // Map each f64 onto an i64 that sorts the same way, so subtraction counts
  // representable values and crossing zero is not a discontinuity.
  fn ordered(x: f64) -> i64 {
    let bits = x.to_bits() as i64;
    if bits < 0 {
      i64::MIN.wrapping_sub(bits)
    } else {
      bits
    }
  }
  ordered(a).wrapping_sub(ordered(b)).unsigned_abs()
}

/// Pins `ulp_distance` against the divergence actually observed on
/// macos-arm64, so the budget above is checked by something executable rather
/// than by a comment.
///
/// These four pairs are the CI failure that motivated the budget, verbatim.
/// The parity test's tolerant branch does not run on the recording platform,
/// so without this the helper would ship untested from Linux.
#[test]
fn the_ulp_helper_measures_the_observed_macos_divergence() {
  // (got on macos-arm64, want from the snapshot, ULP apart)
  for (got, want, expected) in [
    (0x3fd3_23ef_f332_0b09u64, 0x3fd3_23ef_f332_0b0cu64, 3u64),
    (0x3f8a_7f34_7ed5_8212, 0x3f8a_7f34_7ed5_8217, 5),
    (0xbfd0_9f93_ac90_a61c, 0xbfd0_9f93_ac90_a61b, 1),
    (0xbfce_f0c6_3bdc_c526, 0xbfce_f0c6_3bdc_c524, 2),
  ] {
    let (got, want) = (f64::from_bits(got), f64::from_bits(want));
    assert_eq!(ulp_distance(got, want), expected, "{got:?} vs {want:?}");
    assert_eq!(ulp_distance(want, got), expected, "not symmetric");
    assert!(
      expected <= PARITY_ULP_BUDGET,
      "budget no longer covers macOS"
    );
  }

  // Crossing zero must not read as a huge jump, and a value must be zero ULP
  // from itself. Both signs of zero are the same number here.
  assert_eq!(ulp_distance(0.0, -0.0), 0);
  assert_eq!(ulp_distance(0.299, 0.299), 0);
  // The smallest subnormal either side of zero is two representable values
  // apart, not the 2^53 a naive sign-magnitude subtraction would report.
  // `MIN_POSITIVE` would be wrong here: it is the smallest *normal*, so every
  // subnormal still sits between it and its negation.
  let tiny = f64::from_bits(1);
  assert_eq!(ulp_distance(tiny, -tiny), 2);
}

/// `SpreadsheetCompat` output must be **bit-identical** to commit `473b9ff`
/// for every golden case and every guess in the snapshot, on the platform the
/// snapshot was recorded on - and within `PARITY_ULP_BUDGET` everywhere else.
///
/// The fixture was produced by running the pre-change implementation, not by
/// running this one, so it cannot drift with the code it protects. Raw `f64`
/// bit patterns rather than decimal, because a decimal round-trip would hide
/// exactly the one-ULP divergences that break parity on multiple-root flows.
///
/// Bit-identity is not assertable across platforms - see `PARITY_ULP_BUDGET`
/// and the module doc of `determinism.rs` for why demanding it would be
/// asserting a promise neither the language nor the hardware makes. A `NaN`
/// row is exempt from the budget and must stay `NaN`: reproducing `#NUM!` is
/// the point of those rows, and a `NaN` that became a number is a behaviour
/// change rather than a rounding difference.
#[test]
fn spreadsheet_compat_is_unchanged_from_473b9ff() {
  let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
  let snapshot = fs::read_to_string(root.join("tests/fixtures/spreadsheet_compat_473b9ff.csv"))
    .expect("parity snapshot missing");

  let mut cases: BTreeMap<String, (Vec<DateLike>, Vec<f64>)> = BTreeMap::new();
  let raw = fs::read_to_string(root.join("../../__test__/golden/cases.csv"))
    .expect("golden fixtures not found; run scripts/build_corpus.py");
  for line in raw.lines().skip(1).filter(|l| !l.trim().is_empty()) {
    let f: Vec<&str> = line.trim_end_matches('\r').split(',').collect();
    let entry = cases
      .entry(f[0].to_string())
      .or_insert_with(|| (Vec::new(), Vec::new()));
    entry.0.push(DateLike::from_str(f[1]).unwrap());
    entry.1.push(f[2].parse().unwrap());
  }

  let mut checked = 0usize;
  let mut divergences = Vec::new();
  let mut tolerated = 0u64;

  for line in snapshot.lines().skip(1).filter(|l| !l.trim().is_empty()) {
    let f: Vec<&str> = line.trim_end_matches('\r').split(',').collect();
    let (id, guess_field, want_bits) = (f[0], f[1], u64::from_str_radix(f[2], 16).unwrap());
    let Some((dates, amounts)) = cases.get(id) else {
      panic!("snapshot references unknown case {id}");
    };
    let guess = (guess_field != "default").then(|| guess_field.parse::<f64>().unwrap());

    let got = xirr(
      dates,
      amounts,
      guess,
      None,
      Some(RootPolicy::SpreadsheetCompat),
    )
    .unwrap();

    checked += 1;
    if got.to_bits() == want_bits {
      continue;
    }

    let want = f64::from_bits(want_bits);
    let drift = if got.is_nan() && want.is_nan() {
      0 // a differing NaN payload is not a differing answer
    } else if got.is_finite() && want.is_finite() {
      ulp_distance(got, want)
    } else {
      u64::MAX // NaN <-> number, or an infinity appearing: a real change
    };

    if SNAPSHOT_PLATFORM || drift > PARITY_ULP_BUDGET {
      divergences.push(format!(
        "{id} guess={guess_field}: {got:?} (0x{:016x}), was 0x{want_bits:016x} ({drift} ULP)",
        got.to_bits()
      ));
    } else {
      tolerated = tolerated.max(drift);
    }
  }

  assert!(checked > 400, "thin snapshot: only {checked} rows");
  assert!(
    divergences.is_empty(),
    "{} parity break(s) against 473b9ff:\n  {}",
    divergences.len(),
    divergences.join("\n  ")
  );
  // Printed so the budget table above can be kept honest on a new platform.
  if tolerated > 0 {
    println!(
      "libm divergence tolerated: {tolerated} ULP over {checked} rows (budget {PARITY_ULP_BUDGET})"
    );
  }
}

#[test]
fn the_robust_path_never_overrides_a_finite_spreadsheet_answer() {
  // The other half of the parity contract: Phase 2 may add an answer, never
  // change one. Checked across the whole corpus rather than a sample.
  let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
  let raw = fs::read_to_string(root.join("../../__test__/golden/cases.csv")).unwrap();
  let mut cases: BTreeMap<String, (Vec<DateLike>, Vec<f64>)> = BTreeMap::new();
  for line in raw.lines().skip(1).filter(|l| !l.trim().is_empty()) {
    let f: Vec<&str> = line.trim_end_matches('\r').split(',').collect();
    let entry = cases
      .entry(f[0].to_string())
      .or_insert_with(|| (Vec::new(), Vec::new()));
    entry.0.push(DateLike::from_str(f[1]).unwrap());
    entry.1.push(f[2].parse().unwrap());
  }

  for (id, (dates, amounts)) in &cases {
    let strict = xirr(
      dates,
      amounts,
      None,
      None,
      Some(RootPolicy::SpreadsheetCompat),
    )
    .unwrap();
    if !strict.is_finite() {
      continue;
    }
    let default = xirr(dates, amounts, None, None, None).unwrap();
    assert_eq!(
      strict.to_bits(),
      default.to_bits(),
      "{id}: default policy returned {default} where the spreadsheet says {strict}"
    );
  }
}

// ===========================================================================
// 8. Performance
// ===========================================================================

#[test]
fn the_robust_path_stays_in_single_digit_milliseconds() {
  // Acceptance 8. The hardest case in the corpus: a root at u = 426.7, which
  // only the robust path can reach, so every call here pays the full Phase 1
  // failure plus the whole log-space search.
  //
  // The bound is build-dependent, so it is stated twice. The release figure
  // is the one the brief asks about; the debug figure exists so that running
  // `cargo test` without `--release` does not produce a spurious failure on a
  // loaded machine.
  let (dates, amounts) = series(SMALLEST_SOLVABLE_OUTFLOW);

  // Warm the caches so the first call's page faults are not the measurement.
  for _ in 0..5 {
    assert!(xirr(&dates, &amounts, None, None, None)
      .unwrap()
      .is_finite());
  }

  const RUNS: u32 = 50;
  let started = Instant::now();
  for _ in 0..RUNS {
    std::hint::black_box(xirr(&dates, &amounts, None, None, None).unwrap());
  }
  let per_call = started.elapsed().as_secs_f64() * 1e3 / f64::from(RUNS);

  let budget = if cfg!(debug_assertions) { 80.0 } else { 9.0 };
  println!("robust path: {per_call:.3} ms/call (budget {budget} ms)");
  assert!(
    per_call < budget,
    "robust path took {per_call:.3} ms/call, budget {budget} ms"
  );
}

#[test]
fn the_spreadsheet_path_is_not_slowed_down_by_the_existence_test() {
  // Netting is O(n log n) once per call. On a conventional flow that Phase 1
  // answers immediately, that must not become the dominant cost.
  let dates: Vec<DateLike> = (0..240)
    .map(|i| DateLike::from_str(&format!("{}-{:02}-01", 2000 + i / 12, i % 12 + 1)).unwrap())
    .collect();
  let mut amounts = vec![50.0; 240];
  amounts[0] = -8_000.0;

  for _ in 0..5 {
    assert!(xirr(&dates, &amounts, None, None, None)
      .unwrap()
      .is_finite());
  }
  const RUNS: u32 = 100;
  let started = Instant::now();
  for _ in 0..RUNS {
    std::hint::black_box(xirr(&dates, &amounts, None, None, None).unwrap());
  }
  let per_call = started.elapsed().as_secs_f64() * 1e3 / f64::from(RUNS);

  let budget = if cfg!(debug_assertions) { 40.0 } else { 3.0 };
  println!("spreadsheet path: {per_call:.3} ms/call (budget {budget} ms)");
  assert!(per_call < budget, "{per_call:.3} ms/call");
}

// ===========================================================================
// 9. Property test: one sign change means a root, always
// ===========================================================================

/// SplitMix64. Written out rather than pulled in as a dependency so the
/// generated corpus is identical on every platform, every toolchain and every
/// run - a property test that cannot be reproduced is not evidence.
struct Rng(u64);

impl Rng {
  fn next_u64(&mut self) -> u64 {
    self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = self.0;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
  }

  /// Uniform in `[lo, hi)`.
  fn uniform(&mut self, lo: f64, hi: f64) -> f64 {
    let unit = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
    lo + unit * (hi - lo)
  }

  fn below(&mut self, n: usize) -> usize {
    (self.next_u64() % n as u64) as usize
  }
}

#[test]
fn a_single_sign_change_always_yields_a_root() {
  // Acceptance 9. Conventional flows - outflows then inflows - across nine
  // orders of magnitude in amount and horizons from one week to sixty years.
  // Every one of these has exactly one root by Descartes, so a failure here
  // is a solver failure and nothing else.
  let mut rng = Rng(0x5EED_1234_ABCD_0001);
  let mut checked = 0usize;

  for trial in 0..2_000 {
    let payments = 2 + rng.below(14);
    let outflows = 1 + rng.below(payments - 1);
    let magnitude = 10f64.powf(rng.uniform(-4.0, 5.0));

    // Strictly increasing day offsets, so no netting collapses the flow.
    let mut day = 0i64;
    let mut dates = vec![DateLike::from(
      time::Date::from_calendar_date(2000, time::Month::January, 1).unwrap(),
    )];
    for _ in 1..payments {
      day += 1 + rng.below(1_500) as i64;
      dates.push(DateLike::from(
        time::Date::from_calendar_date(2000, time::Month::January, 1)
          .unwrap()
          .saturating_add(time::Duration::days(day)),
      ));
    }

    let amounts: Vec<f64> = (0..payments)
      .map(|i| {
        let size = magnitude * rng.uniform(0.1, 10.0);
        if i < outflows {
          -size
        } else {
          size
        }
      })
      .collect();

    // Skip flows that cannot have a root: all-out or all-in after netting,
    // and flows whose root is not representable (total loss beyond -1 + 2^-53
    // or a return beyond f64::MAX).
    if sign_changes(&dates, &amounts, None).unwrap() != 1 {
      continue;
    }
    let outside_domain = [MIN_SEARCHED_LOG_RATE, MAX_SEARCHED_LOG_RATE]
      .map(|u| xnpv(u.exp_m1(), &dates, &amounts, None).unwrap());
    if !outside_domain.iter().all(|v| v.is_finite()) || outside_domain[0] * outside_domain[1] > 0.0
    {
      continue;
    }

    checked += 1;
    let outcome = xirr_outcome(&dates, &amounts, None, None, None).unwrap();
    let rate = outcome.rate().unwrap_or_else(|| {
      panic!("trial {trial}: {outcome:?}\n  dates {dates:?}\n  amounts {amounts:?}")
    });

    assert!(rate > -1.0, "trial {trial}: rate {rate} outside the domain");
    let residual = xnpv(rate, &dates, &amounts, None).unwrap().abs();
    let budget = residual_budget(rate, &dates, &amounts);
    assert!(
      residual <= budget,
      "trial {trial}: |XNPV({rate:e})| = {residual:e} exceeds {budget:e}\n  amounts {amounts:?}"
    );
    assert_eq!(outcome.root_count(), 1, "trial {trial}: {outcome:?}");
  }

  assert!(checked > 1_000, "only {checked} usable trials generated");
  println!("{checked} single-sign-change flows, all solved");
}

// ===========================================================================
// Typed outcomes
// ===========================================================================

#[test]
fn the_four_non_answers_are_distinguishable() {
  // The defect this replaces: all of these returned `NaN`, and `null` through
  // the binding, so a caller could not tell a data-quality problem from a
  // solver problem from a faithfully reproduced spreadsheet error.
  let no_root = series(11_000.0);
  assert_eq!(
    xirr_outcome(&no_root.0, &no_root.1, None, None, None).unwrap(),
    XirrOutcome::NoRootExists
  );

  // A spreadsheet reports #NUM! here; the true IRR is -99.898%.
  let dates: Vec<DateLike> = ["2020-01-01", "2021-01-01"]
    .iter()
    .map(|s| DateLike::from_str(s).unwrap())
    .collect();
  let amounts = [-1_000.0, 1.0];

  assert_eq!(
    xirr_outcome(
      &dates,
      &amounts,
      None,
      None,
      Some(RootPolicy::SpreadsheetCompat)
    )
    .unwrap(),
    XirrOutcome::SpreadsheetNumError,
    "a reproduced #NUM! must not be reported as a solver failure"
  );

  let robust = xirr_outcome(&dates, &amounts, None, None, None).unwrap();
  match robust {
    XirrOutcome::Root(r) => assert!((r - -0.998_980_947_1).abs() < 1e-9, "got {r}"),
    other => panic!("expected a root, got {other:?}"),
  }

  // Ambiguity is reported, not hidden.
  let three: Vec<DateLike> = ["2015-01-01", "2016-01-01", "2017-01-01", "2018-01-01"]
    .iter()
    .map(|s| DateLike::from_str(s).unwrap())
    .collect();
  let outcome = xirr_outcome(&three, &[-1000., 3000., -2500., 600.], None, None, None).unwrap();
  assert!(outcome.is_ambiguous(), "{outcome:?}");
  assert_eq!(outcome.root_count(), 3);
  let XirrOutcome::MultipleRoots { selected, all } = &outcome else {
    unreachable!()
  };
  // `selected` comes from the spreadsheet's iteration and `all` from the
  // enumerator, so they agree as rates but need not be the same f64. The
  // documented contract is agreement to within DISTINCT_ROOT_TOL.
  assert!(
    all
      .iter()
      .any(|r| (r - selected).abs() <= DISTINCT_ROOT_TOL),
    "pick {selected} matches none of {all:?}"
  );
  assert!(all.windows(2).all(|w| w[0] < w[1]), "unsorted: {all:?}");
}

#[test]
fn the_outcome_rate_always_equals_the_plain_rate() {
  // `xirr_outcome` adds information; it must never disagree about the number.
  let mut checked = 0;
  for outflow in [11_000.0, 20_000.0, 20_001.0, 25_000.0, 100_000.0] {
    for policy in [
      RootPolicy::SpreadsheetCompat,
      RootPolicy::SpreadsheetThenRobust,
      RootPolicy::Lowest,
      RootPolicy::ClosestToGuess,
    ] {
      for guess in [None, Some(0.5), Some(-0.9)] {
        let (dates, amounts) = series(outflow);
        let plain = xirr(&dates, &amounts, guess, None, Some(policy)).unwrap();
        let typed = xirr_outcome(&dates, &amounts, guess, None, Some(policy)).unwrap();
        match typed.rate() {
          Some(r) => assert_eq!(r.to_bits(), plain.to_bits(), "{outflow} {policy:?}"),
          None => assert!(plain.is_nan(), "{outflow} {policy:?}: {typed:?} vs {plain}"),
        }
        checked += 1;
      }
    }
  }
  assert_eq!(checked, 60);
}

// ===========================================================================
// Invariants the financial-systems requirements call for
// ===========================================================================

#[test]
fn zero_amounts_and_duplicate_dates_are_invisible_to_the_result() {
  let plain: Vec<DateLike> = ["2020-01-01", "2021-06-15", "2023-03-01"]
    .iter()
    .map(|s| DateLike::from_str(s).unwrap())
    .collect();
  let reference = xirr(&plain, &[-1_000.0, 400.0, 900.0], None, None, None).unwrap();

  // Same flow, with the first payment split across two lines on one date, a
  // zero interleaved, and a second zero on a date that appears nowhere else.
  let padded: Vec<DateLike> = [
    "2020-01-01",
    "2020-01-01",
    "2020-09-09",
    "2021-06-15",
    "2022-02-02",
    "2023-03-01",
  ]
  .iter()
  .map(|s| DateLike::from_str(s).unwrap())
  .collect();
  let got = xirr(
    &padded,
    &[-1_400.0, 400.0, 0.0, 400.0, 0.0, 900.0],
    None,
    None,
    None,
  )
  .unwrap();

  assert!(
    (got - reference).abs() < 1e-12,
    "{got} vs {reference}: netting or zero handling changed the rate"
  );
}

#[test]
fn the_robust_path_is_convention_agnostic() {
  // The parity guarantee is ACT/365F-only, but the robust path must reach a
  // true root under every convention, including at extreme magnitudes.
  let (dates, amounts) = series(20_001.0);
  for convention in [
    DayCount::ACT_365F,
    DayCount::ACT_360,
    DayCount::ACT_ACT_ISDA,
    DayCount::THIRTY_E_360,
    DayCount::NL_365,
  ] {
    let rate = xirr(&dates, &amounts, None, None, None).unwrap();
    assert!(rate.is_finite());

    let rate = xirr(&dates, &amounts, None, Some(convention), None).unwrap();
    assert!(rate.is_finite(), "{convention:?} returned no rate");
    let residual = xnpv(rate, &dates, &amounts, Some(convention))
      .unwrap()
      .abs();
    assert!(
      residual <= residual_budget(rate, &dates, &amounts),
      "{convention:?}: residual {residual:e}"
    );
  }
}

#[test]
fn a_bad_guess_never_costs_an_answer_that_exists() {
  // On the robust path `guess` is a hint, not a contract: the bracketed search
  // runs first and does not consult it. Only the spreadsheet path is allowed
  // to be guess-dependent.
  let (dates, amounts) = series(20_001.0);
  let reference = xirr(&dates, &amounts, None, None, None).unwrap();

  for guess in [-0.999_999, -0.9, -0.5, 0.0, 0.1, 1.0, 1e3, 1e12] {
    let rate = xirr(&dates, &amounts, Some(guess), None, None).unwrap();
    // Bit-identity is deliberately *not* asserted: a guess large enough to
    // let the spreadsheet's Newton converge means Phase 1 answers instead of
    // Phase 2, and parity requires that its answer be returned verbatim. The
    // guarantee is that the guess never costs the answer and never moves it
    // beyond solver noise.
    assert!(
      relative_error(rate, reference) < 1e-9,
      "guess {guess} gave {rate:e}, no-guess gave {reference:e}"
    );
    let residual = xnpv(rate, &dates, &amounts, None).unwrap().abs();
    assert!(
      residual <= residual_budget(rate, &dates, &amounts),
      "guess {guess}: {residual:e}"
    );
  }
}

#[test]
fn results_are_bit_identical_across_repeated_calls() {
  // No RNG, no iteration-order dependence, no global state, on the robust
  // path as well as the parity one.
  for outflow in [SMALLEST_SOLVABLE_OUTFLOW, 20_001.0, 25_000.0] {
    let (dates, amounts) = series(outflow);
    let first = xirr(&dates, &amounts, None, None, None).unwrap();
    let first_roots = xirr_all_roots(&dates, &amounts, None).unwrap();
    for _ in 0..25 {
      assert_eq!(
        xirr(&dates, &amounts, None, None, None).unwrap().to_bits(),
        first.to_bits()
      );
      assert_eq!(xirr_all_roots(&dates, &amounts, None).unwrap(), first_roots);
    }
  }
}

#[test]
fn nothing_on_the_solve_path_panics() {
  // Every public entry point, against input designed to break each of them.
  // The requirement is "no panics", so the assertion is that we get here.
  let d = |s: &str| DateLike::from_str(s).unwrap();
  let poison = [
    f64::NAN,
    f64::INFINITY,
    f64::NEG_INFINITY,
    0.0,
    f64::MIN_POSITIVE,
    f64::MAX,
  ];

  for bad in poison {
    let cases: Vec<(Vec<DateLike>, Vec<f64>)> = vec![
      (vec![d("2020-01-01"), d("2021-01-01")], vec![-1000.0, bad]),
      (vec![d("2020-01-01"), d("2021-01-01")], vec![bad, 1000.0]),
      (
        vec![d("2020-01-01"), d("2020-01-01"), d("2021-01-01")],
        vec![bad, -bad, 1000.0],
      ),
      (
        vec![d("2020-01-01"), d("2020-01-02")],
        vec![-f64::MAX, f64::MAX],
      ),
    ];
    for (dates, amounts) in cases {
      let _ = xirr(&dates, &amounts, None, None, None);
      let _ = xirr_outcome(&dates, &amounts, None, None, None);
      let _ = xirr_all_roots(&dates, &amounts, None);
      let _ = sign_changes(&dates, &amounts, None);
      let _ = xnpv(0.1, &dates, &amounts, None);
      for policy in [
        RootPolicy::SpreadsheetCompat,
        RootPolicy::Lowest,
        RootPolicy::ClosestToGuess,
      ] {
        let _ = xirr(&dates, &amounts, Some(0.25), None, Some(policy));
      }
    }
  }
}

#[test]
fn the_searched_domain_matches_the_representable_domain() {
  // The published reachable range has to be the truth, not a slogan.
  assert_eq!(
    MIN_SEARCHED_LOG_RATE,
    (2f64.powi(-53)).ln(),
    "lower bound is not ln(2^-53)"
  );
  assert_eq!(
    MAX_SEARCHED_LOG_RATE,
    f64::MAX.ln(),
    "upper bound is not ln(f64::MAX)"
  );

  // One step below the lower bound is no longer a distinct rate above -1...
  assert!(MIN_SEARCHED_LOG_RATE.exp_m1() > -1.0);
  assert_eq!(
    (MIN_SEARCHED_LOG_RATE - 1e-9).exp_m1().to_bits(),
    MIN_SEARCHED_LOG_RATE.exp_m1().to_bits(),
    "rates below the bound are not representable, as documented"
  );
  // ...and one step above the upper bound overflows.
  assert!(MAX_SEARCHED_LOG_RATE.exp_m1().is_finite());
  assert!((MAX_SEARCHED_LOG_RATE + 1.0).exp_m1().is_infinite());
}

// ===========================================================================
// 10. Property test: an even number of sign changes
// ===========================================================================
//
// The odd case is settled by construction: with an odd number of netted sign
// changes `G` has opposite signs at the two ends of the searched domain, so a
// grid spanning that domain *must* contain a bracket and Brent cannot fail.
// `a_single_sign_change_always_yields_a_root` covers it.
//
// An even number proves nothing. Roots arrive in pairs, and a pair is
// invisible to a sign-change scan in two situations:
//
//   1. both roots fall inside one grid cell, so `G` has the same sign at the
//      two nodes that straddle them;
//   2. the curve touches zero without crossing it - a double root - so no
//      bracket exists anywhere at any resolution.
//
// Neither is exotic: a fund with a clawback that very nearly breaks even
// produces exactly case 2. These tests build flows whose roots are known
// *by construction* and check the solver reports them.

/// Three payments at `δ = 0, 1, 2` whose XNPV factorises as
/// `(x - x₁)(x - x₂)` in `x = (1 + r)⁻¹`, so its roots are exactly `u₁` and
/// `u₂` and no solving is needed to know the answer.
///
/// Returns the dates, the amounts, and the day offsets the brute-force
/// reference below uses.
fn flow_with_roots(u1: f64, u2: f64) -> (Vec<DateLike>, Vec<f64>, Vec<i64>) {
  let (x1, x2) = ((-u1).exp(), (-u2).exp());
  // (x - x1)(x - x2) = x1·x2 - (x1 + x2)·x + x², and the coefficient of xⁱ is
  // the payment at δ = i.
  let amounts = vec![x1 * x2, -(x1 + x2), 1.0];
  let dates = vec![
    DateLike::from_str("2015-01-01").unwrap(),
    DateLike::from_str("2016-01-01").unwrap(), // +365 days: δ = 1 under ACT/365F
    DateLike::from_str("2016-12-31").unwrap(), // +730 days: δ = 2
  ];
  (dates, amounts, vec![0, 365, 730])
}

/// `G(u) = Σ aᵢ·exp(-u·δᵢ)`, evaluated from first principles.
///
/// Deliberately does not call the library: a reference derived from the code
/// under test proves nothing. `δ` comes from day offsets the caller already
/// knows, so not even the date arithmetic is shared. The dominant-term scaling
/// is the same guard the solver uses, but that is an overflow precaution, not
/// a search - the search below is brute force and shares nothing.
fn reference_g(days: &[i64], amounts: &[f64], u: f64) -> f64 {
  let d_ref = if u >= 0.0 {
    days[0]
  } else {
    days[days.len() - 1]
  } as f64
    / 365.0;
  days
    .iter()
    .zip(amounts)
    .map(|(&d, &a)| a * (-u * (d as f64 / 365.0 - d_ref)).exp())
    .sum()
}

/// Every root of `G` in `[lo, hi]`, by brute force: a scan ten times finer
/// than the solver's densest band, refined by bisection. Slow and stupid on
/// purpose - it is the yardstick, so it must not be clever.
fn reference_roots(days: &[i64], amounts: &[f64], lo: f64, hi: f64) -> Vec<f64> {
  const STEP: f64 = 1e-3;
  const BISECTIONS: u32 = 200;

  let mut out = Vec::new();
  let steps = ((hi - lo) / STEP).ceil() as usize;
  let (mut prev_u, mut prev_v) = (lo, reference_g(days, amounts, lo));

  for i in 1..=steps {
    let u = lo + i as f64 * STEP;
    let v = reference_g(days, amounts, u);
    if prev_v != 0.0 && v != 0.0 && (prev_v > 0.0) != (v > 0.0) {
      let (mut a, mut b, mut fa) = (prev_u, u, prev_v);
      for _ in 0..BISECTIONS {
        let m = 0.5 * (a + b);
        let fm = reference_g(days, amounts, m);
        if (fa > 0.0) != (fm > 0.0) {
          b = m;
        } else {
          a = m;
          fa = fm;
        }
      }
      out.push(0.5 * (a + b));
    }
    (prev_u, prev_v) = (u, v);
  }
  out
}

/// Is `rate` within one part in `1e-6` of `expected`? Loose on purpose: the
/// question here is "did the solver find this root at all", not "to how many
/// digits", which `every_returned_rate_passes_the_documented_audit_check`
/// already pins.
fn found(reported: &[f64], expected: f64) -> bool {
  reported
    .iter()
    .any(|r| (r - expected).abs() <= 1e-6 * expected.abs().max(1.0))
}

#[test]
fn a_pair_of_roots_inside_one_grid_cell_is_still_found() {
  // Acceptance 10a. The solver's densest band steps 0.01 in `u`, so two roots
  // closer than that share a cell and `G` has the same sign at both ends of
  // it. The gaps below straddle that width deliberately.
  let mut rng = Rng(0xC0FF_EE00_1234_5678);
  let mut checked = 0usize;

  for trial in 0..400 {
    let u1 = rng.uniform(-6.0, 6.0);
    // Sub-cell, cell-width, and comfortably-wider gaps, in that proportion.
    let gap = match trial % 3 {
      0 => rng.uniform(1e-3, 9e-3),
      1 => rng.uniform(9e-3, 3e-2),
      _ => rng.uniform(3e-2, 5e-1),
    };
    let (dates, amounts, days) = flow_with_roots(u1, u1 + gap);

    // Skip anything the library would reject outright, or that netting
    // collapses - the claim under test is about the search, not validation.
    if sign_changes(&dates, &amounts, None).unwrap() != 2 {
      continue;
    }
    let expected = reference_roots(&days, &amounts, u1 - 1.0, u1 + gap + 1.0);
    if expected.len() != 2 {
      continue; // conditioning lost one of them; not this test's subject
    }

    checked += 1;
    let reported = xirr_all_roots(&dates, &amounts, None).unwrap();

    for u in &expected {
      let rate = u.exp_m1();
      assert!(
        found(&reported, rate),
        "trial {trial}: gap {gap:.5} in u\n  \
         expected a root at r = {rate:e} (u = {u:.6})\n  \
         xirr_all_roots returned {reported:?}\n  \
         amounts {amounts:?}"
      );
    }
  }

  assert!(checked > 100, "only {checked} flows exercised the property");
  println!("pairs inside one grid cell: {checked} flows checked");
}

#[test]
fn a_tangential_root_is_found_even_though_nothing_brackets_it() {
  // Acceptance 10b. A double root touches zero without crossing, so `G` never
  // changes sign and no bracket exists at any grid resolution. This is the one
  // case a sign-change scan provably cannot see.
  //
  // `[x₀², -2x₀, 1]` has the double root `x = x₀`, i.e. `u = -ln(x₀)`.
  for u0 in [
    0.0f64,   // lands exactly on a grid node - the easy case
    0.005,    // mid-cell in the dense band
    -0.003,   //
    3.141_59, // nowhere near a node
    -4.567, 7.5, // just outside the dense band, where the step widens to 0.5
  ] {
    let x0 = (-u0).exp();
    let amounts = vec![x0 * x0, -2.0 * x0, 1.0];
    let dates = vec![
      DateLike::from_str("2015-01-01").unwrap(),
      DateLike::from_str("2016-01-01").unwrap(),
      DateLike::from_str("2016-12-31").unwrap(),
    ];

    let rate = u0.exp_m1();
    let residual = xnpv(rate, &dates, &amounts, None).unwrap();
    let gross: f64 = amounts.iter().map(|a| a.abs()).sum();
    assert!(
      residual.abs() <= 1e-9 * gross.max(1.0),
      "u0 {u0}: the constructed root is not one - |XNPV| = {residual:e}"
    );

    let reported = xirr_all_roots(&dates, &amounts, None).unwrap();
    assert!(
      found(&reported, rate),
      "u0 {u0}: double root at r = {rate:e} not reported; got {reported:?}"
    );

    let outcome = xirr_outcome(&dates, &amounts, None, None, None).unwrap();
    assert!(
      outcome.rate().is_some(),
      "u0 {u0}: no rate at all, got {outcome:?}"
    );
  }
}

#[test]
fn xirr_and_the_enumeration_agree_on_even_sign_changes() {
  // The existing agreement test walks `SOLVABLE`, a one-parameter family that
  // never produces a close root pair. This is the same contract - any rate
  // `xirr` produces must appear in the enumeration - over the case that
  // family does not reach.
  let mut rng = Rng(0x0DDB_A11_5EED_0002);
  let mut checked = 0usize;

  for _ in 0..300 {
    let u1 = rng.uniform(-6.0, 6.0);
    // Gaps wide enough that the two roots are genuinely distinguishable. This
    // test is about *agreement* between the two entry points, and a pair
    // closer than this is a near-double root where the curve is flat enough
    // that Phase 1's weak convergence test stops measurably short of Brent's
    // answer - real ill-conditioning, not disagreement, and already pinned by
    // `a_cancelling_flow_is_stable_to_its_own_conditioning_and_no_better`.
    // Sub-cell gaps are covered by the existence test above, which is the
    // property that actually matters there.
    let (dates, amounts, _) = flow_with_roots(u1, u1 + rng.uniform(0.05, 0.5));
    if sign_changes(&dates, &amounts, None).unwrap() != 2 {
      continue;
    }

    let rate = xirr(&dates, &amounts, None, None, None).unwrap();
    if !rate.is_finite() {
      continue;
    }
    let roots = xirr_all_roots(&dates, &amounts, None).unwrap();
    checked += 1;

    // Compared in `u = ln(1 + r)`, for the same reason `DISTINCT_ROOT_U_TOL`
    // exists: a fixed step in `u` is a fixed *relative* step in `1 + r`, so
    // the comparison means the same thing at `r = 0.05` and at `r = 125`.
    // It also has to: these flows are built with roots as close as 1e-3 apart
    // in `u`, and near a near-double root the curve is flat enough that
    // Newton in rate space and Brent in log-rate space legitimately stop on
    // different floats. The module docs already say `selected` need not be
    // bit-identical to its counterpart in `all`.
    let separation = |root: f64| (root.ln_1p() - rate.ln_1p()).abs();
    let closest = roots
      .iter()
      .map(|r| separation(*r))
      .fold(f64::INFINITY, f64::min);
    assert!(
      closest < 1e-6,
      "xirr returned {rate:e} (u = {:.9}), nearest enumerated root is \
       {closest:e} away in u\n  enumeration {roots:?}\n  amounts {amounts:?}",
      rate.ln_1p()
    );
  }

  assert!(checked > 100, "only {checked} flows exercised the property");
  println!("xirr/enumeration agreement on even sign changes: {checked} flows");
}

#[test]
fn three_roots_in_one_cell_still_yield_a_rate_even_though_enumeration_is_short() {
  // The documented boundary of the turning-point mechanism, pinned so it is a
  // measured limit rather than a claim in a document.
  //
  // A *pair* inside one cell is found: the curve turns once between them, so
  // `G'` takes opposite signs at the two nodes straddling the cell. Three
  // roots turn it twice, `G'` returns to the sign it started with, and the
  // derivative scan misses them exactly as the value scan does.
  //
  // What must not regress is the weaker but more important promise: `xirr`
  // still returns a genuine rate. This asserts that, and the width at which
  // enumeration becomes complete, without asserting the shortfall itself -
  // fixing the gap should not fail this test.
  let cubic = |us: [f64; 3]| {
    let [x1, x2, x3] = us.map(|u: f64| (-u).exp());
    // (x - x1)(x - x2)(x - x3); the coefficient of xⁱ is the payment at δ = i.
    let amounts = vec![
      -x1 * x2 * x3,
      x1 * x2 + x1 * x3 + x2 * x3,
      -(x1 + x2 + x3),
      1.0,
    ];
    let dates = vec![
      DateLike::from_str("2015-01-01").unwrap(),
      DateLike::from_str("2016-01-01").unwrap(),
      DateLike::from_str("2016-12-31").unwrap(),
      DateLike::from_str("2017-12-31").unwrap(),
    ];
    (dates, amounts)
  };

  // Spread across two dense-band cells: all three are enumerated.
  let (dates, amounts) = cubic([1.00, 1.01, 1.02]);
  let roots = xirr_all_roots(&dates, &amounts, None).unwrap();
  assert_eq!(
    roots.len(),
    3,
    "three roots one cell apart should all be found, got {roots:?}"
  );

  // Packed into one cell: the enumeration is allowed to come up short, but a
  // rate must still be returned and it must be a real root.
  let (dates, amounts) = cubic([1.000, 1.004, 1.008]);
  let rate = xirr(&dates, &amounts, None, None, None).unwrap();
  assert!(
    rate.is_finite(),
    "no rate at all for three tightly clustered roots"
  );
  let residual = xnpv(rate, &dates, &amounts, None).unwrap().abs();
  assert!(
    residual <= residual_budget(rate, &dates, &amounts),
    "returned {rate:e} with residual {residual:e}, which is not a root"
  );
}
