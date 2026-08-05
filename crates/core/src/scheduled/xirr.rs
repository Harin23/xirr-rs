//! XIRR: internal rate of return for an irregular schedule of cash flows.
//!
//! # What this module guarantees
//!
//! For a given cash flow it returns the same rate Excel, Google Sheets and
//! LibreOffice Calc return - **including which root they pick when several
//! exist** - and only deviates when those engines would give up entirely.
//!
//! # Why that is harder than it sounds
//!
//! `XNPV(r) = 0` can have many solutions. A cash flow that changes sign three
//! times can have three mathematically valid IRRs, and picking a different one
//! is not a rounding difference: it is the difference between reporting -57%
//! and reporting -22% for the same fund.
//!
//! Spreadsheets do not apply a *rule* to choose among them. They run Newton's
//! method from a starting guess and return wherever it lands. The answer is
//! therefore **path-dependent**, and no selection heuristic reproduces it:
//! measured over ~400 multiple-root cash flows, "return the lowest root"
//! matched the spreadsheet 37% of the time and "return the root nearest the
//! guess" 62%. Reproducing the iteration itself matches 100%.
//!
//! # The two phases
//!
//! ```text
//!   xirr()
//!     |
//!     +-- Phase 1: CashFlow::solve_like_a_spreadsheet()   <- always runs first
//!     |     Newton from `guess` (default 0.1), then a fixed 0.01 rescan
//!     |     grid over [-0.99, +0.99]. Verbatim port of the algorithm
//!     |     Excel-compatible spreadsheets use. Returns NaN on #NUM!.
//!     |
//!     +-- Phase 2: CashFlow::solve_robustly()             <- only if 1 gave up
//!           Bracketed root finding over (-1, 1e6], then multi-start Newton.
//!           Answers cash flows no spreadsheet can, but never *overrides* one.
//! ```
//!
//! Phase 1's result is returned **verbatim, without checking its residual**.
//! That is deliberate and is the core of the parity contract: re-checking is
//! exactly what would let this library print 200% where Excel prints 5%.
//! Callers who want to know whether the rate is a true root can call [`xnpv`]
//! on it. See `docs/ALGORITHM.md` for the full rationale.
//!
//! # Attribution
//!
//! Phase 1 is a port of `AnalysisAddIn::getXirr` from
//! `main/scaddins/source/analysis/financial.cxx` in Apache OpenOffice,
//! Apache License 2.0. See the NOTICE file at the repository root.

use super::{year_fraction, DayCount};
use crate::{
  models::{validate, validate_length, DateLike, InvalidPaymentsError},
  optimize::{brentq, find_brackets, newton_excel_order, newton_to_residual},
};

// ---------------------------------------------------------------------------
// Tolerances
//
// Every tolerance XIRR uses is declared here, once, with its reason. They are
// not interchangeable and must not be collapsed into a single number.
// ---------------------------------------------------------------------------

/// Starting rate when the caller does not supply one. Every spreadsheet uses
/// 10%, and because the answer is path-dependent under multiple roots this
/// value is part of the public contract rather than a performance hint.
pub const DEFAULT_GUESS: f64 = 0.1;

/// How close to zero `XNPV(rate)` must be for `rate` to count as a root,
/// **relative to the gross size of the cash flow**.
///
/// Relative, not absolute, because an absolute threshold makes correctness
/// depend on denomination: an earlier revision solved a cash flow at 1e9 and
/// returned nothing for the identical flow at 1e12, purely because
/// `|XNPV| < 1e-3` had become unreachable.
pub const RESIDUAL_REL_TOL: f64 = 1e-9;

/// How far apart two rates must be to count as different roots.
///
/// Unrelated to [`RESIDUAL_REL_TOL`] despite the similar magnitude: that one
/// measures money, this one measures rates. 1e-7 is a hundred-thousandth of a
/// basis point - far below any reporting precision, but comfortably above the
/// spread between two `brentq` runs converging on one root from two brackets.
const DISTINCT_ROOT_TOL: f64 = 1e-7;

