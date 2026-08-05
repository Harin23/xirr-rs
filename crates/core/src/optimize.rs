//! Root finders.
//!
//! `newton_excel_order` is a port of the Newton loop in
//! `AnalysisAddIn::getXirr`, `main/scaddins/source/analysis/financial.cxx`,
//! Apache OpenOffice, Apache License 2.0. See NOTICE.
//!
//! `brentq` is a port of scipy/optimize/Zeros/brentq.c (BSD-3-Clause).

// ---------------------------------------------------------------------------
// Spreadsheet-parity constants. Every one of these is load-bearing. Changing
// any of them changes which root is returned for multiple-root cash flows.
// ---------------------------------------------------------------------------

/// `fMaxEps` upstream. Absolute, not relative - matching this exactly is the
/// point, even though an absolute epsilon is otherwise poor practice.
const EXCEL_EPS: f64 = 1e-10;
/// `nMaxIter` upstream.
const EXCEL_MAX_ITER: u32 = 50;
/// Upstream scans `nIterScan` in `0..200`, seeding rate at
/// `-0.99 + (nIterScan - 1) * 0.01` from the second pass on, i.e. a fixed
/// 0.01 grid over [-0.99, +0.99].
const EXCEL_MAX_SCAN: u32 = 200;
const EXCEL_SCAN_START: f64 = -0.99;
const EXCEL_SCAN_STEP: f64 = 0.01;

/// Newton's method in the exact order a spreadsheet performs it.
///
/// Returns `NaN` where a spreadsheet returns #NUM!.
///
/// Three details are deliberately preserved and must not be "improved":
///
/// 1. The continue test is `step > eps && |value| > eps`, so the loop stops as
///    soon as EITHER is small. That is a weak criterion which can terminate at
///    a non-root on a near-flat stretch. It is why spreadsheets occasionally
///    report a nonsense IRR - and reproducing that is the job.
/// 2. The rescan grid is fixed-step 0.01 over [-0.99, +0.99]. A geometric or
///    wider grid finds more roots but lands on different ones.
/// 3. The first attempt uses the caller's `guess` (default 0.1) before any
///    rescan, so `guess` is part of the contract, not a perf hint.
pub fn newton_excel_order<Func>(guess: f64, fd: &Func) -> f64
where
  Func: Fn(f64) -> (f64, f64),
{
  if !guess.is_finite() || guess <= -1.0 {
    return f64::NAN;
  }

  let mut rate = guess;
  let mut value;
  let mut cont = false;

  for scan in 0..EXCEL_MAX_SCAN {
    if scan >= 1 {
      rate = EXCEL_SCAN_START + (scan as f64 - 1.0) * EXCEL_SCAN_STEP;
    }

    let mut iter = 0u32;
    loop {
      let (v, deriv) = fd(rate);
      value = v;
      // Upstream divides unconditionally; a zero derivative yields +/-inf,
      // which the finiteness check below turns into another rescan.
      let new_rate = rate - value / deriv;
      let rate_eps = (new_rate - rate).abs();
      rate = new_rate;
      cont = rate_eps > EXCEL_EPS && value.abs() > EXCEL_EPS;
      iter += 1;
      if !cont || iter >= EXCEL_MAX_ITER {
        break;
      }
    }

    if !rate.is_finite() || !value.is_finite() {
      cont = true;
    }
    if !cont {
      break;
    }
  }

  if cont {
    f64::NAN
  } else {
    rate
  }
}

/// Newton's method that stops on a caller-supplied residual tolerance.
///
/// Used only by the robust fallback, never on the parity path - Phase 1 has
/// its own convergence test that must not be second-guessed. The tolerance is
/// a parameter rather than a constant so the single source of truth stays in
/// `xirr.rs`, where it can be documented next to the other tolerances.
pub fn newton_to_residual<FD>(start: f64, fd: &FD, residual_tol: f64) -> f64
where
  FD: Fn(f64) -> (f64, f64),
{
  const MAX_ITER: u32 = 50;
  /// Below this the step has stopped moving; either we are at a root or we
  /// are stuck on a flat spot, and the caller's `is_root` check decides which.
  const STALLED_STEP: f64 = 1e-12;

  let mut rate = start;
  for _ in 0..MAX_ITER {
    let (value, deriv) = fd(rate);
    if !value.is_finite() || deriv == 0.0 {
      return f64::NAN;
    }
    if value.abs() <= residual_tol {
      return rate;
    }
    let step = value / deriv;
    rate -= step;
    if step.abs() < STALLED_STEP {
      return rate;
    }
  }
  f64::NAN
}

