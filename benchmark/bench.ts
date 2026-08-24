import { Bench } from 'tinybench'

import { xirr, xirrRate, xirrAllRoots } from '../index.js'

const day = (iso: string) => Date.parse(`${iso}T00:00:00Z`)

/** The reference series: netted day zero is `20000 - outflow`. */
function series(outflow: number): [Float64Array, Float64Array] {
  const rows: [string, number][] = [
    ['2023-01-01', -outflow],
    ['2023-01-01', 20_000],
  ]
  for (let m = 2; m <= 12; m++) rows.push([`2023-${String(m).padStart(2, '0')}-01`, 20_000])
  rows.push(['2023-12-31', 0])
  return [Float64Array.from(rows.map((r) => day(r[0]))), Float64Array.from(rows.map((r) => r[1]))]
}

/** Money out, then money in. The overwhelmingly common shape. */
function conventional(payments: number): [Float64Array, Float64Array] {
  const dates: number[] = []
  const amounts: number[] = []
  for (let i = 0; i < payments; i++) {
    dates.push(day(`${2000 + Math.floor(i / 12)}-${String((i % 12) + 1).padStart(2, '0')}-01`))
    amounts.push(i === 0 ? -10_000 : 50)
  }
  return [Float64Array.from(dates), Float64Array.from(amounts)]
}

const conventionalSmall = conventional(24)
const conventionalLarge = conventional(1000)
// Root at ln(1 + r) = 426.7, i.e. r ~ 2.1e185. Only the robust path reaches
// this, so every call pays a full Phase 1 failure plus the log-rate search.
const hardest = series(20_000.000000000004)
// Provably rootless: zero sign changes after netting, so no search happens.
const rootless = series(11_000)
const threeRoots: [Float64Array, Float64Array] = [
  Float64Array.from(['2015-01-01', '2016-01-01', '2017-01-01', '2018-01-01'].map(day)),
  Float64Array.from([-1000, 3000, -2500, 600]),
]

const bench = new Bench({ time: 1000 })

bench
  .add('xirr, conventional, 24 payments', () => {
    xirr(...conventionalSmall)
  })
  .add('xirr, conventional, 1000 payments', () => {
    xirr(...conventionalLarge)
  })
  .add('xirrRate, conventional, 24 payments', () => {
    xirrRate(...conventionalSmall)
  })
  .add('xirr, robust path (root at r = 2.1e185)', () => {
    xirr(...hardest)
  })
  .add('xirr, provably rootless (no search)', () => {
    xirr(...rootless)
  })
  .add('xirrAllRoots, three roots', () => {
    xirrAllRoots(...threeRoots)
  })
  .add('xirrAllRoots, robust path', () => {
    xirrAllRoots(...hardest)
  })

await bench.run()

console.table(bench.table())
