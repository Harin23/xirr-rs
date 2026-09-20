/**
 * Parity tests against real spreadsheet engines.
 *
 * Fixtures in ./golden are produced by opening xirr_golden_corpus.xlsx in a
 * spreadsheet and exporting the `results` sheet. expected_libreoffice.csv is
 * checked in; add expected_excel.csv and expected_sheets.csv by running
 * `npm run golden:regen` (see scripts/README.md) and this file picks them up
 * automatically.
 *
 * A row whose expected value is `NUM` means the engine reported a numeric
 * error. We assert that strict `spreadsheet` policy also returns null there,
 * and separately report - without failing - the cases where the default
 * policy legitimately does better.
 */
import { readFileSync, existsSync } from 'node:fs'
import { join } from 'node:path'

import test from 'ava'

// See edge-cases.spec.ts: `xirrRate` is the numeric form of `xirr`.
import { xirrRate as xirr, xirr as xirrResult, xnpv, xirrAllRoots, signChanges } from '../index.js'

// Resolved from cwd, not import.meta: this package is CommonJS
// ("module": "CommonJS", no "type": "module"), so import.meta is unavailable.
// ava always runs from the package root.
const GOLDEN = join(process.cwd(), '__test__', 'golden')

/** Relative tolerance. Engines differ in the last few ULP; 1e-9 is far tighter
 *  than any reporting requirement while still catching a wrong-root pick,
 *  which is always a difference of whole percentage points. */
const REL_TOL = 1e-9

type Case = { id: string; dates: Float64Array; amounts: Float64Array }

function parseCsv(path: string): string[][] {
  return readFileSync(path, 'utf8')
    .split(/\r?\n/)
    .filter((l) => l.length > 0)
    .slice(1)
    .map((l) => l.split(','))
}

function loadCases(): Map<string, Case> {
  const out = new Map<string, Case>()
  const dates = new Map<string, number[]>()
  const amounts = new Map<string, number[]>()
  for (const [id, date, amount] of parseCsv(join(GOLDEN, 'cases.csv'))) {
    if (!dates.has(id)) {
      dates.set(id, [])
      amounts.set(id, [])
    }
    dates.get(id)!.push(Date.parse(`${date}T00:00:00Z`))
    amounts.get(id)!.push(Number(amount))
  }
  for (const id of dates.keys()) {
    out.set(id, {
      id,
      dates: Float64Array.from(dates.get(id)!),
      amounts: Float64Array.from(amounts.get(id)!),
    })
  }
  return out
}

function loadExpected(engine: string): Map<string, number | 'NUM'> | null {
  const path = join(GOLDEN, `expected_${engine}.csv`)
  if (!existsSync(path)) return null
  const m = new Map<string, number | 'NUM'>()
  for (const [id, value] of parseCsv(path)) {
    m.set(id, value === 'NUM' ? 'NUM' : Number(value))
  }
  return m
}

const CASES = loadCases()
/**
 * `rho(r) = |XNPV(r)| / sum |term_i(r)|` on the date-netted flow: how much of
 * the sum actually cancelled. Near 0 the terms genuinely cancelled and the
 * rate is a root; near 1 nothing cancelled and the "sum" is just its one
 * surviving term, which is what an asymptote looks like.
 *
 * The denominator is `xnpv` over the absolute amounts - it validates only
 * length, so an all-positive flow is fine - which keeps this in step with the
 * library's own day-count convention instead of reimplementing it. Netting
 * first is what makes the ratio meaningful; see crates/core/tests/verification.rs.
 */
function rho(rate: number, dates: Float64Array, amounts: Float64Array): number {
  const netted = new Map<number, number>()
  for (let i = 0; i < dates.length; i++) {
    netted.set(dates[i], (netted.get(dates[i]) ?? 0) + amounts[i])
  }
  const kept = [...netted.entries()].filter(([, amount]) => amount !== 0)
  const d = Float64Array.from(kept.map(([date]) => date))
  const a = Float64Array.from(kept.map(([, amount]) => amount))

  const sum = Math.abs(xnpv(rate, d, a))
  const gross = xnpv(rate, d, a.map(Math.abs))
  return sum / gross
}

