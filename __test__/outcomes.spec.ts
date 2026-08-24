//! The typed result surface: `xirr()` returning a `XirrResult` rather than
//! `number | null`.
//!
//! The point of these tests is the *distinction*. Before this release every
//! one of the failures below was the same `null`, so a caller could not tell a
//! data-quality bug in their own ledger from a solver failure from a
//! faithfully reproduced spreadsheet error.

import test from 'ava'

import { xirr, xirrRate, xnpv, signChanges } from '../index.js'

const day = (iso: string) => Date.parse(`${iso}T00:00:00Z`)

function flow(rows: [string, number][]): [Float64Array, Float64Array] {
  return [Float64Array.from(rows.map((r) => day(r[0]))), Float64Array.from(rows.map((r) => r[1]))]
}

/// The reference series: an outflow and a +20000 inflow on day zero, then
/// +20000 on the first of each month, February to December.
function series(outflow: number): [Float64Array, Float64Array] {
  const rows: [string, number][] = [
    ['2023-01-01', -outflow],
    ['2023-01-01', 20_000],
  ]
  for (let m = 2; m <= 12; m++) rows.push([`2023-${String(m).padStart(2, '0')}-01`, 20_000])
  rows.push(['2023-12-31', 0])
  return flow(rows)
}

// ---------------------------------------------------------------------------
// 1. A rate, and the shape it arrives in
// ---------------------------------------------------------------------------

test('a conventional flow reports a single root', (t) => {
  const [dates, amounts] = flow([
    ['2020-01-01', -1000],
    ['2021-01-01', 750],
    ['2022-01-01', 500],
  ])
  const result = xirr(dates, amounts)

  t.is(result.status, 'root')
  t.true(Math.abs(result.rate! - 0.175009264615451) < 1e-9)
  t.deepEqual(result.roots, [result.rate!])
})

test('xirrRate is the same number as xirr().rate', (t) => {
  for (const outflow of [11_000, 20_000, 20_001, 25_000, 100_000]) {
    const [dates, amounts] = series(outflow)
    t.is(xirr(dates, amounts).rate, xirrRate(dates, amounts), `outflow ${outflow}`)
  }
})

// ---------------------------------------------------------------------------
// 2. The failures are distinguishable
// ---------------------------------------------------------------------------

test('a flow that cannot have an IRR says so, rather than returning null', (t) => {
  // Netted day zero is -11000 + 20000 = +9000 and every later flow is
  // positive, so XNPV >= 9000 everywhere. The raw amounts contain both signs,
  // which is why this passes validation and has to be caught by the netted
  // existence test.
  const [dates, amounts] = series(11_000)

  t.is(signChanges(dates, amounts), 0)
  const result = xirr(dates, amounts)
  t.is(result.status, 'noRootExists')
  t.is(result.rate, null)
  t.deepEqual(result.roots, [])

  // And it really is bounded away from zero.
  t.true(xnpv(0, dates, amounts) >= 9_000)
  t.true(xnpv(1e6, dates, amounts) >= 9_000)
})

test('a netted day zero of exactly zero has no IRR and no underflow artifact', (t) => {
  const [dates, amounts] = series(20_000)
  t.is(signChanges(dates, amounts), 0)
  t.is(xirr(dates, amounts).status, 'noRootExists')
  t.deepEqual(xirr(dates, amounts).roots, [])
})

test('a reproduced #NUM! is not reported as a solver failure', (t) => {
  // Every spreadsheet reports #NUM! here. The IRR is -99.898%, so this is not
  // "no root exists" — and nothing failed, so it is not "did not converge".
  const [dates, amounts] = flow([
    ['2020-01-01', -1000],
    ['2021-01-01', 1],
  ])

  const strict = xirr(dates, amounts, null, null, 'spreadsheet')
  t.is(strict.status, 'spreadsheetNumError')
  t.is(strict.rate, null)

  const robust = xirr(dates, amounts)
  t.is(robust.status, 'root')
  t.true(Math.abs(robust.rate! - -0.9989809471) < 1e-9)
})

// ---------------------------------------------------------------------------
// 3. Ambiguity is surfaced, not hidden
// ---------------------------------------------------------------------------

test('a three-root flow reports every root alongside the pick', (t) => {
  const [dates, amounts] = flow([
    ['2015-01-01', -1000],
    ['2016-01-01', 3000],
    ['2017-01-01', -2500],
    ['2018-01-01', 600],
  ])

  const result = xirr(dates, amounts)
  t.is(result.status, 'multipleRoots')
  t.is(result.roots.length, 3)
  t.is(signChanges(dates, amounts), 3)

  // The pick is the spreadsheet's, and it matches one of the enumerated roots
  // as a rate — not necessarily as the same f64, since the two come from
  // different algorithms by design.
  t.true(Math.abs(result.rate! - -0.571885951525731) < 1e-9)
  t.true(result.roots.some((r) => Math.abs(r - result.rate!) <= 1e-7))

  const ascending = [...result.roots].sort((a, b) => a - b)
  t.deepEqual(result.roots, ascending)
})

// ---------------------------------------------------------------------------
// 4. Rates the previous release could not reach
// ---------------------------------------------------------------------------

test('roots far above the old 1e12 ceiling are returned', (t) => {
  // Both of these returned null before the log-rate rewrite: 1e12 is
  // u = ln(1 + r) ~ 27.6, and these roots live at u = 116.6 and u = 426.7.
  const cases: [number, number][] = [
    [20_001, 4.383552505094086e50],
    [20_000.000000000004, 2.1271889750893688e185],
    [20_382, 2.3982461250299013e20],
    [25_000, 232604535.36894694],
  ]

  for (const [outflow, expected] of cases) {
    const [dates, amounts] = series(outflow)
    t.is(signChanges(dates, amounts), 1, `outflow ${outflow}`)

    const result = xirr(dates, amounts)
    t.is(result.status, 'root', `outflow ${outflow}`)
    t.true(
      Math.abs((result.rate! - expected) / expected) < 1e-9,
      `outflow ${outflow}: got ${result.rate}, want ${expected}`,
    )
  }
})

// ---------------------------------------------------------------------------
// 5. signChanges needs dates, and that is the point
// ---------------------------------------------------------------------------

test('signChanges nets payments that share a date', (t) => {
  // One flow of +9000, so no sign change at all. Counting the raw amounts
  // reports one and implies an IRR exists.
  const [dates, amounts] = flow([
    ['2023-01-01', -11_000],
    ['2023-01-01', 20_000],
  ])
  t.is(signChanges(dates, amounts), 0)
})

test('signChanges orders by date before counting', (t) => {
  // -1, +3, -5 once ordered: two sign changes, where input order shows one.
  const [dates, amounts] = flow([
    ['2020-01-01', -1],
    ['2020-06-01', -5],
    ['2020-03-01', 3],
  ])
  t.is(signChanges(dates, amounts), 2)
})
