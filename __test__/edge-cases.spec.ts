/**
 * Edge cases at the JavaScript boundary.
 *
 * The Rust suite (`crates/core/tests/edge_cases.rs`) covers the maths. This
 * file covers what only the binding layer can get wrong: date marshalling
 * across the epoch-millisecond boundary, timezone handling, `null` vs `NaN`
 * semantics, typed-array plumbing, and error surfacing.
 *
 * Grouped by failure mode, because that is how you read a red build.
 */
import test from 'ava'

import { xirr, xnpv, xirrAllRoots, signChanges } from '../index.js'

/** UTC midnight for an ISO date. XIRR truncates to the calendar day. */
const day = (iso: string) => Date.parse(`${iso}T00:00:00Z`)

const flow = (rows: [string, number][]) =>
  [Float64Array.from(rows.map(([d]) => day(d))), Float64Array.from(rows.map(([, a]) => a))] as const

const SIMPLE: [string, number][] = [
  ['2020-01-01', -1000],
  ['2021-01-01', 750],
  ['2022-01-01', 500],
]

/** Three valid IRRs: -57.19%, -21.92%, +79.59%. Calc returns the first. */
const THREE_ROOTS: [string, number][] = [
  ['2015-01-01', -1000],
  ['2016-01-01', 3000],
  ['2017-01-01', -2500],
  ['2018-01-01', 600],
]

// ---------------------------------------------------------------------------
// 1. Date marshalling
// ---------------------------------------------------------------------------

test('dates are truncated to the UTC calendar day', (t) => {
  // Any instant within a day must give the same answer, because a spreadsheet
  // stores dates as integer serials. A time-of-day dependent result would be
  // silently wrong for anyone passing `new Date()`.
  const midnight = flow(SIMPLE)
  const base = xirr(...midnight)!

  for (const offsetHours of [0, 1, 11, 12, 23]) {
    const dates = Float64Array.from(SIMPLE.map(([d]) => day(d) + offsetHours * 3_600_000))
    const amounts = Float64Array.from(SIMPLE.map(([, a]) => a))
    t.is(xirr(dates, amounts), base, `${offsetHours}h offset changed the rate`)
  }
})

test('pre-epoch dates work', (t) => {
  // Negative epoch milliseconds must floor toward the earlier day, not
  // truncate toward zero - the classic sign bug in date conversion.
  const [dates, amounts] = flow([
    ['1955-06-15', -1000],
    ['1965-06-15', 2000],
  ])
  const rate = xirr(dates, amounts)
  t.true(rate !== null && Number.isFinite(rate))
  t.true(Math.abs(rate! - 0.07171245441) < 1e-9, `got ${rate}`)
})

test('a Date object round-trips through getTime()', (t) => {
  const dates = Float64Array.from([new Date(Date.UTC(2020, 0, 1)).getTime(), new Date(Date.UTC(2021, 0, 1)).getTime()])
  const rate = xirr(dates, Float64Array.from([-100, 130]))
  // Not 0.30: 2020 is a leap year, so the period is 366/365 of a year.
  t.true(Math.abs(rate! - 0.29906843900259) < 1e-12, `got ${rate}`)
})

test('non-finite dates throw rather than producing garbage', (t) => {
  const amounts = Float64Array.from([-100, 130])
  for (const bad of [Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY]) {
    t.throws(() => xirr(Float64Array.from([bad, day('2021-01-01')]), amounts), undefined, `${bad}`)
  }
})

// ---------------------------------------------------------------------------
// 2. null vs throw semantics
//
// null means "no rate exists". throw means "your input was invalid".
// Conflating the two is the most likely integration bug for a caller.
// ---------------------------------------------------------------------------

test('returns null when no rate exists', (t) => {
  const [dates, amounts] = flow([
    ['2020-01-01', -0.000001],
    ['2021-01-01', 100000],
    ['2030-01-01', 5],
  ])
  t.is(xirr(dates, amounts), null)
})

