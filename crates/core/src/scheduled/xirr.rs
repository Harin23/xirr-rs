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
//! # The three phases
//!
//! ```text
//!   xirr()
//!     |
//!     +-- Phase 0: NettedFlow::shape()                    <- always runs first
//!     |     Net by year fraction, drop zeros, count sign changes. Decides
//!     |     whether a root can exist at all, before any search happens.
//!     |
//!     +-- Phase 1: CashFlow::solve_like_a_spreadsheet()   <- parity path
//!     |     Newton from `guess` (default 0.1), then a fixed 0.01 rescan
//!     |     grid over [-0.99, +0.99]. Verbatim port of the algorithm
//!     |     Excel-compatible spreadsheets use. Returns NaN on #NUM!.
//!     |
//!     +-- Phase 2: CashFlow::solve_robustly()             <- only if 1 gave up
//!           Bracketed Brent in **log-rate space** `u = ln(1 + r)` over the
//!           whole representable domain, then multi-start Newton in the same
//!           space. Answers cash flows no spreadsheet can, but never
//!           *overrides* one.
//! ```
//!
//! Phase 1's result is returned **verbatim, without checking its residual**.
//! That is deliberate and is the core of the parity contract: re-checking is
//! exactly what would let this library print 200% where Excel prints 5%.
//! Callers who want to know whether the rate is a true root can call [`xnpv`]
//! on it. See `docs/ALGORITHM.md` for the full rationale.
//!
//! # Why log-rate space
//!
//! Substituting `u = ln(1 + r)` turns `XNPV` into
//! `G(u) = Σ aᵢ·exp(-u·δᵢ)`, which is well conditioned across the entire
//! domain. The whole of `r ∈ (-1, f64::MAX]` maps into
//! `u ∈ [-36.74, 709.79]`, so a bounded grid at fixed cost reaches *every
//! representable rate* - something no ceiling on a rate-space search can do.
//! Roots at `r ≈ 1e20` are ordinary here; in rate space one ULP at `1e20` is
//! ~16384, so no absolute step or residual test can ever be satisfied there.
//!
//! # Failure is typed, not `NaN`
//!
//! [`xirr`] keeps its historical `NaN`-means-no-answer signature. Use
//! [`xirr_outcome`] to find out *which* non-answer occurred: a cash flow that
//! provably has no root ([`XirrOutcome::NoRootExists`]) and one the solver
//! merely failed on ([`XirrOutcome::DidNotConverge`]) are different facts with
//! different operational responses, and collapsing both to `NaN` is a defect
//! in a financial system.
//!
//! # Attribution
//!
//! Phase 1 is a port of `AnalysisAddIn::getXirr` from
//! `main/scaddins/source/analysis/financial.cxx` in Apache OpenOffice,
//! Apache License 2.0. See the NOTICE file at the repository root.

