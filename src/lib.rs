#![deny(clippy::all)]

use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::str::FromStr;
use time::OffsetDateTime;
use xirr_core as fin;

const MS_PER_DAY: f64 = 86_400_000.0;

/// Epoch milliseconds -> DateLike, truncated to the UTC calendar day.
/// Spreadsheets store dates as integer serials and truncate; we match that.
fn to_date(ms: f64) -> Result<fin::DateLike> {
  if !ms.is_finite() {
    return Err(Error::new(
      Status::InvalidArg,
      format!("date is not a finite number: {ms}"),
    ));
  }
  let secs = (ms / MS_PER_DAY).floor() * 86_400.0;
  OffsetDateTime::from_unix_timestamp(secs as i64)
    .map(|dt| fin::DateLike::from(dt.date()))
    .map_err(|e| Error::new(Status::InvalidArg, format!("invalid date value {ms}: {e}")))
}

fn to_dates(ms: &[f64]) -> Result<Vec<fin::DateLike>> {
  ms.iter().map(|&m| to_date(m)).collect()
}

fn day_count(s: Option<String>) -> Result<Option<fin::DayCount>> {
  match s {
    None => Ok(None),
    Some(s) => fin::DayCount::from_str(&s).map(Some).map_err(|e| {
      Error::new(
        Status::InvalidArg,
        format!("unknown day count convention '{s}': {e}"),
      )
    }),
  }
}

/// Accepts the policy as a string so the JS surface stays ergonomic and
/// forward compatible. Unknown values are a hard error rather than a silent
/// fallback - picking a different root than the caller asked for is exactly
/// the failure mode this release exists to remove.
fn root_policy(s: Option<String>) -> Result<Option<fin::RootPolicy>> {
  let Some(s) = s else { return Ok(None) };
  let p = match s.as_str() {
    "spreadsheet" | "spreadsheetCompat" => fin::RootPolicy::SpreadsheetCompat,
    "spreadsheetThenRobust" | "default" => fin::RootPolicy::SpreadsheetThenRobust,
    "lowest" => fin::RootPolicy::Lowest,
    "closestToGuess" => fin::RootPolicy::ClosestToGuess,
    other => {
      return Err(Error::new(
        Status::InvalidArg,
        format!(
          "unknown root policy '{other}'; expected one of: spreadsheet, \
           spreadsheetThenRobust, lowest, closestToGuess"
        ),
      ))
    }
  };
  Ok(Some(p))
}

fn invalid(e: fin::InvalidPaymentsError) -> Error {
  Error::new(Status::InvalidArg, e.to_string())
}

/// The result of a solve, with every non-answer named.
///
/// `status` is one of:
///
/// | `status`              | `rate`   | meaning                                          |
/// | --------------------- | -------- | ------------------------------------------------ |
/// | `root`                | the rate | one rate solves the cash flow                    |
/// | `multipleRoots`       | the pick | several do; `roots` lists them all                |
/// | `unverifiedRate`      | the rate | the spreadsheet's answer is **not** a root       |
/// | `noRootExists`        | `null`   | **proved**: no rate can solve this cash flow      |
/// | `didNotConverge`      | `null`   | a root may exist; the solver did not produce one  |
/// | `spreadsheetNumError` | `null`   | `#NUM!` reproduced under the `spreadsheet` policy |
///
/// The last three were a single `null` before this release, which meant a
/// caller could not tell a data-quality problem in their own ledger from a
/// solver failure from a faithfully reproduced spreadsheet error.
///
/// `unverifiedRate` is the one status that carries a rate you should not post
/// without looking. Spreadsheets stop iterating as soon as either the step or
/// the residual is small, against an absolute epsilon, so they occasionally
/// return a point that `XNPV` never actually reaches zero at. `rate` is that
/// answer, reproduced faithfully so your report still ties out to the
/// workbook; `roots` holds whatever is genuinely a root, and a non-empty
/// `roots` here means the spreadsheet picked a number that is not one of
/// them.
#[napi(object)]
pub struct XirrResult {
  #[napi(
    ts_type = "'root' | 'multipleRoots' | 'unverifiedRate' | 'noRootExists' | 'didNotConverge' | 'spreadsheetNumError'"
  )]
  pub status: String,
  /// The rate, or `null` for every failure status. Never `NaN`.
  // `Either<f64, Null>` rather than `Option<f64>`: napi omits the property
  // entirely for `None`, which reads as `undefined` in JS. A missing key and
  // an explicit "there is no rate" are not the same claim, and `xirrRate`
  // already returns a real `null` - the two surfaces have to agree.
  pub rate: Either<f64, Null>,
  /// Every root found, ascending. Empty for every failure status. A length
  /// above one means the IRR is genuinely ambiguous and `rate` is a
  /// convention, not a fact.
  ///
  /// Under `unverifiedRate` this is the list `rate` failed to join.
  pub roots: Vec<f64>,
}