test('never returns NaN to JavaScript', (t) => {
  // NaN would poison downstream arithmetic silently; null does not.
  const cases: [string, number][][] = [
    SIMPLE,
    THREE_ROOTS,
    [
      ['2020-01-01', -0.000001],
      ['2021-01-01', 100000],
      ['2030-01-01', 5],
    ],
  ]
  for (const c of cases) {
    const got = xirr(...flow(c))
    t.true(got === null || Number.isFinite(got), `got ${got}`)
  }
})

test('throws on structurally invalid input', (t) => {
  const [dates, amounts] = flow(SIMPLE)

  t.throws(() => xirr(dates, Float64Array.from([-100])), undefined, 'length mismatch')
  t.throws(() => xirr(new Float64Array(0), new Float64Array(0)), undefined, 'empty')
  t.throws(() => xirr(dates, Float64Array.from([100, 200, 300])), undefined, 'all positive')
  t.throws(() => xirr(dates, Float64Array.from([-100, -200, -300])), undefined, 'all negative')
  t.throws(() => xirr(dates, amounts, -1), undefined, 'guess = -1')
  t.throws(() => xirr(dates, amounts, Number.NaN), undefined, 'guess = NaN')
  t.throws(() => xirr(dates, amounts, null, 'act/999'), undefined, 'bad day count')
  t.throws(() => xirr(dates, amounts, null, null, 'nearest' as never), undefined, 'bad policy')
})

test('error messages name the problem', (t) => {
  // A caller debugging a 500 should not have to read our source.
  const [dates] = flow(SIMPLE)
  const err = t.throws(() => xirr(dates, Float64Array.from([100, 200, 300])))
  t.regex(err!.message, /positive|negative/i)
})

test('dates out of order throw instead of being silently sorted', (t) => {
  // Spreadsheets raise #NUM!. Sorting would give an answer that disagrees
  // with the caller's workbook for no visible reason.
  const [dates, amounts] = flow([
    ['2021-01-01', -100],
    ['2020-01-01', 60],
    ['2022-01-01', 60],
  ])
  t.throws(() => xirr(dates, amounts))
})

// ---------------------------------------------------------------------------
// 3. Typed-array plumbing
// ---------------------------------------------------------------------------

test('accepts a large array without stack or precision trouble', (t) => {
  const n = 5000
  const dates = new Float64Array(n)
  const amounts = new Float64Array(n)
  for (let i = 0; i < n; i++) {
    dates[i] = Date.UTC(2000 + Math.floor(i / 12), i % 12, 1)
    amounts[i] = i === 0 ? -50_000 : 15
  }
  const rate = xirr(dates, amounts)
  t.true(rate !== null && Number.isFinite(rate), `got ${rate}`)

  const gross = amounts.reduce((s, a) => s + Math.abs(a), 0)
  t.true(Math.abs(xnpv(rate!, dates, amounts)) < 1e-6 * gross)
})

test('does not mutate its inputs', (t) => {
  const [dates, amounts] = flow(THREE_ROOTS)
  const dCopy = Float64Array.from(dates)
  const aCopy = Float64Array.from(amounts)
  xirr(dates, amounts)
  xirrAllRoots(dates, amounts)
  xnpv(0.1, dates, amounts)
  t.deepEqual(Array.from(dates), Array.from(dCopy))
  t.deepEqual(Array.from(amounts), Array.from(aCopy))
})

test('repeated calls are identical', (t) => {
  const [dates, amounts] = flow(THREE_ROOTS)
  const first = xirr(dates, amounts)
  for (let i = 0; i < 100; i++) t.is(xirr(dates, amounts), first)
})

// ---------------------------------------------------------------------------
// 4. Optional arguments
// ---------------------------------------------------------------------------

test('omitted, undefined and null optional arguments behave identically', (t) => {
  const [dates, amounts] = flow(SIMPLE)
  const a = xirr(dates, amounts)
  const b = xirr(dates, amounts, undefined)
  const c = xirr(dates, amounts, null)
  const d = xirr(dates, amounts, null, null)
  t.is(a, b)
  t.is(a, c)
  t.is(a, d)
})

test('the default guess is 10%, matching every spreadsheet', (t) => {
  const [dates, amounts] = flow(THREE_ROOTS)
  t.is(xirr(dates, amounts), xirr(dates, amounts, 0.1))
})

