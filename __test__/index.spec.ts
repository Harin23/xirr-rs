import test from 'ava'
import { xirr } from '../index.js'

const D = (s: string) => new Date(s + 'T00:00:00Z').getTime()

/** Build Float64Arrays from [dateString, amount] pairs. */
function cf(rows: [string, number][]): [Float64Array, Float64Array] {
  return [
    Float64Array.from(rows.map(([d]) => D(d))),
    Float64Array.from(rows.map(([, a]) => a)),
  ]
}

// ---------------------------------------------------------------- baseline

test('simple one-year return', (t) => {
  const [d, a] = cf([['2020-01-01', -100], ['2021-01-01', 130]])
  t.true(Math.abs(xirr(d, a)! - 0.29906844) < 1e-6)
})

// ------------------------------------------- cases where npm `xirr` throws

test('20-year 50x return — npm xirr throws here', (t) => {
  const [d, a] = cf([['1975-01-01', -1000], ['1995-01-01', 50000]])
  // verified algebraically: 50^(1/20.0055) - 1
  t.true(Math.abs(xirr(d, a)! - 0.2158789958) < 1e-9)
})

test('50-year 50x return — npm xirr throws here', (t) => {
  const [d, a] = cf([['1975-01-01', -1000], ['2025-01-01', 50000]])
  t.true(Math.abs(xirr(d, a)! - 0.0813224328) < 1e-9)
})

test('sign reversal after large inflow — npm xirr throws here', (t) => {
  const [d, a] = cf([
    ['2020-01-01', -100],
    ['2020-02-01', 10000],
    ['2020-03-01', -9000],
  ])
  t.true(Math.abs(xirr(d, a)! - -0.7024054932) < 1e-9)
})

test('extreme return from tiny basis — npm xirr throws here', (t) => {
  const [d, a] = cf([
    ['2020-01-01', -1],
    ['2020-01-02', -1],
    ['2030-01-01', 1000000],
  ])
  t.true(Math.abs(xirr(d, a)! - 2.7111358904) < 1e-9)
})

// ------------------------------------------------ correct non-convergence

test('returns null when no rate exists', (t) => {
  // Real lease data: one small outflow, all else inflows. No root exists —
  // Excel returns #NUM! for this too.
  const rows: [string, number][] = [
    ['2026-06-04', -176],
    ['2026-06-04', 25000],
  ]
  let y = 2026, m = 7
  while (y < 2036 || (y === 2036 && m <= 6)) {
    rows.push([`${y}-${String(m).padStart(2, '0')}-01`, 0])
    if (++m === 13) { m = 1; y++ }
  }
  rows.push(['2036-06-03', 433])
  const [d, a] = cf(rows)
  t.is(xirr(d, a), null)
})

// ---------------------------------------------------------------- errors

test('throws on unknown day count convention', (t) => {
  const [d, a] = cf([['2020-01-01', -100], ['2021-01-01', 130]])
  t.throws(() => xirr(d, a, null, 'NOT_A_CONVENTION'))
})

test('throws when all amounts have the same sign', (t) => {
  const [d, a] = cf([['2020-01-01', 100], ['2021-01-01', 130]])
  t.throws(() => xirr(d, a))
})
