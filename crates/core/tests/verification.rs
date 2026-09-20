//! Every rate this library labels a root must actually be one.
//!
//! The oracle here is the mathematics, not a spreadsheet. That is deliberate:
//! the golden corpus can only check the engines whose `expected_*.csv` happens
//! to be present, and a rate can be wrong in a way every engine agrees on -
//! the spreadsheet convergence test is shared by all of them. Back-calculating
//! `XNPV` at the answer needs no engine and no fixture.
//!
//! Run with `cargo test -p xirr-core --test verification`.

use std::str::FromStr;

use xirr_core::{xirr, xirr_all_roots, xirr_outcome, xnpv, DateLike, RootPolicy, XirrOutcome};

fn dates(iso: &[&str]) -> Vec<DateLike> {
  iso.iter().map(|s| DateLike::from_str(s).unwrap()).collect()
}

/// `rho(r) = |XNPV(r)| / sum |term_i(r)|` over the **raw** payments: the
/// relative cancellation of the sum as entered.
///
/// The denominator is just `xnpv` over the absolute amounts - `xnpv` validates
/// only length, so it will evaluate an all-positive flow, and that sum is
/// `sum |a_i| (1+r)^-d_i` term for term. Deriving it this way rather than
/// reimplementing year-fraction arithmetic means the test cannot drift from
/// the library's own day-count convention: if `year_fraction` changes, both
/// sides move together.
fn rho_raw(rate: f64, d: &[DateLike], a: &[f64]) -> f64 {
  let magnitudes: Vec<f64> = a.iter().map(|x| x.abs()).collect();
  let sum = xnpv(rate, d, a, None).unwrap().abs();
  let gross = xnpv(rate, d, &magnitudes, None).unwrap();
  sum / gross
}

/// The same ratio over the **netted** flow, which is the one that means
/// something.
///
/// Two payments sharing a date share a year fraction, so they are one term of
/// `XNPV` however they were entered; summing them first is exact, not an
/// approximation. (Netting by date rather than by year fraction is equivalent
/// under the ACT conventions this test uses.)
///
/// The distinction is load-bearing rather than cosmetic - see
/// [`netting_is_what_makes_the_ratio_mean_anything`].
fn rho(rate: f64, d: &[DateLike], a: &[f64]) -> f64 {
  let mut netted: Vec<(DateLike, f64)> = Vec::new();
  for (date, amount) in d.iter().zip(a) {
    match netted.iter_mut().find(|(seen, _)| seen == date) {
      Some((_, sum)) => *sum += amount,
      None => netted.push((*date, *amount)),
    }
  }
  netted.retain(|(_, amount)| *amount != 0.0);

  let dates: Vec<DateLike> = netted.iter().map(|(d, _)| *d).collect();
  let amounts: Vec<f64> = netted.iter().map(|(_, a)| *a).collect();
  rho_raw(rate, &dates, &amounts)
}

/// A flow whose day-zero payments cancel exactly and whose remaining terms
/// have no real root: with `v = (1 + r)^-1 > 0` the netted flow is
/// `v(-1000 + 300v - 50v^2)`, and that quadratic has discriminant `-44`.
///
/// `XNPV` therefore only *approaches* zero as `r -> infinity`. The spreadsheet
/// algorithm stops on an absolute epsilon of 1e-10 and declares the asymptote
/// solved; this library reproduces that rate and must not call it a root.
fn asymptote() -> (Vec<DateLike>, Vec<f64>) {
  (
    dates(&[
      "2020-01-01",
      "2020-01-01",
      "2021-01-01",
      "2022-01-01",
      "2023-01-01",
    ]),
    vec![-100.0, 100.0, -1000.0, 300.0, -50.0],
  )
}

#[test]
fn the_asymptote_flow_really_has_no_root() {
  // Guards the premise of every test below. If this ever fails, the fixture
  // has been edited into a flow that does have a root and the rest is vacuous.
  let (d, a) = asymptote();
  assert_eq!(
    xirr_all_roots(&d, &a, None).unwrap(),
    Vec::<f64>::new(),
    "the fixture is supposed to be rootless"
  );
}

#[test]
fn an_asymptote_is_never_labelled_a_root() {
  let (d, a) = asymptote();
  for policy in [
    RootPolicy::SpreadsheetCompat,
    RootPolicy::SpreadsheetThenRobust,
  ] {
    let outcome = xirr_outcome(&d, &a, None, None, Some(policy)).unwrap();
    let XirrOutcome::UnverifiedRate { rate, roots } = &outcome else {
      panic!("{policy:?}: expected UnverifiedRate, got {outcome:?}");
    };
    assert!(roots.is_empty(), "{policy:?}: roots {roots:?}");

    // Parity is intact: the rate is still the spreadsheet's, and still what
    // `xirr` hands back. Only the label changed.
    assert_eq!(
      *rate,
      xirr(&d, &a, None, None, Some(policy)).unwrap(),
      "{policy:?}: outcome and xirr disagree"
    );

    // And it is emphatically not a root - no cancellation at all.
    let r = rho(*rate, &d, &a);
    assert!(
      r > 0.1,
      "{policy:?}: rho = {r:e}, which is not the asymptote we think it is"
    );
  }
}