const ENGINES = ['libreoffice', 'excel', 'sheets']

function closeEnough(got: number, want: number): boolean {
  return Math.abs(got - want) <= REL_TOL * Math.max(Math.abs(want), 1)
}

// ---------------------------------------------------------------------------
// 1. Strict parity: we must agree wherever the engine produced a number.
// ---------------------------------------------------------------------------
for (const engine of ENGINES) {
  const expected = loadExpected(engine)
  if (!expected) continue

  test(`${engine}: every solved case matches exactly`, (t) => {
    const failures: string[] = []
    let compared = 0

    for (const [id, want] of expected) {
      const c = CASES.get(id)
      if (!c || want === 'NUM') continue
      compared++
      const got = xirr(c.dates, c.amounts)
      if (got === null || !closeEnough(got, want)) {
        failures.push(`${id}: expected ${want}, got ${got}`)
      }
    }

    t.true(compared > 50, `expected a substantial corpus, compared ${compared}`)
    t.deepEqual(failures, [], `${failures.length} of ${compared} cases diverged`)
  })

  test(`${engine}: strict policy reproduces numeric errors as null`, (t) => {
    const failures: string[] = []
    for (const [id, want] of expected) {
      const c = CASES.get(id)
      if (!c || want !== 'NUM') continue
      const got = xirr(c.dates, c.amounts, null, null, 'spreadsheet')
      if (got !== null) failures.push(`${id}: expected null, got ${got}`)
    }
    t.deepEqual(failures, [])
  })

  test(`${engine}: default policy never contradicts the engine`, (t) => {
    // The default may return a rate where the engine gave up. It must never
    // return a DIFFERENT rate where the engine gave an answer. That is the
    // "200% vs 5%" guarantee, stated as an invariant.
    const rescued: string[] = []
    for (const [id, want] of expected) {
      const c = CASES.get(id)
      if (!c) continue
      const got = xirr(c.dates, c.amounts)
      if (want === 'NUM') {
        if (got !== null) rescued.push(id)
        continue
      }
      t.true(got !== null && closeEnough(got, want), `${id}: ${got} vs ${want}`)
    }
    t.log(`rescued ${rescued.length} case(s) the engine could not solve: ${rescued.join(', ')}`)
    t.pass()
  })

  test(`${engine}: every returned rate is an actual root`, (t) => {
    // Now an assertion, not a report. A weakly-converged non-root is no longer
    // handed back as `status: 'root'` - it comes back as 'unverifiedRate' -
    // so "the label is honest" is a property that can be enforced rather than
    // logged. Parity is untouched: this asserts nothing about *which* status a
    // case gets, only that a claimed root survives back-calculation.
    const unverified: string[] = []
    for (const [id, want] of expected) {
      const c = CASES.get(id)
      if (!c || want === 'NUM') continue

      const res = xirrResult(c.dates, c.amounts)
      if (res.status === 'unverifiedRate') {
        unverified.push(`${id}: rate = ${res.rate?.toExponential(2)}`)
        continue
      }
      if (res.rate === null) continue

      const r = rho(res.rate, c.dates, c.amounts)
      t.true(r <= 1e-9, `${id}: status '${res.status}' claims ${res.rate} is a root, rho = ${r.toExponential(2)}`)
    }
    t.log(unverified.length ? `unverified rates: ${unverified.join(' | ')}` : 'every rate is a verified root')
  })
}

// ---------------------------------------------------------------------------
// 2. Cross-engine agreement, when more than one golden file is present.
// ---------------------------------------------------------------------------
const LOADED_ENGINES = ENGINES.map((e) => [e, loadExpected(e)] as const).filter(
  (x): x is readonly [string, Map<string, number | 'NUM'>] => x[1] !== null,
)

