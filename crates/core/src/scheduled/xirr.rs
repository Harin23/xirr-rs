//! XIRR with explicit, spreadsheet-compatible multiple-root handling.
//!
//! The default policy reproduces the answer Excel / Google Sheets / LibreOffice
//! Calc give, including which root they pick when several exist, and only
//! deviates when those engines would return #NUM!.
//!
//! Phase 1 is a port of `AnalysisAddIn::getXirr` from
//! `main/scaddins/source/analysis/financial.cxx` in Apache OpenOffice
//! (Apache License 2.0 - see NOTICE). LibreOffice ships the same algorithm.

use super::{year_fraction, DayCount};
use crate::{
  models::{validate, validate_length, DateLike, InvalidPaymentsError},
  optimize::{brentq, find_brackets, newton_excel_order, newton_residual},
  utils::{cashflow_scale, is_a_good_rate_scaled},
};

/// The guess every spreadsheet uses when the caller omits one.
pub const DEFAULT_GUESS: f64 = 0.1;

/// Relative residual gate. Absolute tolerances make correctness depend on
/// whether you denominate in dollars or cents, so every acceptance test is
/// scaled by the gross size of the cash flow.
pub const RESIDUAL_REL_TOL: f64 = 1e-9;

/// What to do when XNPV(r) = 0 has more than one solution in (-1, inf).
///
/// This is a *business* decision, not an implementation detail, so it is
/// surfaced as an explicit type rather than left to fall out of solver order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RootPolicy {
  /// Exactly what a spreadsheet does, including returning nothing where a
  /// spreadsheet shows #NUM!. Use when output must tie out to a workbook.
  SpreadsheetCompat,

  /// Spreadsheet answer whenever a spreadsheet has one; otherwise fall back to
  /// bracketed root finding. Never contradicts a spreadsheet, strictly more
  /// likely to return an answer. This is the default.
  #[default]
  SpreadsheetThenRobust,

  /// Enumerate every root and return the smallest. Deterministic and
  /// conservative; ignores spreadsheet convention. Suited to reporting where
  /// understating return is the safe failure direction.
  Lowest,

  /// Enumerate every root and return the one nearest `guess`. Deterministic,
  /// and lets a caller who knows the expected magnitude steer the answer.
  ClosestToGuess,
}

pub fn xirr(
  dates: &[DateLike],
  amounts: &[f64],
  guess: Option<f64>,
  day_count: Option<DayCount>,
  policy: Option<RootPolicy>,
) -> Result<f64, InvalidPaymentsError> {
  validate(amounts, Some(dates))?;
  validate_dates_start(dates)?;

  let policy = policy.unwrap_or_default();
  let guess = guess.unwrap_or(DEFAULT_GUESS);
  if !guess.is_finite() || guess <= -1.0 {
    return Err(InvalidPaymentsError::new(
      "guess must be a finite number greater than -1",
    ));
  }

  let deltas = &day_count_factor(dates, day_count);
  let scale = cashflow_scale(amounts);

  let f = |rate| xnpv_result(amounts, deltas, rate);
  let fd = |rate| xnpv_result_with_deriv(amounts, deltas, rate);

  // Phase 1 runs first for every policy that claims parity.
  let spreadsheet = newton_excel_order(guess, &fd);

  match policy {
    RootPolicy::SpreadsheetCompat => Ok(spreadsheet),

    RootPolicy::SpreadsheetThenRobust => {
      // Return the spreadsheet's rate verbatim, WITHOUT re-checking its
      // residual. Rejecting it here is what would let us print 200% where
      // Excel prints 5%. If the caller wants the residual, they can ask for
      // it via `xnpv` and decide themselves.
      if spreadsheet.is_finite() {
        return Ok(spreadsheet);
      }
      // Only now, where a spreadsheet gives up entirely, do we do better.
      let roots = all_roots(amounts, deltas);
      if let Some(r) = pick_closest(&roots, guess) {
        return Ok(r);
      }
      for seed in [guess, 0.0, -0.5, -0.9, 0.5, 2.0, 10.0] {
        let r = newton_residual(seed, &fd, &f, scale);
        if is_a_good_rate_scaled(r, &f, scale) {
          return Ok(r);
        }
      }
      Ok(f64::NAN)
    }

    RootPolicy::Lowest | RootPolicy::ClosestToGuess => {
      let roots = all_roots(amounts, deltas);
      if roots.is_empty() {
        return Ok(spreadsheet);
      }
      Ok(match policy {
        RootPolicy::Lowest => roots[0],
        _ => pick_closest(&roots, guess).unwrap_or(f64::NAN),
      })
    }
  }
}