// ---------------------------------------------------------------------------
// 5. Cross-function consistency
// ---------------------------------------------------------------------------

test('xnpv at the returned rate is approximately zero', (t) => {
  for (const c of [SIMPLE, THREE_ROOTS]) {
    const [dates, amounts] = flow(c)
    const rate = xirr(dates, amounts)!
    const gross = amounts.reduce((s, a) => s + Math.abs(a), 0)
    t.true(Math.abs(xnpv(rate, dates, amounts)) < 1e-6 * gross)
  }
})

test('xirr returns one of the roots xirrAllRoots reports', (t) => {
  const [dates, amounts] = flow(THREE_ROOTS)
  const roots = xirrAllRoots(dates, amounts)
  const rate = xirr(dates, amounts)!
  t.is(roots.length, 3)
  t.true(
    roots.some((r) => Math.abs(r - rate) < 1e-9),
    `${rate} not in ${roots}`,
  )
})

test('signChanges predicts when the root is unique', (t) => {
  const simple = flow(SIMPLE)
  t.is(signChanges(simple[1]), 1)
  t.is(xirrAllRoots(...simple).length, 1)

  const multi = flow(THREE_ROOTS)
  t.is(signChanges(multi[1]), 3)
  t.true(xirrAllRoots(...multi).length > 1)
})

test('xnpv works where xirr refuses', (t) => {
  // xnpv has no positive/negative requirement, so it can price a schedule
  // that has no IRR at all.
  const [dates, amounts] = flow([
    ['2020-01-01', 100],
    ['2021-01-01', 200],
  ])
  t.throws(() => xirr(dates, amounts))
  t.true(Number.isFinite(xnpv(0.1, dates, amounts)))
})

// ---------------------------------------------------------------------------
// 6. Policies
// ---------------------------------------------------------------------------

test('strict spreadsheet policy returns null where a spreadsheet shows #NUM!', (t) => {
  // True IRR is -99.898%, below Calc's -0.99 rescan floor.
  const [dates, amounts] = flow([
    ['2020-01-01', -1000],
    ['2021-01-01', 1],
  ])
  t.is(xirr(dates, amounts, null, null, 'spreadsheet'), null)

  const robust = xirr(dates, amounts)
  t.true(Math.abs(robust! - -0.9989809471) < 1e-9, `got ${robust}`)
})

test('policies agree when the root is unique', (t) => {
  const [dates, amounts] = flow(SIMPLE)
  const rates = (['spreadsheet', 'spreadsheetThenRobust', 'lowest', 'closestToGuess'] as const).map((p) =>
    xirr(dates, amounts, null, null, p),
  )
  for (const r of rates) t.is(r, rates[0])
})

test('policies deliberately differ when roots are ambiguous', (t) => {
  const [dates, amounts] = flow(THREE_ROOTS)
  const sheet = xirr(dates, amounts, null, null, 'spreadsheet')!
  const closest = xirr(dates, amounts, null, null, 'closestToGuess')!
  t.true(Math.abs(sheet - -0.571885951525731) < 1e-9, `sheet ${sheet}`)
  t.true(Math.abs(closest - -0.21924296785) < 1e-8, `closest ${closest}`)
  t.not(sheet, closest)
})

// ---------------------------------------------------------------------------
// 7. Day count conventions
// ---------------------------------------------------------------------------

test('a non-default day count changes the rate but still solves', (t) => {
  const [dates, amounts] = flow([
    ['2020-01-15', -1000],
    ['2021-03-20', 400],
    ['2022-07-04', 800],
  ])
  const act365 = xirr(dates, amounts)!
  const act360 = xirr(dates, amounts, null, 'act/360')!

  t.true(Number.isFinite(act365) && Number.isFinite(act360))
  // ACT/360 discounts over a shorter year, so the rates must differ. Equality
  // would mean the convention was being ignored.
  t.true(Math.abs(act365 - act360) > 1e-6, `${act365} vs ${act360}`)

  // Each is a true root under its own convention.
  const gross = amounts.reduce((s, a) => s + Math.abs(a), 0)
  t.true(Math.abs(xnpv(act360, dates, amounts, 'act/360')) < 1e-6 * gross)
})