impl From<fin::XirrOutcome> for XirrResult {
  fn from(outcome: fin::XirrOutcome) -> Self {
    let (status, rate, roots) = match outcome {
      fin::XirrOutcome::Root(r) => ("root", Either::A(r), vec![r]),
      fin::XirrOutcome::MultipleRoots { selected, all } => {
        ("multipleRoots", Either::A(selected), all)
      }
      fin::XirrOutcome::UnverifiedRate { rate, roots } => {
        ("unverifiedRate", Either::A(rate), roots)
      }
      fin::XirrOutcome::NoRootExists => ("noRootExists", Either::B(Null), Vec::new()),
      fin::XirrOutcome::DidNotConverge => ("didNotConverge", Either::B(Null), Vec::new()),
      fin::XirrOutcome::SpreadsheetNumError => ("spreadsheetNumError", Either::B(Null), Vec::new()),
    };
    Self {
      status: status.to_string(),
      rate,
      roots,
    }
  }
}

/// Internal rate of return for an irregular schedule.
///
/// Returns the same rate Excel, Google Sheets and LibreOffice Calc return,
/// including which root is chosen when several exist, and additionally
/// answers cash flows those engines give up on.
///
/// # Breaking change
///
/// This used to return `number | null`. It now returns a [`XirrResult`], so
/// that "no rate exists" and "no rate was found" are distinguishable. For the
/// old shape use `xirrRate`, or read `.rate` from the result.
#[napi]
pub fn xirr(
  dates: Float64Array,
  amounts: Float64Array,
  guess: Option<f64>,
  day_count_convention: Option<String>,
  // napi regenerates index.d.ts on every build, so the TS type has to live
  // here rather than in a hand-edited declaration file - otherwise the next
  // `pnpm build` silently reverts it to `string`.
  #[napi(ts_arg_type = "'spreadsheetThenRobust' | 'spreadsheet' | 'lowest' | 'closestToGuess'")]
  policy: Option<String>,
) -> Result<XirrResult> {
  let d = to_dates(&dates)?;
  let dc = day_count(day_count_convention)?;
  let p = root_policy(policy)?;
  fin::xirr_outcome(&d, &amounts, guess, dc, p)
    .map(XirrResult::from)
    .map_err(invalid)
}

/// The rate alone, or `null` if there is not one.
///
/// The lossy convenience form of `xirr`: it cannot tell you *why* there is no
/// rate. Prefer `xirr` anywhere that distinction has to reach a human or a
/// ledger; use this where a spreadsheet-style blank cell is genuinely all the
/// caller needs.
#[napi]
pub fn xirr_rate(
  dates: Float64Array,
  amounts: Float64Array,
  guess: Option<f64>,
  day_count_convention: Option<String>,
  #[napi(ts_arg_type = "'spreadsheetThenRobust' | 'spreadsheet' | 'lowest' | 'closestToGuess'")]
  policy: Option<String>,
) -> Result<Option<f64>> {
  let d = to_dates(&dates)?;
  let dc = day_count(day_count_convention)?;
  let p = root_policy(policy)?;
  fin::xirr(&d, &amounts, guess, dc, p)
    .map(finite)
    .map_err(invalid)
}

/// Net present value of an irregular schedule at a given rate.
/// Use with `xirr` to check the residual of whatever rate you were handed.
#[napi]
pub fn xnpv(
  rate: f64,
  dates: Float64Array,
  amounts: Float64Array,
  day_count_convention: Option<String>,
) -> Result<f64> {
  let d = to_dates(&dates)?;
  let dc = day_count(day_count_convention)?;
  fin::xnpv(rate, &d, &amounts, dc).map_err(invalid)
}

/// Every rate at which XNPV crosses zero, ascending.
///
/// A length greater than one means the IRR is genuinely ambiguous and the
/// single value returned by `xirr` is a convention, not a fact. Surface this
/// in reporting rather than hiding it.
#[napi]
pub fn xirr_all_roots(
  dates: Float64Array,
  amounts: Float64Array,
  day_count_convention: Option<String>,
) -> Result<Vec<f64>> {
  let d = to_dates(&dates)?;
  let dc = day_count(day_count_convention)?;
  fin::xirr_all_roots(&d, &amounts, dc).map_err(invalid)
}

/// Number of sign changes in the **date-netted** cash flow.
///
/// # Breaking change
///
/// This used to take only the amounts, which meant it could not net payments
/// sharing a date and could not order them. Both defects changed the answer:
/// `[-11000, +20000]` on one date is `+9000` and has **no** sign change, so
/// no IRR exists, where counting the raw amounts reports one and implies an
/// IRR does.
///
/// Zero proves no root exists; one proves exactly one root exists; any odd
/// number proves at least one exists. An even number above zero proves
/// nothing either way.
#[napi]
pub fn sign_changes(
  dates: Float64Array,
  amounts: Float64Array,
  day_count_convention: Option<String>,
) -> Result<u32> {
  let d = to_dates(&dates)?;
  let dc = day_count(day_count_convention)?;
  fin::sign_changes(&d, &amounts, dc)
    .map(|n| n as u32)
    .map_err(invalid)
}

/// The core returns NaN where a spreadsheet shows #NUM!. Surface that as
/// `null` so it cannot silently poison arithmetic on the JS side.
fn finite(v: f64) -> Option<f64> {
  v.is_finite().then_some(v)
}