/// Every root of XNPV in `(-1, hi]`, ascending. Exposed publicly because
/// "there are three IRRs and the spreadsheet picked the leftmost" is far more
/// actionable for a fund accountant than a single silent number.
pub fn xirr_all_roots(
  dates: &[DateLike],
  amounts: &[f64],
  day_count: Option<DayCount>,
) -> Result<Vec<f64>, InvalidPaymentsError> {
  validate(amounts, Some(dates))?;
  validate_dates_start(dates)?;
  let deltas = &day_count_factor(dates, day_count);
  Ok(all_roots(amounts, deltas))
}

fn all_roots(amounts: &[f64], deltas: &[f64]) -> Vec<f64> {
  let f = |rate| xnpv_result(amounts, deltas, rate);
  let scale = cashflow_scale(amounts);

  let mut roots: Vec<f64> = find_brackets(&f)
    .into_iter()
    .map(|(a, b)| brentq(&f, a, b, 100))
    .filter(|r| is_a_good_rate_scaled(*r, &f, scale))
    .collect();

  roots.sort_by(|a, b| a.partial_cmp(b).unwrap());
  roots.dedup_by(|a, b| (*a - *b).abs() <= 1e-9);
  roots
}

fn pick_closest(roots: &[f64], guess: f64) -> Option<f64> {
  roots.iter().copied().min_by(|a, b| {
    (*a - guess)
      .abs()
      .partial_cmp(&(*b - guess).abs())
      .unwrap()
      .then(a.partial_cmp(b).unwrap())
  })
}

/// Spreadsheets raise #NUM! if any date precedes the first one rather than
/// silently reordering. Matching that is part of parity: a caller who hands us
/// unsorted input is getting a different answer from their workbook, and they
/// need to know.
fn validate_dates_start(dates: &[DateLike]) -> Result<(), InvalidPaymentsError> {
  match dates.split_first() {
    Some((first, rest)) if rest.iter().any(|d| d < first) => Err(InvalidPaymentsError::new(
      "all dates must be on or after the first date",
    )),
    _ => Ok(()),
  }
}

/// Closed form for the two-payment case. Exact, no iteration. Kept out of the
/// main path so it can never disagree with the spreadsheet by a few ULP.
pub fn xirr_analytical_2(amounts: &[f64], deltas: &[f64]) -> f64 {
  (-amounts[1] / amounts[0]).powf(1. / (deltas[1] - deltas[0])) - 1.0
}

pub fn xnpv(
  rate: f64,
  dates: &[DateLike],
  amounts: &[f64],
  day_count: Option<DayCount>,
) -> Result<f64, InvalidPaymentsError> {
  validate_length(amounts, dates)?;
  let deltas = &day_count_factor(dates, day_count);
  Ok(xnpv_result(amounts, deltas, rate))
}

pub fn sign_changes(v: &[f64]) -> i32 {
  v.iter()
    .filter(|x| x.is_finite() && **x != 0.0)
    .collect::<Vec<_>>()
    .windows(2)
    .map(|p| (p[0].signum() != p[1].signum()) as i32)
    .sum()
}

/// Reference date is `dates[0]` - the FIRST payment in input order, not the
/// earliest. Spreadsheets do the same. Using `min()` leaves the roots
/// unchanged but rescales the objective by `(1+r)^k`, which changes the
/// Newton trajectory and therefore which root you land on: measured, that
/// alone breaks parity on ~10% of multiple-root inputs.
fn day_count_factor(dates: &[DateLike], day_count: Option<DayCount>) -> Vec<f64> {
  let d0 = &dates[0];
  let dc = day_count.unwrap_or_default();
  dates.iter().map(|d| year_fraction(d0, d, dc)).collect()
}