// Declared skipped rather than passing green. A ✔ on a comparison that never
// ran is exactly how the absence of expected_excel.csv stayed invisible: only
// expected_libreoffice.csv is checked in, so this has never actually compared
// two engines. AVA reports a declaration-time skip in the summary; `t.pass()`
// and `t.fail.skip()` both render as a tick.
const crossEngine = LOADED_ENGINES.length >= 2 ? test : test.skip

crossEngine('engines agree with each other where all of them solved', (t) => {
  const loaded = LOADED_ENGINES
  const [firstName, first] = loaded[0]
  for (const [name, other] of loaded.slice(1)) {
    for (const [id, a] of first) {
      const b = other.get(id)
      if (typeof a !== 'number' || typeof b !== 'number') continue
      t.true(closeEnough(a, b), `${id}: ${firstName}=${a} ${name}=${b}`)
    }
  }
})

// ---------------------------------------------------------------------------
// 3. Regression pins for the specific defects this release fixes.
// ---------------------------------------------------------------------------
const D = (s: string) => Date.parse(`${s}T00:00:00Z`)
const cf = (rows: [string, number][]) =>
  [Float64Array.from(rows.map(([d]) => D(d))), Float64Array.from(rows.map(([, a]) => a))] as const

test('picks the spreadsheet root, not the brentq root', (t) => {
  // Three roots: -0.5719, -0.2192, +0.7959. The previous build's wide brentq
  // returned a different one. This is the "200% vs 5%" class of bug.
  const [d, a] = cf([
    ['2015-01-01', -1000],
    ['2016-01-01', 3000],
    ['2017-01-01', -2500],
    ['2018-01-01', 600],
  ])
  t.true(Math.abs(xirr(d, a)! - -0.571885951525731) < 1e-9)
  t.is(xirrAllRoots(d, a).length, 3)
  t.is(signChanges(d, a), 3)
})

test('result is stable across cash flow magnitude', (t) => {
  // Previously returned null at 1e12 because tolerances were absolute.
  const base: [string, number][] = [
    ['2020-01-01', -1000],
    ['2021-01-01', 5000],
    ['2022-01-01', -6000],
    ['2023-01-01', 2500],
  ]
  const rates = [1, 1e3, 1e6, 1e9, 1e12].map((k) => {
    const [d, a] = cf(base.map(([dt, amt]) => [dt, amt * k]))
    return xirr(d, a)
  })
  for (const r of rates) t.true(r !== null)
  for (const r of rates) t.true(Math.abs(r! - rates[0]!) < 1e-9)
})

test('guess steers the answer and is part of the contract', (t) => {
  const [d, a] = cf([
    ['2020-01-01', -100],
    ['2021-01-01', 230],
    ['2022-01-01', -132],
  ])
  t.true(Math.abs(xirr(d, a)! - 0.10339792770066) < 1e-9)
  t.deepEqual(
    xirrAllRoots(d, a).map((r) => Math.round(r * 1e6) / 1e6),
    [0.103398, 0.192586],
  )
  t.true(Math.abs(xirr(d, a, 0.19, null, 'closestToGuess')! - 0.192586) < 1e-5)
})

test('rejects input a spreadsheet would reject', (t) => {
  const [d, a] = cf([
    ['2021-01-01', -100],
    ['2020-01-01', 130],
  ])
  t.throws(() => xirr(d, a), undefined, 'date precedes the first date')

  const [d2, a2] = cf([
    ['2020-01-01', -100],
    ['2021-01-01', 130],
  ])
  t.throws(() => xirr(d2, a2, -1))
  t.throws(() => xirr(d2, a2, null, null, 'nearest' as never))
  const [d3, a3] = cf([
    ['2020-01-01', 100],
    ['2021-01-01', 130],
  ])
  t.throws(() => xirr(d3, a3))
})
