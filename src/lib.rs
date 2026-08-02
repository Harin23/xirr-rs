#![deny(clippy::all)]

use napi::bindgen_prelude::*;
use napi_derive::napi;
use std::str::FromStr;
use time::OffsetDateTime;
use xirr_core as fin;

//helpers

const MS_PER_DAY: f64 = 86_400_000.0;

/// Epoch milliseconds -> DateLike, truncated to the UTC calendar day.
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

fn invalid(e: fin::InvalidPaymentsError) -> Error {
  Error::new(Status::InvalidArg, e.to_string())
}

/// The core returns NaN when a solve doesn't converge. Surface that as `null`
/// so it can't silently poison arithmetic on the JS side.
fn finite(v: f64) -> Option<f64> {
  v.is_finite().then_some(v)
}

//xirr

#[napi]
pub fn xirr(
  dates: Float64Array,
  amounts: Float64Array,
  guess: Option<f64>,
  day_count_convention: Option<String>,
) -> Result<Option<f64>> {
  let d = to_dates(&dates)?;
  let dc = day_count(day_count_convention)?;
  fin::xirr(&d, &amounts, guess, dc)
    .map(finite)
    .map_err(invalid)
}