/// Upper bound of the Phase 2 root search. 1e6 is 100,000,000% - a rate no
/// real cash flow produces, but reachable by pathological inputs.
const MAX_SEARCHED_RATE: f64 = 1.0e6;

/// Seeds for the multi-start Newton fallback, tried in order after the
/// caller's guess. Spread across the plausible range so a root that bracketing
/// missed still gets a chance.
const FALLBACK_SEEDS: [f64; 6] = [0.0, -0.5, -0.9, 0.5, 2.0, 10.0];

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// What to do when `XNPV(r) = 0` has more than one solution in `(-1, inf)`.
///
/// This is a business decision, not an implementation detail, so it is an
/// explicit type rather than something that falls out of solver ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RootPolicy {
  /// Exactly what a spreadsheet does, including returning `NaN` where a
  /// spreadsheet shows `#NUM!`. Use when output must tie out to a workbook.
  SpreadsheetCompat,

  /// The spreadsheet's answer whenever a spreadsheet has one; bracketed root
  /// finding otherwise. Never contradicts a spreadsheet, strictly more likely
  /// to return something. **Default.**
  #[default]
  SpreadsheetThenRobust,

  /// Enumerate every root and return the smallest. Deterministic and
  /// conservative; ignores spreadsheet convention. Suited to reporting where
  /// understating return is the safe direction to fail.
  Lowest,

  /// Enumerate every root and return the one nearest `guess`. Deterministic,
  /// and lets a caller who knows the expected magnitude steer the answer.
  ClosestToGuess,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Internal rate of return for an irregular schedule.
///
/// Returns `NaN` when no rate can be found; the binding layer maps that to
/// `null`. Returns `Err` for input a spreadsheet would reject outright.
///
/// # Parity caveat
///
/// The parity guarantee holds for the default day count (ACT/365F), because
/// that is the only convention spreadsheet `XIRR()` implements. Any other
/// `day_count` yields a mathematically sound rate that no spreadsheet agrees
/// with.
pub fn xirr(
  dates: &[DateLike],
  amounts: &[f64],
  guess: Option<f64>,
  day_count: Option<DayCount>,
  policy: Option<RootPolicy>,
) -> Result<f64, InvalidPaymentsError> {
  let flow = CashFlow::new(dates, amounts, day_count)?;
  let guess = checked_guess(guess)?;
  let policy = policy.unwrap_or_default();

  // Phase 1 always runs first: every policy either returns its answer or
  // needs to know that it failed.
  let spreadsheet_rate = flow.solve_like_a_spreadsheet(guess);

  Ok(match policy {
    RootPolicy::SpreadsheetCompat => spreadsheet_rate,

    RootPolicy::SpreadsheetThenRobust if spreadsheet_rate.is_finite() => spreadsheet_rate,
    RootPolicy::SpreadsheetThenRobust => flow.solve_robustly(guess),

    // These two ignore Phase 1's choice but still fall back to it when no root
    // can be enumerated, so they never lose an answer the default would find.
    RootPolicy::Lowest => flow.roots().first().copied().unwrap_or(spreadsheet_rate),
    RootPolicy::ClosestToGuess => closest_to(&flow.roots(), guess).unwrap_or(spreadsheet_rate),
  })
}

/// Every rate at which XNPV crosses zero, ascending.
///
/// A length greater than one means the IRR is genuinely ambiguous and the
/// single value from [`xirr`] is a convention, not a fact. Surface this in
/// reporting rather than hiding it: "there are three IRRs and the spreadsheet
/// picked the leftmost" is far more actionable than one silent number.
pub fn xirr_all_roots(
  dates: &[DateLike],
  amounts: &[f64],
  day_count: Option<DayCount>,
) -> Result<Vec<f64>, InvalidPaymentsError> {
  Ok(CashFlow::new(dates, amounts, day_count)?.roots())
}

