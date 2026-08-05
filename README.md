# xirr-rs

**XIRR for Node.js that returns the same rate your spreadsheet does.**

A native (Rust) implementation of `XIRR` — the internal rate of return for an
irregular schedule of cash flows — built to match Excel, Google Sheets and
LibreOffice Calc, **including which root they pick when a cash flow has more
than one valid IRR**.

```bash
npm install xirr-rs
```

Prebuilt binaries for macOS (x64, arm64), Linux (glibc and musl, x64 and
arm64) and Windows x64. No build step, no Python, no node-gyp.

---

## Quick start

```js
const { xirr } = require('xirr-rs')

const dates = Float64Array.from([Date.UTC(2020, 0, 1), Date.UTC(2021, 0, 1), Date.UTC(2022, 0, 1)])
const amounts = Float64Array.from([-1000, 750, 500])

xirr(dates, amounts) // 0.175009264615451  (17.50%)
```

The same cash flow in a spreadsheet:

```
=XIRR({-1000;750;500}, {DATE(2020,1,1);DATE(2021,1,1);DATE(2022,1,1)})
→ 0.175009264615451
```

---

## Why this package exists

Most JavaScript XIRR implementations run Newton's method once from a guessed
starting rate and throw if it does not converge. That fails on long horizons,
on sign reversals, and — worse — it can **silently return a different root**
than your spreadsheet.

Measured against 89 cash flows whose expected values come from LibreOffice
Calc:

|                                      | agreed with the spreadsheet | threw | **silently wrong root** |
| ------------------------------------ | --------------------------- | ----- | ----------------------- |
| a typical Newton-only implementation | 56 / 71                     | 12    | **3**                   |
| `xirr-rs`                            | **71 / 71**                 | 0     | **0**                   |

One of those three silent divergences reported **-53.3%** where the spreadsheet
reports **+970.5%**. No error, no warning — just a wrong number in a report.

### The multiple-root problem

`XNPV(r) = 0` can have several solutions. A fund with a capital call after a
distribution can have three mathematically valid IRRs. Spreadsheets do not
apply a rule to choose among them — they run Newton's method from a starting
guess and return wherever it lands, so the answer is **path-dependent**.

Over ~400 generated multiple-root cash flows:

| Selection strategy                   | Agreement with the spreadsheet |
| ------------------------------------ | ------------------------------ |
| "return the lowest root"             | 37%                            |
| "return the root nearest the guess"  | 62%                            |
| **reproducing the iteration itself** | **100%**                       |

So that is what this package does. It ports the search order used by
Excel-compatible spreadsheets, rather than trying to be clever.

---

## API

### `xirr(dates, amounts, guess?, dayCountConvention?, policy?)`

Returns `number | null`.

| Argument             | Type           | Default                   | Notes                                                 |
| -------------------- | -------------- | ------------------------- | ----------------------------------------------------- |
| `dates`              | `Float64Array` | —                         | Epoch milliseconds, truncated to the UTC calendar day |
| `amounts`            | `Float64Array` | —                         | Must contain at least one positive and one negative   |
| `guess`              | `number`       | `0.1`                     | Must be `> -1`. Part of the contract — see below      |
| `dayCountConvention` | `string`       | `'act/365f'`              | Spreadsheets only implement `act/365f`                |
| `policy`             | `RootPolicy`   | `'spreadsheetThenRobust'` | See below                                             |

**Returns `null`** when no rate exists. Never returns `NaN`.

**Throws** when: array lengths differ; the amounts are not both positive and
negative; any date precedes `dates[0]`; `guess <= -1` or is not finite; or the
day count / policy string is unrecognised.

> `guess` is not a performance hint. Because the answer is path-dependent when
> multiple roots exist, changing it can change which rate you get — exactly as
> in a spreadsheet.

> Dates must be in chronological order from `dates[0]`. This matches spreadsheet
> behaviour, which raises `#NUM!` rather than reordering. Sorting silently would
> give you a rate that disagrees with your workbook for no visible reason.

### `xnpv(rate, dates, amounts, dayCountConvention?)`

Net present value at a given rate. Unlike `xirr` it does not require both signs,
so you can use it to check the residual of any rate:

```js
const rate = xirr(dates, amounts)
const gross = amounts.reduce((s, a) => s + Math.abs(a), 0)
const isTrueRoot = Math.abs(xnpv(rate, dates, amounts)) < 1e-9 * gross
```

