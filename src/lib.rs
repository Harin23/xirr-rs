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

/// The core returns NaN where a spreadsheet shows #NUM!. Surface that as
/// `null` so it cannot silently poison arithmetic on the JS side.
fn finite(v: f64) -> Option<f64> {
  v.is_finite().then_some(v)
}

/// Internal rate of return for an irregular schedule.
///
/// Returns the same rate Excel, Google Sheets and LibreOffice Calc return,
/// including which root is chosen when several exist. Returns `null` where
/// those engines return #NUM! and no root can be found.
#[napi]
pub fn xirr(
  dates: Float64Array,
  amounts: Float64Array,
  guess: Option<f64>,
  day_count_convention: Option<String>,
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

/// Number of sign changes in the cash flow. Zero or one guarantees a unique
/// root in (-1, inf) by Descartes' rule, so no policy question arises.
#[napi]
pub fn sign_changes(amounts: Float64Array) -> i32 {
  fin::sign_changes(&amounts)
}