/// Net present value of an irregular schedule at a given rate.
///
/// Unlike [`xirr`] this does not require both positive and negative amounts,
/// so it can be used to check the residual of any rate.
pub fn xnpv(
  rate: f64,
  dates: &[DateLike],
  amounts: &[f64],
  day_count: Option<DayCount>,
) -> Result<f64, InvalidPaymentsError> {
  validate_length(amounts, dates)?;
  if dates.is_empty() {
    return Ok(0.0);
  }
  Ok(
    CashFlow {
      amounts: amounts.to_vec(),
      deltas: year_fractions(dates, day_count),
      scale: gross_size(amounts),
    }
    .xnpv(rate),
  )
}

/// Sign changes in the cash flow, ignoring zero and non-finite amounts.
///
/// By Descartes' rule of signs, zero or one sign change means at most one root
/// exists in `(-1, inf)` - so every [`RootPolicy`] must agree and the
/// multiple-root question does not arise.
pub fn sign_changes(amounts: &[f64]) -> i32 {
  let mut changes = 0;
  let mut previous: Option<f64> = None;
  for &amount in amounts.iter().filter(|a| a.is_finite() && **a != 0.0) {
    if previous.is_some_and(|prev: f64| prev.signum() != amount.signum()) {
      changes += 1;
    }
    previous = Some(amount);
  }
  changes
}

// ---------------------------------------------------------------------------
// CashFlow: the objective function and everything that operates on it
// ---------------------------------------------------------------------------

/// A validated cash flow, ready to be solved.
///
/// Bundling the amounts with their year fractions and gross size means the
/// solver methods read like the mathematics, instead of threading three
/// parallel slices and a tolerance through every call.
struct CashFlow {
  amounts: Vec<f64>,
  /// Year fractions measured from the **first** payment in input order.
  deltas: Vec<f64>,
  /// Sum of `|amount|`, floored at 1.0. Scales the residual tolerance.
  scale: f64,
}

impl CashFlow {
  fn new(
    dates: &[DateLike],
    amounts: &[f64],
    day_count: Option<DayCount>,
  ) -> Result<Self, InvalidPaymentsError> {
    validate(amounts, Some(dates))?;
    reject_dates_before_the_first(dates)?;
    Ok(Self {
      amounts: amounts.to_vec(),
      deltas: year_fractions(dates, day_count),
      scale: gross_size(amounts),
    })
  }

  /// `XNPV(rate) = sum over i of amount_i * (1 + rate)^-delta_i`
  ///
  /// Uses `powf` deliberately. The `exp2(log2(a) * b)` shortcut is faster but
  /// loses a few ULP, and Phase 1 compares against a 1e-10 *absolute* epsilon,
  /// so those ULP can change which root the iteration converges to. Measured:
  /// 4 divergences in 2,484 samples.
  fn xnpv(&self, rate: f64) -> f64 {
    if rate <= -1.0 {
      return f64::INFINITY;
    }
    let base = 1.0 + rate;
    self
      .amounts
      .iter()
      .zip(&self.deltas)
      .map(|(amount, &delta)| amount * base.powf(-delta))
      .sum()
  }

  /// `XNPV(rate)` and its first derivative, sharing one `powf` per payment.
  ///
  /// Returning both from a single pass halves the transcendental calls in
  /// Newton's inner loop, which is the hot path.
  fn xnpv_with_deriv(&self, rate: f64) -> (f64, f64) {
    if rate <= -1.0 {
      // Push Newton back into the domain rather than let it evaluate
      // fractional powers of a negative base.
      return (f64::INFINITY, f64::INFINITY);
    }
    let base = 1.0 + rate;
    self
      .amounts
      .iter()
      .zip(&self.deltas)
      .fold((0.0, 0.0), |(value, deriv), (amount, &delta)| {
        let term = amount * base.powf(-delta);
        (value + term, deriv + term * -delta / base)
      })
  }

  /// Is `rate` a true root, within the relative residual tolerance?
  fn is_root(&self, rate: f64) -> bool {
    rate.is_finite() && self.xnpv(rate).abs() <= self.residual_tolerance()
  }

  fn residual_tolerance(&self) -> f64 {
    RESIDUAL_REL_TOL * self.scale
  }