#[test]
fn the_correctness_policies_refuse_an_unverified_rate() {
  // `lowest` and `closestToGuess` exist to trade parity for correctness, so
  // falling back to a rate that fails verification is the one thing they must
  // not do.
  let (d, a) = asymptote();
  for policy in [RootPolicy::Lowest, RootPolicy::ClosestToGuess] {
    let rate = xirr(&d, &a, None, None, Some(policy)).unwrap();
    assert!(rate.is_nan(), "{policy:?} returned {rate}");
  }
}

#[test]
fn a_labelled_root_always_survives_back_calculation() {
  // The general contract, over every shape of flow in the suite: if the
  // library calls something a root, XNPV at that rate is zero to within the
  // noise of the terms being summed.
  let flows: Vec<(&str, Vec<DateLike>, Vec<f64>)> = vec![
    (
      "conventional",
      dates(&["2020-01-01", "2021-01-01", "2022-01-01"]),
      vec![-1000.0, 750.0, 500.0],
    ),
    (
      "multiple-roots",
      dates(&["2020-01-01", "2021-01-01", "2022-01-01"]),
      vec![-1000.0, 2500.0, -1540.0],
    ),
    (
      "near-total-loss",
      dates(&["2020-01-01", "2029-01-01"]),
      vec![-1000.0, 1.0],
    ),
    (
      "huge-rate",
      dates(&["2020-01-01", "2020-01-02"]),
      vec![-1.0, 1000.0],
    ),
    (
      "tiny-amounts",
      dates(&["2020-01-01", "2021-01-01"]),
      vec![-0.0001, 0.00013],
    ),
    ("asymptote", asymptote().0, asymptote().1),
  ];

  for (name, d, a) in &flows {
    for policy in [
      RootPolicy::SpreadsheetCompat,
      RootPolicy::SpreadsheetThenRobust,
      RootPolicy::Lowest,
      RootPolicy::ClosestToGuess,
    ] {
      let outcome = xirr_outcome(d, a, None, None, Some(policy)).unwrap();
      let claimed = match &outcome {
        XirrOutcome::Root(r) => vec![*r],
        XirrOutcome::MultipleRoots { selected, all } => {
          let mut v = all.clone();
          v.push(*selected);
          v
        }
        // Carries a rate on purpose, and does not claim it is a root.
        XirrOutcome::UnverifiedRate { .. } => Vec::new(),
        _ => Vec::new(),
      };

      for rate in claimed {
        let r = rho(rate, d, a);
        assert!(
          r <= 1e-9,
          "{name}/{policy:?}: claimed root {rate:e} has rho = {r:e}, \
           |XNPV| = {:e}",
          xnpv(rate, d, a, None).unwrap().abs()
        );
      }
    }
  }
}

#[test]
fn every_enumerated_root_survives_back_calculation() {
  // `xirrAllRoots` makes the strongest claim in the library - it says these
  // are *all* the roots - so each one had better be one.
  let flows = [
    (
      dates(&["2020-01-01", "2021-01-01", "2022-01-01"]),
      vec![-1000.0, 2500.0, -1540.0],
    ),
    (
      dates(&["2020-01-01", "2021-01-01", "2022-01-01", "2023-01-01"]),
      vec![-1.0, 5.0, -7.0, 3.0],
    ),
    (asymptote().0, asymptote().1),
  ];
  for (d, a) in &flows {
    for rate in xirr_all_roots(d, a, None).unwrap() {
      let r = rho(rate, d, a);
      assert!(r <= 1e-9, "enumerated root {rate:e} has rho = {r:e}");
    }
  }
}

#[test]
fn netting_is_what_makes_the_ratio_mean_anything() {
  // Why `is_root` measures the netted flow, pinned as a fact rather than left
  // in a comment.
  //
  // The asymptote flow opens with `-100, +100` on the same day. Those two
  // contribute nothing to `XNPV` at any rate - they cancel - but they are the
  // largest terms in `sum |term|` once a large rate has discounted everything
  // else away. Divide by that and the ratio is tiny for *every* sufficiently
  // large rate, so the test passes on a cash flow that has no root at all.
  //
  // This is exactly the defect the previous rule had: it normalised against
  // the gross cash flow, which has the same blind spot for the same reason.
  let (d, a) = asymptote();
  let rate = xirr(&d, &a, None, None, None).unwrap();

  let raw = rho_raw(rate, &d, &a);
  let netted = rho(rate, &d, &a);

  assert!(
    raw < 1e-9,
    "the un-netted ratio is supposed to be fooled here, got {raw:e}"
  );
  assert!(
    netted > 0.1,
    "the netted ratio is supposed to catch it, got {netted:e}"
  );
}
