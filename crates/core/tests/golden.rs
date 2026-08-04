//! Spreadsheet-parity tests at the core level, sharing the exact fixtures the
//! Node test suite uses. Run with `cargo test -p xirr-core`.
//!
//! Fixtures live at `__test__/golden/` in the repo root so a single corpus
//! backs both test suites; a divergence between the Rust core and the napi
//! layer therefore shows up as one suite passing and the other failing.

use std::{collections::BTreeMap, fs, path::PathBuf, str::FromStr};

use xirr_core::{sign_changes, xirr, xirr_all_roots, xnpv, DateLike, RootPolicy};

const REL_TOL: f64 = 1e-9;

fn golden_dir() -> PathBuf {
  PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    .join("../../__test__/golden")
    .canonicalize()
    .expect("golden fixtures not found; run scripts/build_corpus.py")
}

fn read_csv(name: &str) -> Vec<Vec<String>> {
  let raw = fs::read_to_string(golden_dir().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
  raw
    .lines()
    .skip(1)
    .filter(|l| !l.trim().is_empty())
    .map(|l| {
      l.trim_end_matches('\r')
        .split(',')
        .map(str::to_string)
        .collect()
    })
    .collect()
}

struct Case {
  dates: Vec<DateLike>,
  amounts: Vec<f64>,
}

fn load_cases() -> BTreeMap<String, Case> {
  let mut out: BTreeMap<String, Case> = BTreeMap::new();
  for row in read_csv("cases.csv") {
    let e = out.entry(row[0].clone()).or_insert_with(|| Case {
      dates: Vec::new(),
      amounts: Vec::new(),
    });
    e.dates.push(DateLike::from_str(&row[1]).unwrap());
    e.amounts.push(row[2].parse().unwrap());
  }
  out
}

/// `None` represents a numeric error in the source spreadsheet.
fn load_expected(engine: &str) -> Option<BTreeMap<String, Option<f64>>> {
  let path = golden_dir().join(format!("expected_{engine}.csv"));
  if !path.exists() {
    return None;
  }
  Some(
    read_csv(&format!("expected_{engine}.csv"))
      .into_iter()
      .map(|r| {
        let v = if r[1] == "NUM" {
          None
        } else {
          Some(r[1].parse().unwrap())
        };
        (r[0].clone(), v)
      })
      .collect(),
  )
}

fn close(got: f64, want: f64) -> bool {
  (got - want).abs() <= REL_TOL * want.abs().max(1.0)
}

const ENGINES: [&str; 3] = ["libreoffice", "excel", "sheets"];

#[test]
fn matches_every_spreadsheet_engine_present() {
  let cases = load_cases();
  let mut engines_checked = 0;

  for engine in ENGINES {
    let Some(expected) = load_expected(engine) else {
      continue;
    };
    engines_checked += 1;

    let mut failures = Vec::new();
    let mut rescued = Vec::new();
    let mut compared = 0usize;

    for (id, want) in &expected {
      let Some(c) = cases.get(id) else { continue };
      let got = xirr(&c.dates, &c.amounts, None, None, None).unwrap();

      match want {
        Some(w) => {
          compared += 1;
          if !close(got, *w) {
            failures.push(format!("{id}: expected {w}, got {got}"));
          }
        }
        None => {
          // Strict policy must reproduce the numeric error.
          let strict = xirr(
            &c.dates,
            &c.amounts,
            None,
            None,
            Some(RootPolicy::SpreadsheetCompat),
          )
          .unwrap();
          if strict.is_finite() {
            failures.push(format!(
              "{id}: strict policy should have failed, got {strict}"
            ));
          }
          if got.is_finite() {
            rescued.push(id.clone());
          }
        }
      }
    }

    assert!(
      compared > 50,
      "{engine}: thin corpus, only {compared} cases"
    );
    assert!(
      failures.is_empty(),
      "{engine}: {} divergence(s):\n  {}",
      failures.len(),
      failures.join("\n  ")
    );
    println!(
      "{engine}: {compared} matched, {} rescued: {rescued:?}",
      rescued.len()
    );
  }

  assert!(engines_checked > 0, "no golden files found");
}

#[test]
fn returned_rates_are_actual_roots() {
  // Parity outranks correctness on the default path by design, so this
  // reports rather than fails - but a growing list means the spreadsheet's
  // weak convergence test is biting your data.
  let cases = load_cases();
  let mut suspect = Vec::new();
  for (id, c) in &cases {
    let Ok(rate) = xirr(&c.dates, &c.amounts, None, None, None) else {
      continue;
    };
    if !rate.is_finite() {
      continue;
    }
    let gross: f64 = c.amounts.iter().map(|a| a.abs()).sum::<f64>().max(1.0);
    let residual = xnpv(rate, &c.dates, &c.amounts, None).unwrap().abs();
    if residual > 1e-6 * gross {
      suspect.push(format!("{id}: |XNPV|={residual:e}"));
    }
  }
  println!("weak-convergence rates: {suspect:?}");
}

#[test]
fn single_sign_change_implies_a_unique_root() {
  // Descartes' rule: at most one sign change means at most one root in
  // (-1, inf), so no policy can disagree with any other. Pins the fast path.
  let cases = load_cases();
  for (id, c) in &cases {
    if sign_changes(&c.amounts) > 1 {
      continue;
    }
    let roots = xirr_all_roots(&c.dates, &c.amounts, None).unwrap();
    assert!(
      roots.len() <= 1,
      "{id}: {} roots for a conventional flow",
      roots.len()
    );

    if roots.len() == 1 {
      for policy in [
        RootPolicy::SpreadsheetThenRobust,
        RootPolicy::Lowest,
        RootPolicy::ClosestToGuess,
      ] {
        let r = xirr(&c.dates, &c.amounts, None, None, Some(policy)).unwrap();
        assert!(
          close(r, roots[0]),
          "{id}: {policy:?} gave {r}, root is {}",
          roots[0]
        );
      }
    }
  }
}

#[test]
fn tolerances_are_relative_to_cash_flow_size() {
  let dates: Vec<DateLike> = ["2020-01-01", "2021-01-01", "2022-01-01", "2023-01-01"]
    .iter()
    .map(|s| DateLike::from_str(s).unwrap())
    .collect();
  let base = [-1000.0, 5000.0, -6000.0, 2500.0];

  let mut first = None;
  for exp in [0i32, 3, 6, 9, 12] {
    let k = 10f64.powi(exp);
    let amounts: Vec<f64> = base.iter().map(|a| a * k).collect();
    let r = xirr(&dates, &amounts, None, None, None).unwrap();
    assert!(r.is_finite(), "1e{exp} returned no rate");
    let f = *first.get_or_insert(r);
    assert!(close(r, f), "1e{exp} gave {r}, 1e0 gave {f}");
  }
}