  /// Phase 1. Newton in the exact order a spreadsheet performs it.
  /// `NaN` means a spreadsheet would show `#NUM!`.
  fn solve_like_a_spreadsheet(&self, guess: f64) -> f64 {
    newton_excel_order(guess, &|rate| self.xnpv_with_deriv(rate))
  }

  /// Phase 2. Reached only when Phase 1 returned `NaN`, so it can add answers
  /// but never change one a spreadsheet would have given.
  fn solve_robustly(&self, guess: f64) -> f64 {
    if let Some(root) = closest_to(&self.roots(), guess) {
      return root;
    }
    // Bracketing missed it - a root can hide between grid points where the
    // function only just crosses zero. Try Newton from a spread of seeds.
    std::iter::once(guess)
      .chain(FALLBACK_SEEDS)
      .map(|seed| {
        newton_to_residual(
          seed,
          &|rate| self.xnpv_with_deriv(rate),
          self.residual_tolerance(),
        )
      })
      .find(|rate| self.is_root(*rate))
      .unwrap_or(f64::NAN)
  }

  /// Every root in `(-1, MAX_SEARCHED_RATE]`, ascending and deduplicated.
  fn roots(&self) -> Vec<f64> {
    let xnpv = |rate| self.xnpv(rate);

    let mut roots: Vec<f64> = find_brackets(&xnpv, MAX_SEARCHED_RATE)
      .into_iter()
      .map(|(lo, hi)| brentq(&xnpv, lo, hi, 100))
      .filter(|rate| self.is_root(*rate))
      .collect();

    roots.sort_by(|a, b| a.partial_cmp(b).expect("is_root filtered out non-finite"));
    roots.dedup_by(|a, b| (*a - *b).abs() <= DISTINCT_ROOT_TOL);
    roots
  }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Year fractions measured from `dates[0]` - the **first** payment in input
/// order, not the earliest date.
///
/// Spreadsheets do the same. Using `min()` leaves the roots unchanged but
/// rescales the objective by `(1 + r)^k`, and because that factor is itself a
/// function of `r` it changes the Newton trajectory. Measured: that alone
/// breaks parity on ~10% of multiple-root inputs.
fn year_fractions(dates: &[DateLike], day_count: Option<DayCount>) -> Vec<f64> {
  let convention = day_count.unwrap_or_default();
  let Some(first) = dates.first() else {
    return Vec::new();
  };
  dates
    .iter()
    .map(|date| year_fraction(first, date, convention))
    .collect()
}

/// Sum of absolute amounts, floored at 1.0 so sub-unit cash flows are not held
/// to an impossible tolerance.
fn gross_size(amounts: &[f64]) -> f64 {
  amounts
    .iter()
    .filter(|a| a.is_finite())
    .map(|a| a.abs())
    .sum::<f64>()
    .max(1.0)
}

/// Spreadsheets raise `#NUM!` if any date precedes the first one rather than
/// silently reordering. Matching that is part of parity: a caller who hands us
/// unsorted input would otherwise get a different answer from their workbook
/// with no indication why.
fn reject_dates_before_the_first(dates: &[DateLike]) -> Result<(), InvalidPaymentsError> {
  match dates.split_first() {
    Some((first, rest)) if rest.iter().any(|date| date < first) => Err(InvalidPaymentsError::new(
      "all dates must be on or after the first date",
    )),
    _ => Ok(()),
  }
}

/// A guess of `-1` or below is outside the domain of `(1 + r)^-t`, and NaN
/// would poison the whole iteration. Spreadsheets reject both.
fn checked_guess(guess: Option<f64>) -> Result<f64, InvalidPaymentsError> {
  let guess = guess.unwrap_or(DEFAULT_GUESS);
  if !guess.is_finite() || guess <= -1.0 {
    return Err(InvalidPaymentsError::new(
      "guess must be a finite number greater than -1",
    ));
  }
  Ok(guess)
}

/// Root nearest `guess`, breaking ties toward the smaller rate so the result
/// is deterministic for symmetric root pairs.
fn closest_to(roots: &[f64], guess: f64) -> Option<f64> {
  roots.iter().copied().min_by(|a, b| {
    let by_distance = (a - guess)
      .abs()
      .partial_cmp(&(b - guess).abs())
      .expect("roots are finite");
    by_distance.then(a.partial_cmp(b).expect("roots are finite"))
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::str::FromStr;

  fn cash_flow(rows: &[(&str, f64)]) -> (Vec<DateLike>, Vec<f64>) {
    (
      rows
        .iter()
        .map(|(d, _)| DateLike::from_str(d).unwrap())
        .collect(),
      rows.iter().map(|(_, a)| *a).collect(),
    )
  }

  /// `[-1000, 3000, -2500, 600]` on annual dates has three valid IRRs:
  /// -57.19%, -21.92% and +79.59%. LibreOffice Calc returns the first.
  fn three_root_flow() -> (Vec<DateLike>, Vec<f64>) {
    cash_flow(&[
      ("2015-01-01", -1000.),
      ("2016-01-01", 3000.),
      ("2017-01-01", -2500.),
      ("2018-01-01", 600.),
    ])
  }

  #[test]
  fn matches_the_spreadsheet_when_roots_are_ambiguous() {
    let (dates, amounts) = three_root_flow();
    let rate = xirr(&dates, &amounts, None, None, None).unwrap();
    assert!((rate - -0.571885951525731).abs() < 1e-9, "got {rate}");
    assert_eq!(xirr_all_roots(&dates, &amounts, None).unwrap().len(), 3);
  }

  #[test]
  fn policies_select_different_roots_deliberately() {
    let (dates, amounts) = three_root_flow();
    let pick = |p| xirr(&dates, &amounts, None, None, Some(p)).unwrap();

    assert!((pick(RootPolicy::Lowest) - -0.571885951525731).abs() < 1e-8);
    assert!((pick(RootPolicy::ClosestToGuess) - -0.21924296785).abs() < 1e-8);
  }

  #[test]
  fn strict_compat_gives_up_exactly_where_a_spreadsheet_does() {
    // True IRR is -99.898%, below Calc's -0.99 rescan floor.
    let (dates, amounts) = cash_flow(&[("2020-01-01", -1000.), ("2021-01-01", 1.)]);

    let strict = xirr(
      &dates,
      &amounts,
      None,
      None,
      Some(RootPolicy::SpreadsheetCompat),
    );
    assert!(strict.unwrap().is_nan());

    let robust = xirr(&dates, &amounts, None, None, None).unwrap();
    assert!((robust - -0.9989809471).abs() < 1e-9, "got {robust}");
  }

  #[test]
  fn rejects_dates_before_the_first() {
    let (dates, amounts) = cash_flow(&[("2021-01-01", -100.), ("2020-01-01", 130.)]);
    assert!(xirr(&dates, &amounts, None, None, None).is_err());
  }

  #[test]
  fn rejects_guess_outside_the_domain() {
    let (dates, amounts) = cash_flow(&[("2020-01-01", -100.), ("2021-01-01", 130.)]);
    for bad in [-1.0, -2.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
      assert!(
        xirr(&dates, &amounts, Some(bad), None, None).is_err(),
        "guess {bad} should be rejected"
      );
    }
  }

  #[test]
  fn sign_changes_ignores_zeros_and_non_finite() {
    assert_eq!(sign_changes(&[-1., 0., 0., 3.]), 1);
    assert_eq!(sign_changes(&[-1., 2., -3.]), 2);
    assert_eq!(sign_changes(&[1., 2., 3.]), 0);
    assert_eq!(sign_changes(&[]), 0);
    assert_eq!(sign_changes(&[-1., f64::NAN, 3.]), 1);
  }

  #[test]
  fn xnpv_is_zero_at_the_returned_rate() {
    let (dates, amounts) = cash_flow(&[
      ("2020-01-01", -1000.),
      ("2021-01-01", 750.),
      ("2022-01-01", 500.),
    ]);
    let rate = xirr(&dates, &amounts, None, None, None).unwrap();
    let residual = xnpv(rate, &dates, &amounts, None).unwrap();
    assert!(residual.abs() < 1e-9, "residual {residual}");
  }
}