/// Sign-change brackets for XNPV over `(-1, 1e6]`.
///
/// Dense and linear near zero where realistic rates live, geometric above 1.0
/// so pathological cash flows with four-digit IRRs are still bracketed without
/// the grid costing a million evaluations.
pub fn find_brackets<Func>(f: &Func, max_rate: f64) -> Vec<(f64, f64)>
where
  Func: Fn(f64) -> f64,
{
  /// Just inside the domain boundary at -1, where XNPV goes to infinity.
  const LO: f64 = -0.999_999_999_9;
  /// Grid spacing below +100%, where realistic rates live.
  const FINE_STEP: f64 = 0.005;
  /// Above +100% the grid grows geometrically: 5% wider each step, so the
  /// whole range up to `max_rate` costs a few hundred evaluations, not a
  /// million.
  const COARSE_GROWTH: f64 = 1.05;

  let mut grid = Vec::with_capacity(1024);
  grid.push(LO);
  let mut x = -0.99;
  while x <= 1.0 {
    grid.push(x);
    x += FINE_STEP;
  }
  let mut x = 1.0;
  while x < max_rate {
    grid.push(x);
    x *= COARSE_GROWTH;
  }
  grid.push(max_rate);

  let mut out = Vec::new();
  let mut prev_x = grid[0];
  let mut prev_f = f(prev_x);
  for &cx in &grid[1..] {
    let cf = f(cx);
    if prev_f.is_finite() && cf.is_finite() && prev_f != 0.0 && prev_f.signum() != cf.signum() {
      out.push((prev_x, cx));
    }
    prev_x = cx;
    prev_f = cf;
  }
  out
}

/// Brent's method. Bracketed, so convergence is guaranteed given a sign change.
/// Unlike the previous revision this returns the root unconditionally and
/// leaves acceptance to the caller, whose tolerance is cash-flow relative.
pub fn brentq<Func>(f: &Func, xa: f64, xb: f64, iter: usize) -> f64
where
  Func: Fn(f64) -> f64,
{
  const XTOL: f64 = 2e-14;
  const RTOL: f64 = 8.881_784_197_001_252e-16;

  let mut xpre = xa;
  let mut xcur = xb;
  let (mut xblk, mut fblk, mut spre, mut scur) = (0., 0., 0., 0.);

  let mut fpre = f(xpre);
  let mut fcur = f(xcur);

  if !fpre.is_finite() || !fcur.is_finite() {
    return f64::NAN;
  }
  if fpre == 0. {
    return xpre;
  }
  if fcur == 0. {
    return xcur;
  }
  if fpre.signum() == fcur.signum() {
    return f64::NAN;
  }

  for _ in 0..iter {
    if fpre != 0. && fcur != 0. && fpre.signum() != fcur.signum() {
      xblk = xpre;
      fblk = fpre;
      spre = xcur - xpre;
      scur = spre;
    }

    if fblk.abs() < fcur.abs() {
      xpre = xcur;
      xcur = xblk;
      xblk = xpre;
      fpre = fcur;
      fcur = fblk;
      fblk = fpre;
    }

    let delta = (XTOL + RTOL * xcur.abs()) / 2.;
    let sbis = (xblk - xcur) / 2.;

    if fcur == 0. || sbis.abs() < delta {
      return xcur;
    }

    if spre.abs() > delta && fcur.abs() < fpre.abs() {
      let stry = if xpre == xblk {
        -fcur * (xcur - xpre) / (fcur - fpre)
      } else {
        let dpre = (fpre - fcur) / (xpre - xcur);
        let dblk = (fblk - fcur) / (xblk - xcur);
        -fcur * (fblk * dblk - fpre * dpre) / (dblk * dpre * (fblk - fpre))
      };

      if 2. * stry.abs() < spre.abs().min(3. * sbis.abs() - delta) {
        spre = scur;
        scur = stry;
      } else {
        spre = sbis;
        scur = sbis;
      }
    } else {
      spre = sbis;
      scur = sbis;
    }

    xpre = xcur;
    fpre = fcur;
    if scur.abs() > delta {
      xcur += scur;
    } else {
      xcur += if sbis > 0. { delta } else { -delta }
    }

    fcur = f(xcur);
  }

  f64::NAN
}