use super::{year_fraction, DayCount};
use crate::{
  models::{validate, validate_length, DateLike, InvalidPaymentsError},
  optimize::{brentq, find_crossings, newton_excel_order, newton_to_residual, Crossing},
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
pub const DISTINCT_ROOT_TOL: f64 = 1e-7;

/// How far apart two roots must be in **log-rate space** to count as
/// different.
///
/// A fixed step in `u = ln(1 + r)` is a fixed *relative* step in `1 + r`, so
/// unlike [`DISTINCT_ROOT_TOL`] this stays meaningful at every magnitude. At
/// `r ≈ 0` the two are numerically interchangeable (`Δu ≈ Δr`); at `r ≈ 1e20`
/// the rate-space test can never fire, because adjacent `f64` values there are
/// ~16384 apart. Both are applied: this one does the work, the rate-space one
/// is retained as a second pass so behaviour at ordinary rates is unchanged.
const DISTINCT_ROOT_U_TOL: f64 = 1e-7;

/// Lower end of the searched log-rate domain: `ln(2^-53)`.
///
/// This is not a policy choice, it is the representability boundary.
/// `nextafter(-1.0, 0.0)` is `-1 + 2^-53`, so `2^-53` is the smallest `1 + r`
/// that any `f64` rate strictly greater than `-1` can produce. Nothing below
/// this can be returned, so nothing below this is worth searching.
pub const MIN_SEARCHED_LOG_RATE: f64 = -36.736_800_569_677_1;

/// Upper end of the searched log-rate domain: `ln(f64::MAX)`.
///
/// Also a representability boundary rather than a policy choice: `expm1` of
/// anything larger overflows to infinity. The previous revision capped the
/// search at a rate of `1e12`, which is `u ≈ 27.6` - it could not reach the
/// (unique, real, and perfectly ordinary) roots near `u ≈ 47` that a cash
/// flow netting to a small day-zero outflow produces.
pub const MAX_SEARCHED_LOG_RATE: f64 = 709.782_712_893_384;

/// Seeds for the multi-start Newton fallback, in **rate** space, tried in
/// order after the caller's guess. Spread across the plausible range so a root
/// that bracketing missed still gets a chance.
const FALLBACK_SEEDS: [f64; 6] = [0.0, -0.5, -0.9, 0.5, 2.0, 10.0];

/// Further seeds, in **log-rate** space, covering the magnitudes no rate-space
/// seed can express. Newton in `u` from `u = 400` is an ordinary iteration;
/// the same point in rate space is `1e173`.
const FALLBACK_LOG_SEEDS: [f64; 8] = [-20.0, -5.0, 5.0, 20.0, 60.0, 150.0, 350.0, 650.0];

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

/// The result of a solve, with every non-answer named.
///
/// [`xirr`] returns `f64` and uses `NaN` for all four failure modes below.
/// That is fine for a spreadsheet cell and wrong for a ledger: "this cash flow
/// cannot have an IRR" and "our solver gave up" call for different operational
/// responses, and one of them is a data-quality bug in the caller's system.
/// [`xirr_outcome`] returns this instead.
#[derive(Debug, Clone, PartialEq)]
pub enum XirrOutcome {
  /// A single rate solves `XNPV(r) = 0`, and this is it.
  ///
  /// The rate has passed the residual test in [`RESIDUAL_REL_TOL`] under every
  /// policy, including the two spreadsheet ones. Parity is preserved by
  /// *labelling*, not by filtering: a spreadsheet rate that fails the test is
  /// still returned by [`xirr`], and reported here as
  /// [`XirrOutcome::UnverifiedRate`] rather than as a root.
  Root(f64),

  /// `XNPV(r) = 0` has more than one solution. `selected` is what [`xirr`]
  /// returns for the same arguments; `all` is every root found, ascending.
  ///
  /// The single number is a **convention**, not a fact. Report the ambiguity.
  MultipleRoots { selected: f64, all: Vec<f64> },

  /// A rate was produced, but it is **not** a verified root: `|XNPV(rate)|` is
  /// not near zero relative to the terms summed at that rate.
  ///
  /// This is the spreadsheet's weak convergence test surfacing. Spreadsheets
  /// stop as soon as *either* the step *or* the residual is small, both
  /// against an absolute epsilon, so they can return a point on a flat stretch
  /// or on an asymptote that `XNPV` never actually reaches. That behaviour is
  /// reproduced faithfully rather than corrected - see the parity contract in
  /// the module docs - and named here rather than passed off as a root.
  ///
  /// `rate` is what [`xirr`] returns for the same arguments, so parity is
  /// intact. `roots` is what enumeration actually verified: empty when nothing
  /// did, and **non-empty when the spreadsheet's answer is simply not one of
  /// them**, which is the case worth escalating.
  UnverifiedRate { rate: f64, roots: Vec<f64> },

  /// No rate can solve this cash flow, and that is provable rather than
  /// suspected: after netting flows that share a date and discarding zeros,
  /// every remaining amount has the same sign, so every term of `XNPV` has
  /// the same sign at every rate and the sum never reaches zero.
  ///
  /// This is an **input** problem. A spreadsheet shows `#NUM!` here too.
  NoRootExists,

  /// A root may exist - there is at least one sign change - but the solver did
  /// not produce one that passes the residual test.
  ///
  /// This also covers the case where a root provably exists but lies outside
  /// the representable range (see [`MAX_SEARCHED_LOG_RATE`]): with an even
  /// number of sign changes the two are not distinguishable without exact
  /// arithmetic, so they share a variant. Either way it is a **solver**
  /// outcome, not an input verdict, and should be escalated rather than
  /// treated as "no IRR".
  DidNotConverge,

  /// [`RootPolicy::SpreadsheetCompat`] only: the spreadsheet algorithm gave up
  /// (`#NUM!`) on a cash flow that is **not** provably rootless.
  ///
  /// Distinct from [`XirrOutcome::DidNotConverge`] because nothing here failed:
  /// the `#NUM!` was faithfully reproduced, and rerunning under
  /// [`RootPolicy::SpreadsheetThenRobust`] will very often return a rate.
  /// `[-1000, +1]` a year apart is the canonical example - every spreadsheet
  /// reports `#NUM!`, and the IRR is -99.898%.
  SpreadsheetNumError,
}

impl XirrOutcome {
  /// The rate, if one was produced. `None` for every failure variant.
  ///
  /// Named `rate` rather than `unwrap_or_nan` on purpose: the point of this
  /// type is that the `f64` and the failure are not the same channel.
  pub fn rate(&self) -> Option<f64> {
    match self {
      Self::Root(r) => Some(*r),
      Self::MultipleRoots { selected, .. } => Some(*selected),
      // A rate *was* produced; the point of the variant is that it is not
      // labelled a root, not that it is withheld.
      Self::UnverifiedRate { rate, .. } => Some(*rate),
      _ => None,
    }
  }

  /// How many roots were found. `0` for every failure variant, `1` for
  /// [`XirrOutcome::Root`].
  pub fn root_count(&self) -> usize {
    match self {
      Self::Root(_) => 1,
      Self::MultipleRoots { all, .. } => all.len(),
      _ => 0,
    }
  }

  /// Whether the IRR is ambiguous, i.e. the returned rate is one of several
  /// mathematically valid answers.
  pub fn is_ambiguous(&self) -> bool {
    matches!(self, Self::MultipleRoots { .. })
  }
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
  Ok(flow.solve(guess, policy.unwrap_or_default()))
}

/// [`xirr`], with every non-answer named instead of collapsed to `NaN`.
///
/// The rate it reports is **identical** to [`xirr`]'s for the same arguments;
/// this function only adds the information `f64` cannot carry. Prefer it
/// anywhere the difference between "this input cannot have an IRR" and "we
/// could not find the IRR" has to reach a human or a ledger.
///
/// ```ignore
/// match xirr_outcome(&dates, &amounts, None, None, None)? {
///     XirrOutcome::Root(r)                        => post(r),
///     XirrOutcome::MultipleRoots { selected, all } => post_with_caveat(selected, &all),
///     XirrOutcome::NoRootExists                   => reject_as_data_error(),
///     XirrOutcome::DidNotConverge                 => escalate(),
///     XirrOutcome::SpreadsheetNumError            => retry_without_parity(),
/// }
/// ```
pub fn xirr_outcome(
  dates: &[DateLike],
  amounts: &[f64],
  guess: Option<f64>,
  day_count: Option<DayCount>,
  policy: Option<RootPolicy>,
) -> Result<XirrOutcome, InvalidPaymentsError> {
  let flow = CashFlow::new(dates, amounts, day_count)?;
  let guess = checked_guess(guess)?;
  let policy = policy.unwrap_or_default();
  let rate = flow.solve(guess, policy);

  if !rate.is_finite() {
    return Ok(match flow.netted.shape() {
      Shape::NoRootExists => XirrOutcome::NoRootExists,
      // Parity gave up without the robust path ever being consulted, so
      // "did not converge" would blame a solver that never ran.
      _ if policy == RootPolicy::SpreadsheetCompat => XirrOutcome::SpreadsheetNumError,
      _ => XirrOutcome::DidNotConverge,
    });
  }

  // Only a flow with two or more sign changes can have two or more roots
  // (Descartes), so the common case never pays for enumeration.
  if flow.netted.sign_changes() < 2 {
    return Ok(if flow.is_root(rate) {
      XirrOutcome::Root(rate)
    } else {
      XirrOutcome::UnverifiedRate {
        rate,
        roots: Vec::new(),
      }
    });
  }

  let all = flow.roots();
  // Verify before labelling. A rate that reached here is finite, which is the
  // only thing the gate above established; whether it solves the cash flow is
  // a separate question and the one the caller is actually asking.
  if !flow.is_root(rate) {
    return Ok(XirrOutcome::UnverifiedRate { rate, roots: all });
  }
  Ok(if all.len() > 1 {
    XirrOutcome::MultipleRoots {
      selected: rate,
      all,
    }
  } else {
    XirrOutcome::Root(rate)
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
  Ok(CashFlow::unvalidated(amounts, dates, day_count).xnpv(rate))
}

/// Sign changes in the **date-netted** cash flow: the number that Descartes'
/// rule of signs actually bounds the root count by.
///
/// Two payments on the same date share a year fraction, so mathematically they
/// are one flow and must be summed before any sign is read off them. Netting
/// also makes the count independent of input order. Both matter, and a version
/// of this function that took only the amounts got both wrong:
///
/// - `[-11000, +20000]` on one date is `+9000`: **zero** sign changes, so no
///   root exists, where the raw amounts show one and imply a root does.
/// - `[-1, -5, +3]` on dates `d0, d5, d2` is `-1, +3, -5` once ordered:
///   **two** sign changes, where the raw amounts show one.
///
/// # What the count proves
///
/// | Sign changes | Roots in `(-1, inf)`      |
/// | ------------ | ------------------------- |
/// | `0`          | **none**, proved          |
/// | `1`          | **exactly one**, proved   |
/// | odd `n`      | at least one, at most `n` |
/// | even `n > 0` | `0, 2, .. n` - unproved   |
///
/// The odd/even split is Descartes' parity rule and it is what makes the
/// robust path's guarantee possible: see [`xirr_outcome`].
pub fn sign_changes(
  dates: &[DateLike],
  amounts: &[f64],
  day_count: Option<DayCount>,
) -> Result<usize, InvalidPaymentsError> {
  validate_length(amounts, dates)?;
  Ok(NettedFlow::new(&year_fractions(dates, day_count), amounts).sign_changes())
}

// ---------------------------------------------------------------------------
// CashFlow: the objective function and everything that operates on it
// ---------------------------------------------------------------------------

/// What the sign pattern of a netted cash flow proves about its roots.
///
/// Descartes' rule of signs, applied to `XNPV` written as a polynomial in
/// `x = 1/(1 + r) = exp(-u)`: the number of positive roots is at most the
/// number of sign changes in the coefficients ordered by exponent, and has the
/// same parity. Combined with the two limits
/// `sign G(+inf) = sign(first amount)` and `sign G(-inf) = sign(last amount)`,
/// this settles existence outright in two of the three cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
  /// Zero sign changes. Every term of `XNPV` keeps one sign at every rate, so
  /// the sum never reaches zero. Proved, not guessed.
  NoRootExists,
  /// An odd number of sign changes. The two limits therefore have opposite
  /// signs, so a bracket spanning the domain is guaranteed to contain a root
  /// and bisection cannot fail to find one.
  RootGuaranteed,
  /// An even, non-zero number of sign changes. There may be `0, 2, 4, ...`
  /// roots; nothing is proved and the grid has to do the work.
  RootPossible,
  /// A non-finite amount is present. Nothing can be proved about anything.
  Undefined,
}

/// The cash flow reduced to its mathematical content.
///
/// One entry per distinct year fraction, ascending, zeros discarded. This is
/// the object Descartes' rule applies to; the raw input is not, because two
/// payments on the same date share a `δ` and are therefore a single term of
/// the sum however they were entered.
///
/// Netting by year fraction rather than by calendar date is deliberate and
/// slightly stronger: under ACT conventions the two are the same relation,
/// and under the 30/360 family two distinct dates can map to the same `δ` -
/// in which case they *are* one term of `XNPV` and netting them is correct.
struct NettedFlow {
  /// `(δ, amount)`, `δ` strictly ascending, every amount finite and non-zero.
  terms: Vec<(f64, f64)>,
  /// Set when the input contained a non-finite amount, which invalidates
  /// every existence claim below.
  poisoned: bool,
}

impl NettedFlow {
  fn new(deltas: &[f64], amounts: &[f64]) -> Self {
    let poisoned = amounts.iter().any(|a| !a.is_finite()) || deltas.iter().any(|d| !d.is_finite());

    let mut pairs: Vec<(f64, f64)> = deltas
      .iter()
      .copied()
      .zip(amounts.iter().copied())
      .filter(|(d, a)| d.is_finite() && a.is_finite())
      .collect();
    // `total_cmp` rather than `partial_cmp().unwrap()`: a total order needs no
    // `expect`, and the solve path must not contain one.
    pairs.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut terms: Vec<(f64, f64)> = Vec::with_capacity(pairs.len());
    for (delta, amount) in pairs {
      match terms.last_mut() {
        Some(last) if last.0 == delta => last.1 += amount,
        _ => terms.push((delta, amount)),
      }
    }
    terms.retain(|(_, a)| *a != 0.0);

    Self { terms, poisoned }
  }

  fn sign_changes(&self) -> usize {
    self
      .terms
      .windows(2)
      .filter(|w| (w[0].1 > 0.0) != (w[1].1 > 0.0))
      .count()
  }

  fn shape(&self) -> Shape {
    if self.poisoned {
      Shape::Undefined
    } else {
      match self.sign_changes() {
        0 => Shape::NoRootExists,
        n if n % 2 == 1 => Shape::RootGuaranteed,
        _ => Shape::RootPossible,
      }
    }
  }

  /// `G(u) = Σ aᵢ·exp(-u·δᵢ)` and `G'(u)`, both multiplied by the same
  /// strictly positive constant.
  ///
  /// The constant is `exp(u·δ_ref)` where `δ_ref` is whichever end of the
  /// schedule carries the largest exponent at this `u`. Every exponent is then
  /// `≤ 0`, so **no term can overflow at any `u`**, and the smallest amount in
  /// the flow is always present at full magnitude.
  ///
  /// Scaling by a positive constant changes neither the sign of `G` nor the
  /// location of its roots, and it cancels out of the Newton step `G/G'`
  /// because both components carry the same factor. What it does change is the
  /// failure mode the naive form has: with no scaling, every discount factor
  /// underflows to exactly `0.0` at large `u` and `G` evaluates to `0.0`,
  /// which bisection reads as a root. Here the `δ_ref` term is exactly
  /// `a_ref·exp(0)`, so `G(u) == 0.0` can only ever mean genuine cancellation.
  fn scaled_g(&self, u: f64) -> (f64, f64) {
    let Some(&(first, _)) = self.terms.first() else {
      return (0.0, 0.0);
    };
    let d_ref = if u >= 0.0 {
      first
    } else {
      self.terms[self.terms.len() - 1].0
    };

    let mut value = 0.0;
    let mut deriv = 0.0;
    for &(delta, amount) in &self.terms {
      let term = amount * (-u * (delta - d_ref)).exp();
      value += term;
      deriv -= delta * term;
    }
    (value, deriv)
  }

  /// `Σ |aᵢ|·exp(-u·δᵢ)`, carrying the same positive factor as
  /// [`NettedFlow::scaled_g`], so the ratio of the two is scale-free.
  fn scaled_gross(&self, u: f64) -> f64 {
    let Some(&(first, _)) = self.terms.first() else {
      return 0.0;
    };
    let d_ref = if u >= 0.0 {
      first
    } else {
      self.terms[self.terms.len() - 1].0
    };
    self
      .terms
      .iter()
      .map(|&(delta, amount)| amount.abs() * (-u * (delta - d_ref)).exp())
      .sum()
  }
}

/// Log-rate abscissae spanning the whole representable domain.
///
/// A uniform step in `u` is a uniform *relative* step in `1 + r`, so one grid
/// resolves 0.01% differences near zero and factor-of-`e` differences at
/// `r = 1e300` at the same cost - roughly 3,100 nodes for the entire domain.
/// The equivalent rate-space grid does not exist at any node count.
///
/// Three bands, because root density is not uniform: essentially every rate a
/// financial system will ever see lies in `|u| ≤ 8`, i.e.
/// `r ∈ [-99.97%, +298,000%]`.
fn log_rate_grid() -> Vec<f64> {
  /// Half-width of the dense band, in `u`.
  const FINE_HALF_WIDTH: f64 = 8.0;
  /// Dense-band step. 0.01 in `u` is a 1.005x step in `1 + r`.
  const FINE_STEP: f64 = 0.01;
  /// Step below the dense band, covering rates within 0.03% of total loss.
  const LOW_STEP: f64 = 0.25;
  /// Step above the dense band. Two roots a factor of `e^0.5` apart at
  /// `r > 2980` would be missed by the grid; the multi-start Newton fallback
  /// and the guaranteed full-domain bracket both still apply there.
  const HIGH_STEP: f64 = 0.5;

  fn extend(grid: &mut Vec<f64>, from: f64, to: f64, step: f64) {
    let count = ((to - from) / step).ceil().max(0.0) as usize;
    // `from + i * step` rather than repeated addition: no drift, and the
    // node positions are identical on every platform and every run.
    grid.extend((0..count).map(|i| from + i as f64 * step));
  }

  let mut grid = Vec::with_capacity(3200);
  extend(&mut grid, MIN_SEARCHED_LOG_RATE, -FINE_HALF_WIDTH, LOW_STEP);
  extend(&mut grid, -FINE_HALF_WIDTH, FINE_HALF_WIDTH, FINE_STEP);
  extend(&mut grid, FINE_HALF_WIDTH, MAX_SEARCHED_LOG_RATE, HIGH_STEP);
  grid.push(MAX_SEARCHED_LOG_RATE);
  grid
}

/// A validated cash flow, ready to be solved.
///
/// Bundling the amounts with their year fractions and gross size means the
/// solver methods read like the mathematics, instead of threading three
/// parallel slices and a tolerance through every call.
struct CashFlow {
  amounts: Vec<f64>,
  /// Year fractions measured from the **first** payment in input order.
  deltas: Vec<f64>,
  /// The same flow netted by year fraction. Phase 0 and Phase 2 use this;
  /// Phase 1 must not, because summation order is part of parity.
  netted: NettedFlow,
}

impl CashFlow {
  fn new(
    dates: &[DateLike],
    amounts: &[f64],
    day_count: Option<DayCount>,
  ) -> Result<Self, InvalidPaymentsError> {
    validate(amounts, Some(dates))?;
    reject_dates_before_the_first(dates)?;
    Ok(Self::unvalidated(amounts, dates, day_count))
  }

  /// For [`xnpv`], which evaluates any rate against any schedule and so must
  /// not require both signs to be present.
  fn unvalidated(amounts: &[f64], dates: &[DateLike], day_count: Option<DayCount>) -> Self {
    let deltas = year_fractions(dates, day_count);
    Self {
      netted: NettedFlow::new(&deltas, amounts),
      amounts: amounts.to_vec(),
      deltas,
    }
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

  /// Is `rate` a true root?
  ///
  /// The test is the **relative cancellation** of the sum, not its absolute
  /// size:
  ///
  /// ```text
  ///   rho(r) = |SUM a_i (1+r)^-d_i| / SUM |a_i (1+r)^-d_i|
  /// ```
  ///
  /// `rho` lies in `[0, 1]`. Near zero, the terms genuinely cancelled and the
  /// rate is a root. Near one, nothing cancelled: the "sum" is just its one
  /// surviving term, which is what an *asymptote* looks like. That distinction
  /// is the whole job, and no absolute threshold can make it.
  ///
  /// Both are measured on [`NettedFlow`], in log-rate space. Netting is
  /// exact - two payments sharing a year fraction *are* one term of `XNPV` -
  /// and it is load-bearing here rather than an optimisation. A flow whose
  /// day-zero payments cancel (`-100, +100`) keeps them in the raw sum, where
  /// at a large rate they dominate `SUM |term|` while contributing nothing to
  /// the residual; the ratio is then tiny for every sufficiently large rate
  /// and the test passes on a cash flow that has no root at all. Measured on
  /// exactly that flow: `rho = 1.000` netted, `2e-13` un-netted.
  ///
  /// This also subsumes the rule it replaced, which took the larger of the
  /// gross and the *discounted* gross cash flow. That existed so the tolerance
  /// was never tighter than the floating-point noise of the quantity being
  /// measured - a real problem near total loss, where `XNPV` is a difference
  /// of terms around `1e30` and `f64` cannot express a residual below `1e14`.
  /// Dividing by `SUM |term|` does that natively and exactly: `fuzz/039` in the
  /// golden corpus has `|XNPV| = 1.008` against terms of `1.6e14`, i.e.
  /// `rho = 6e-15`, and is correctly accepted.
  ///
  /// Log-rate space, finally, cannot overflow - every term there is scaled by
  /// the dominant one - so there is no second path and no fallback.
  fn is_root(&self, rate: f64) -> bool {
    if !rate.is_finite() || rate <= -1.0 {
      return false;
    }
    // A single non-finite amount makes `xnpv` NaN at *every* rate, so no rate
    // is verifiable. `netted` has dropped that amount, so consulting it would
    // answer a question about a cash flow the caller never passed - and answer
    // it confidently, since the sanitised flow is perfectly well behaved.
    if self.netted.poisoned {
      return false;
    }
    let u = to_log_rate(rate);
    let value = self.netted.scaled_g(u).0;
    let magnitude = self.netted.scaled_gross(u);
    // `magnitude > 0.0` rather than `is_finite`: a flow that nets away to
    // nothing has `XNPV = 0` identically, and every rate would "pass". Such a
    // flow is `Shape::NoRootExists` and never reaches here, but the predicate
    // should not depend on that.
    value.is_finite() && magnitude > 0.0 && value.abs() <= RESIDUAL_REL_TOL * magnitude
  }

  /// Phase 1. Newton in the exact order a spreadsheet performs it.
  /// `NaN` means a spreadsheet would show `#NUM!`.
  fn solve_like_a_spreadsheet(&self, guess: f64) -> f64 {
    newton_excel_order(guess, &|rate| self.xnpv_with_deriv(rate))
  }

  /// The whole pipeline: Phase 0, then Phase 1, then Phase 2 if the policy
  /// asks for it. `NaN` where no rate is produced; [`xirr_outcome`] names why.
  fn solve(&self, guess: f64, policy: RootPolicy) -> f64 {
    // Phase 1 always runs first: every policy either returns its answer or
    // needs to know that it failed.
    let spreadsheet_rate = self.solve_like_a_spreadsheet(guess);

    match policy {
      RootPolicy::SpreadsheetCompat => spreadsheet_rate,

      RootPolicy::SpreadsheetThenRobust if spreadsheet_rate.is_finite() => spreadsheet_rate,
      RootPolicy::SpreadsheetThenRobust => self.solve_robustly(guess),

      // These two ignore Phase 1's choice but still fall back to it when no
      // root can be enumerated, so they never lose an answer the default
      // would find - provided that answer is a root. These are the two
      // policies sold as correctness over parity, so handing back a rate that
      // fails `is_root` is the one thing they must not do: it is precisely the
      // spreadsheet artefact the caller chose them to avoid.
      RootPolicy::Lowest => self
        .roots()
        .first()
        .copied()
        .unwrap_or_else(|| self.verified_or_nan(spreadsheet_rate)),
      RootPolicy::ClosestToGuess => {
        closest_to(&self.roots(), guess).unwrap_or_else(|| self.verified_or_nan(spreadsheet_rate))
      }
    }
  }

  /// `rate` if it survives [`CashFlow::is_root`], `NaN` otherwise.
  fn verified_or_nan(&self, rate: f64) -> f64 {
    if self.is_root(rate) {
      rate
    } else {
      f64::NAN
    }
  }

  /// Phase 2. Reached only when Phase 1 returned `NaN`, so it can add answers
  /// but never change one a spreadsheet would have given.
  ///
  /// `guess` is a **hint only** here, unlike on the parity path where it is
  /// contractual. The bracketed search runs first and ignores it entirely, so
  /// a caller's bad guess cannot cost an answer that exists.
  fn solve_robustly(&self, guess: f64) -> f64 {
    if self.netted.shape() == Shape::NoRootExists {
      return f64::NAN;
    }
    if let Some(root) = closest_to(&self.roots(), guess) {
      return root;
    }
    // Bracketing missed it: with an even number of sign changes a root can be
    // tangential, or hide between grid nodes where the function only just
    // crosses zero. Newton in log-rate space from a spread of seeds is the
    // last resort - and `guess` gets its turn first.
    std::iter::once(to_log_rate(guess))
      .chain(FALLBACK_SEEDS.into_iter().map(to_log_rate))
      .chain(FALLBACK_LOG_SEEDS)
      .filter(|u| u.is_finite())
      .map(|u| self.newton_from(u))
      .find(|rate| self.is_root(*rate))
      .unwrap_or(f64::NAN)
  }

  /// Newton in log-rate space from `u`, converted back to a rate.
  fn newton_from(&self, u: f64) -> f64 {
    // The residual tolerance is a money quantity and `scaled_g` is money
    // multiplied by an unknown positive constant, so it is not directly
    // comparable. Passing 0.0 makes the iteration run to its step tolerance
    // instead, and `is_root` then judges the answer in rate space where the
    // tolerance means something.
    from_log_rate(newton_to_residual(u, &|u| self.netted.scaled_g(u), 0.0))
  }

  /// Turning points of `G` inside the searched domain: the `u` where `G'`
  /// changes sign, refined by the same bracketing the value uses.
  ///
  /// This is what makes an **even** number of sign changes tractable. With an
  /// odd number the two domain limits differ, so a grid spanning the domain
  /// must contain a bracket and Brent cannot fail. With an even number roots
  /// arrive in pairs, and a pair is invisible to a sign-change scan of `G` in
  /// two ways:
  ///
  /// - both roots fall inside one grid cell, so `G` carries the same sign at
  ///   the two nodes that straddle them;
  /// - the curve touches zero without crossing - a double root - so no
  ///   bracket exists anywhere, at any resolution.
  ///
  /// One observation covers both. Between two roots `G'` must change sign, and
  /// at a double root `G'` is zero. So every root that a scan of `G` can miss
  /// has a turning point at or between the pair, and `G'` *does* change sign
  /// there even where `G` does not. Feeding those points back into the grid
  /// splits the offending cell, after which each root brackets normally.
  ///
  /// Nearly free: [`NettedFlow::scaled_g`] already computes the derivative
  /// alongside the value, so only the refinement is new work.
  fn turning_points(&self, grid: &[f64]) -> Vec<f64> {
    let dg = |u| self.netted.scaled_g(u).1;
    find_crossings(grid, &dg)
      .into_iter()
      .map(|crossing| match crossing {
        Crossing::Bracket(lo, hi) => brentq(&dg, lo, hi, 100),
        Crossing::Exact(u) => u,
      })
      .filter(|u| u.is_finite())
      .collect()
  }

  /// Every root in `(-1, f64::MAX]`, ascending and deduplicated.
  ///
  /// Searches in `u = ln(1 + r)`, so the reachable range is the representable
  /// range rather than a constant someone chose. Two guarantees follow:
  ///
  /// - With an odd number of sign changes, `G` has opposite signs at the two
  ///   ends of the domain, so the grid **must** contain a bracket and Brent
  ///   **must** converge. Failure to return a root then proves the root is
  ///   not representable, not that the search was too coarse.
  /// - With `n` sign changes there are at most `n` roots, so the search stops
  ///   as soon as `n` have been found.
  fn roots(&self) -> Vec<f64> {
    if self.netted.shape() == Shape::NoRootExists {
      return Vec::new();
    }
    let g = |u| self.netted.scaled_g(u).0;
    // `.max(1)`: a flow containing a non-finite amount can net to zero sign
    // changes without that proving anything, and a bound of zero would stop
    // the loop before it started. Such a flow yields no roots anyway - `xnpv`
    // is NaN, so `is_root` rejects everything - but the loop should read
    // correctly rather than rely on that.
    let descartes_bound = self.netted.sign_changes().max(1);

    // Refine the grid at the turning points before bracketing anything: a
    // cell holding a pair of roots has one between them, and splitting there
    // turns a pair the scan cannot see into two ordinary brackets. See
    // [`CashFlow::turning_points`] for why this is the whole even-sign-change
    // case.
    let mut grid = log_rate_grid();
    let extrema = self.turning_points(&grid);
    grid.extend_from_slice(&extrema);
    grid.sort_by(f64::total_cmp);
    grid.dedup();

    let mut in_u: Vec<f64> = Vec::new();
    for crossing in find_crossings(&grid, &g) {
      let u = match crossing {
        Crossing::Bracket(lo, hi) => brentq(&g, lo, hi, 100),
        Crossing::Exact(u) => u,
      };
      if u.is_finite() {
        in_u.push(u);
      }
      if in_u.len() >= descartes_bound {
        break; // Descartes: there cannot be another one.
      }
    }

    // A double root is a turning point that no bracket contains, so it has to
    // be offered as a candidate rather than found. The `is_root` filter below
    // is what decides whether it is real: a turning point that merely comes
    // close to zero fails it, exactly as a bracketed candidate would. No new
    // tolerance is needed and none is introduced.
    //
    // Only turning points that no pair of roots straddles are offered.
    // Between two simple roots there is always a turning point and it is not
    // itself a root, so offering it would report a third root in the middle of
    // a pair - which is what a near-double root looks like from the outside.
    // A genuinely tangential root has no such pair around it, precisely
    // because the curve never crossed.
    //
    // Testing the neighbouring grid nodes instead does not work: the pair can
    // be narrower than one cell, which is the case this whole mechanism exists
    // for, and then both neighbours sit outside the pair and carry the same
    // sign. The roots already found are the only reliable witnesses.
    //
    // Known blind spot: a tangential root lying between two simple roots is
    // skipped. It needs at least four sign changes and a curve that touches
    // zero exactly between two crossings without the touch being one of them.
    // `xirr` still answers such a flow; only the enumeration is short by one.
    in_u.sort_by(f64::total_cmp);
    let straddled_by_a_pair = |u: f64| {
      let below = in_u.partition_point(|root| *root < u);
      below > 0 && below < in_u.len()
    };
    in_u.extend(
      extrema
        .into_iter()
        .filter(|u| !straddled_by_a_pair(*u))
        .collect::<Vec<_>>(),
    );

    // Deduplicate in `u`, where the tolerance is scale-free, before the
    // conversion collapses distinguishable large rates onto each other.
    in_u.sort_by(f64::total_cmp);
    in_u.dedup_by(|a, b| (*a - *b).abs() <= DISTINCT_ROOT_U_TOL);

    let mut roots: Vec<f64> = in_u
      .into_iter()
      .map(from_log_rate)
      .filter(|rate| self.is_root(*rate))
      .collect();
    roots.sort_by(f64::total_cmp);
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

/// `r -> u = ln(1 + r)`. `NaN` outside the domain, which the seed filter drops.
fn to_log_rate(rate: f64) -> f64 {
  rate.ln_1p()
}

/// `u -> r = expm1(u)`, clamped to the searched domain first.
///
/// `expm1` rather than `exp(u) - 1` because near `u = 0` - which is where
/// almost every real IRR lives - the subtraction cancels away the significant
/// digits and `exp(1e-17) - 1` is exactly `0.0`.
fn from_log_rate(u: f64) -> f64 {
  if !u.is_finite() {
    return f64::NAN;
  }
  u.clamp(MIN_SEARCHED_LOG_RATE, MAX_SEARCHED_LOG_RATE)
    .exp_m1()
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
  fn sign_changes_nets_by_date_before_counting() {
    let count = |rows: &[(&str, f64)]| {
      let (d, a) = cash_flow(rows);
      sign_changes(&d, &a, None).unwrap()
    };

    // Zeros and repeats collapse.
    assert_eq!(count(&[("2020-01-01", -1.), ("2021-01-01", 0.)]), 0);
    assert_eq!(
      count(&[("2020-01-01", -1.), ("2020-06-01", 0.), ("2021-01-01", 3.)]),
      1
    );
    assert_eq!(
      count(&[("2020-01-01", -1.), ("2021-01-01", 2.), ("2022-01-01", -3.)]),
      2
    );

    // Same date: the two amounts are one flow, so this is +9000 alone and
    // there is no sign change at all. Counting the raw amounts says 1.
    assert_eq!(count(&[("2023-01-01", -11000.), ("2023-01-01", 20000.)]), 0);

    // Out of input order: sorting by date turns one raw change into two.
    assert_eq!(
      count(&[("2020-01-01", -1.), ("2020-06-01", -5.), ("2020-03-01", 3.)]),
      2
    );
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