Worth doing: spreadsheets use a weak convergence test and occasionally return a
rate that is not actually a root. This package reproduces that faithfully — see
[Fidelity, not correction](#fidelity-not-correction).

### `xirrAllRoots(dates, amounts, dayCountConvention?)`

Every rate at which XNPV crosses zero, ascending.

```js
xirrAllRoots(dates, amounts)
// [-0.571885951525731, -0.21924296785, 0.79591178]
```

A length greater than one means the IRR is genuinely ambiguous and the single
value from `xirr` is a convention, not a fact. For fund reporting, _"there are
three IRRs and the spreadsheet picked the leftmost"_ is far more useful than one
silent number.

### `signChanges(amounts)`

Sign changes in the cash flow, ignoring zeros. **Zero or one means the IRR is
unique** (Descartes' rule of signs) and every policy must agree. A cheap way to
know whether ambiguity is even possible:

```js
if (signChanges(amounts) <= 1) {
  // unambiguous; no need to think about policy
}
```

---

## Root policies

```js
xirr(dates, amounts, null, null, 'lowest')
```

| Policy                                  | Behaviour                                                                                                                                        |
| --------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| `'spreadsheetThenRobust'` **(default)** | The spreadsheet's rate whenever a spreadsheet has one; bracketed root finding otherwise. Never contradicts a spreadsheet, more likely to answer. |
| `'spreadsheet'`                         | Strict parity, including `null` where a spreadsheet shows `#NUM!`. Use when output must tie out to a workbook.                                   |
| `'lowest'`                              | Smallest root. Deterministic and conservative — for reporting where understating return is the safe direction to fail.                           |
| `'closestToGuess'`                      | Root nearest `guess`. Deterministic, and steerable if you know the expected magnitude.                                                           |

The default is deliberately conservative: it can _add_ an answer where a
spreadsheet gives up, but it can never _change_ one.

```js
const flow = [
  [Date.UTC(2015, 0, 1), -1000],
  [Date.UTC(2016, 0, 1), 3000],
  [Date.UTC(2017, 0, 1), -2500],
  [Date.UTC(2018, 0, 1), 600],
]
const d = Float64Array.from(flow.map(([x]) => x))
const a = Float64Array.from(flow.map(([, y]) => y))

xirr(d, a) // -0.5718859515  <- what Calc returns
xirr(d, a, null, null, 'lowest') // -0.5718859515
xirr(d, a, null, null, 'closestToGuess') // -0.2192429679
xirrAllRoots(d, a) // three roots
```

---

## Fidelity, not correction

Spreadsheets stop iterating as soon as **either** the step **or** the residual
is small. That is a weak test: it can terminate on a flat stretch at a point
that is not actually a root, which is why spreadsheets occasionally report a
strange IRR.

This package reproduces that behaviour rather than fixing it. Returning a
"better" answer than Excel would mean your report and your workbook disagree,
which is the problem this package exists to solve.

If you want correctness over parity you have two options: check the residual
with `xnpv()`, or use `'lowest'` / `'closestToGuess'`, which enumerate roots
properly instead of following the spreadsheet's path.

There is one place where this package deliberately does better: a spreadsheet's
rescan grid stops at **-99%**, so a cash flow with an IRR below that returns
`#NUM!`. The default policy finds it. Use `'spreadsheet'` if you need the
`#NUM!` too.

---

## Day count conventions

Spreadsheet `XIRR()` only implements ACT/365F, so **the parity guarantee applies
only to the default**. Other conventions produce a mathematically sound rate
that no spreadsheet will agree with.

Supported: `act/365f` (default), `act/365a`, `act/364`, `act/360`, `act/act`,
`act/act_isda`, `act/act_afb`, `30/360`, `30e/360`, `30e+/360`, `30u/360`,
`nl/365`.

---

## TypeScript

Types ship with the package and are generated from the Rust source:

```ts
import { xirr, xnpv, xirrAllRoots, signChanges } from 'xirr-rs'

const rate: number | null = xirr(dates, amounts)
```

---

## Accuracy and testing

Expected values come from a **real spreadsheet engine**, never from this
library. The corpus is 89 cash flows (480 payments) computed by LibreOffice
Calc, covering conventional flows, horizons from one day to 50 years, rates from
-99.9% to +100,000%, magnitudes from 1e-2 to 1e12, multiple-root flows with up
to five sign reversals, and 18 flows with no solution at all.

```
71 cases the spreadsheet solved    → 71 exact matches, 0 divergences
18 cases the spreadsheet could not → 17 also unsolvable, 1 rescued
```

`__test__/golden/xirr_golden_corpus.xlsx` is included so you can open it in
Excel or Google Sheets and verify against your own engine.

---

## Attribution and licence

MIT.

The solver core derives from [pyxirr](https://github.com/Anexen/pyxirr)
(Unlicense). The spreadsheet-parity iteration is a port of
`AnalysisAddIn::getXirr` from Apache OpenOffice
(`scaddins/source/analysis/financial.cxx`), Apache License 2.0 — see
[`NOTICE`](./NOTICE). `brentq` is a port of SciPy's `brentq.c` (BSD-3-Clause).

This is a clean-room reimplementation of published, Excel-compatible behaviour.
It is not affiliated with or endorsed by Microsoft, Google or The Document
Foundation.

---

## Contributing

See [`crates/core/docs/ALGORITHM.md`](./crates/core/docs/ALGORITHM.md) for how
the solver works and which parts must not be changed, and
[`AGENTS.md`](./AGENTS.md) for the short version, aimed at AI coding assistants.

```bash
pnpm install
pnpm build          # regenerates index.js and index.d.ts - commit both
cargo test -p xirr-core
pnpm test
```