// ---------------------------------------------------------------------------
// Legacy solvers retained for the PERIODIC code path (irr / mirr / npv), which
// this change does not touch. They are NOT used by xirr - the parity path uses
// `newton_excel_order` and the relative-tolerance `newton_to_residual` instead.
// Do not point xirr at these: their tolerances are absolute.
// ---------------------------------------------------------------------------

const MAX_ERROR: f64 = 1e-9;
const MAX_ITERATIONS: u32 = 50;

pub fn newton_raphson<Func, Deriv>(start: f64, f: &Func, d: &Deriv) -> f64
where
  Func: Fn(f64) -> f64,
  Deriv: Fn(f64) -> f64,
{
  // x[n + 1] = x[n] - f(x[n])/f'(x[n])

  let mut x = start;

  for _ in 0..MAX_ITERATIONS {
    let y = f(x);

    if y.abs() < MAX_ERROR {
      return x;
    }

    let delta = y / d(x);

    if delta.abs() < MAX_ERROR {
      return x - delta;
    }

    x -= delta;
  }

  f64::NAN
}

// a slightly modified version that accepts a callback function that
// calculates the result and the derivative at once
pub fn newton_raphson_with_default_deriv<Func>(start: f64, f: Func) -> f64
where
  Func: Fn(f64) -> f64,
{
  // deriv = (f(x + e) - f(x - e))/((x + e) - x)
  // multiply denominator by 2 for faster convergence

  // https://programmingpraxis.com/2012/01/13/excels-xirr-function/

  let df = |x| (f(x + MAX_ERROR) - f(x - MAX_ERROR)) / (2.0 * MAX_ERROR);
  newton_raphson(start, &f, &df)
}

pub fn brentq_grid_search<'a, Func>(
  breakpoints: &'a [&[f64]],
  f: &'a Func,
) -> impl Iterator<Item = f64> + 'a
where
  Func: Fn(f64) -> f64 + 'a,
{
  breakpoints
    .iter()
    .flat_map(|x| x.windows(2).map(|pair| brentq(f, pair[0], pair[1], 100)))
    .filter(|r| r.is_finite() && f(*r).abs() < 1e-3)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn excel_order_finds_the_leftmost_of_three_roots() {
    // XNPV for [-1000, 3000, -2500, 600] on 2015-01-01 .. 2018-01-01.
    // Deltas are real ACT/365 year fractions - 2016 is a leap year, so
    // idealised [0,1,2,3] gives a materially different root (-0.5696) and
    // would not match the spreadsheet fixture.
    let d = [0.0, 1.0, 2.002_739_726_027_397_4, 3.002_739_726_027_397_4];
    let a = [-1000., 3000., -2500., 600.];
    let fd = |r: f64| {
      if r <= -1.0 {
        return (f64::INFINITY, f64::INFINITY);
      }
      let b = 1.0 + r;
      a.iter().zip(d.iter()).fold((0., 0.), |acc, (p, e)| {
        let y0 = p * b.powf(-e);
        (acc.0 + y0, acc.1 + y0 * -e / b)
      })
    };
    let r = newton_excel_order(0.1, &fd);
    assert!((r - -0.571885951525731).abs() < 1e-9, "got {r}");
  }

  #[test]
  fn excel_order_rejects_invalid_guess() {
    let fd = |_r: f64| (1.0, 1.0);
    assert!(newton_excel_order(-1.0, &fd).is_nan());
    assert!(newton_excel_order(f64::NAN, &fd).is_nan());
  }

  #[test]
  fn brackets_capture_every_sign_change() {
    let f = |x: f64| (x - 0.05) * (x - 0.5) * (x - 3.0);
    let b = find_brackets(&f, 1.0e6);
    assert_eq!(b.len(), 3);
    for (lo, hi) in b {
      let r = brentq(&f, lo, hi, 100);
      assert!(f(r).abs() < 1e-9);
    }
  }
}