/// XNPV. Uses `powf`, deliberately: the `exp2(log2(a)*b)` shortcut is faster
/// but loses a few ULP, and against Phase 1's 1e-10 ABSOLUTE epsilon those
/// ULP can change which root the iteration converges to.
fn xnpv_result(payments: &[f64], deltas: &[f64], rate: f64) -> f64 {
  if rate <= -1.0 {
    return f64::INFINITY;
  }
  let base = 1.0 + rate;
  payments
    .iter()
    .zip(deltas)
    .map(|(p, &e)| p * base.powf(-e))
    .sum()
}

fn xnpv_result_with_deriv(payments: &[f64], deltas: &[f64], rate: f64) -> (f64, f64) {
  if rate <= -1.0 {
    return (f64::INFINITY, f64::INFINITY);
  }
  let base = 1.0 + rate;
  payments.iter().zip(deltas).fold((0.0, 0.0), |acc, (p, e)| {
    let y0 = p * base.powf(-e);
    let y1 = y0 * -e / base;
    (acc.0 + y0, acc.1 + y1)
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::models::DateLike;
  use std::str::FromStr;

  fn cf(rows: &[(&str, f64)]) -> (Vec<DateLike>, Vec<f64>) {
    (
      rows
        .iter()
        .map(|(d, _)| DateLike::from_str(d).unwrap())
        .collect(),
      rows.iter().map(|(_, a)| *a).collect(),
    )
  }

  #[test]
  fn matches_spreadsheet_on_multiple_roots() {
    // Three roots exist: -0.5719, -0.2192, +0.7959.
    // LibreOffice Calc returns the first. "nearest to guess" would return
    // -0.2192 and "lowest" only agrees here by luck, so neither is a policy.
    let (d, a) = cf(&[
      ("2015-01-01", -1000.),
      ("2016-01-01", 3000.),
      ("2017-01-01", -2500.),
      ("2018-01-01", 600.),
    ]);
    let r = xirr(&d, &a, None, None, None).unwrap();
    assert!((r - -0.571885951525731).abs() < 1e-9, "got {r}");

    let roots = xirr_all_roots(&d, &a, None).unwrap();
    assert_eq!(roots.len(), 3);
  }

  #[test]
  fn policies_can_disagree_deliberately() {
    let (d, a) = cf(&[
      ("2015-01-01", -1000.),
      ("2016-01-01", 3000.),
      ("2017-01-01", -2500.),
      ("2018-01-01", 600.),
    ]);
    let closest = xirr(&d, &a, None, None, Some(RootPolicy::ClosestToGuess)).unwrap();
    assert!((closest - -0.21924296785).abs() < 1e-8, "got {closest}");
  }

  #[test]
  fn strict_compat_returns_nan_where_spreadsheet_shows_num() {
    // True IRR is -99.898%, below Calc's -0.99 rescan floor.
    let (d, a) = cf(&[("2020-01-01", -1000.), ("2021-01-01", 1.)]);
    assert!(
      xirr(&d, &a, None, None, Some(RootPolicy::SpreadsheetCompat))
        .unwrap()
        .is_nan()
    );
    let robust = xirr(&d, &a, None, None, None).unwrap();
    assert!((robust - -0.9989809471).abs() < 1e-9, "got {robust}");
  }

  #[test]
  fn rejects_dates_before_the_first() {
    let (d, a) = cf(&[("2021-01-01", -100.), ("2020-01-01", 130.)]);
    assert!(xirr(&d, &a, None, None, None).is_err());
  }

  #[test]
  fn rejects_guess_at_or_below_minus_one() {
    let (d, a) = cf(&[("2020-01-01", -100.), ("2021-01-01", 130.)]);
    assert!(xirr(&d, &a, Some(-1.0), None, None).is_err());
  }

  #[test]
  fn sign_changes_ignores_zeros() {
    assert_eq!(sign_changes(&[-1., 0., 0., 3.]), 1);
    assert_eq!(sign_changes(&[-1., 2., -3.]), 2);
  }
}
