# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
This package is pre-1.0 and follows [Semantic Versioning](https://semver.org/);
breaking changes before 1.0 bump the minor version.

Releases before 0.3.0 predate this file. `0.2.0` is commit `473b9ff`, which is
the reference point for the parity snapshot described below.

## [0.3.0] — unreleased

Breaking. The package is pre-1.0, so this is a minor bump rather than a major
one; there is no compatibility shim and none is planned.

### Summary

The design goal was spreadsheet parity. It now has a second goal that ranks
alongside it: **if a root exists, find it.** Parity is unchanged and is pinned
by a bit-exact regression snapshot; everything new happens on paths where a
spreadsheet had already given up.

### ⚠️ Breaking — JavaScript surface

#### `xirr()` returns an object, not `number | null`

`null` was the answer for four different situations, and a ledger needs to tell
them apart. "This cash flow cannot have an IRR" is a data-quality bug in the
caller's system; "our solver gave up" is an incident; "the spreadsheet says
`#NUM!`" is neither.

```js
// before
const rate = xirr(dates, amounts)
if (rate === null) { /* ...but why? */ }

// after
const result = xirr(dates, amounts)
switch (result.status) {
  case 'root':                result.rate           // a single IRR
  case 'multipleRoots':       result.rate, result.roots
  case 'noRootExists':        // proved: reject the input
  case 'didNotConverge':      // escalate
  case 'spreadsheetNumError': // retry without the parity policy
}
```

**Migration.** The mechanical fix is `.rate`:

```js
- const rate = xirr(dates, amounts)
+ const rate = xirr(dates, amounts).rate
```

or switch the import, which is a drop-in for the old behaviour:

```js
- import { xirr } from 'xirr-rs'
+ import { xirrRate as xirr } from 'xirr-rs'
```

`xirrRate()` is documented as the lossy convenience form. Prefer `xirr()`
anywhere the distinction has to reach a human or a ledger.

#### `signChanges()` now takes dates

```js
- signChanges(amounts)
+ signChanges(dates, amounts, dayCountConvention?)
```

Without dates it could not net payments sharing a date, and could not order
them — and both changed the answer. `[-11000, +20000]` on one date is `+9000`,
which has **no** sign change and therefore no IRR; counting the raw amounts
reported one sign change and implied an IRR existed. It also under-counted:
`[-1, -5, +3]` on dates `d0, d5, d2` is `-1, +3, -5` once ordered, which is two
sign changes rather than one.

The return type is now an unsigned count.

### ⚠️ Breaking — Rust surface

- `sign_changes(amounts)` → `sign_changes(dates, amounts, day_count) -> Result<usize, _>`,
  netted, for the reasons above.
- `MAX_SEARCHED_RATE` removed. Replaced by `MIN_SEARCHED_LOG_RATE` and
  `MAX_SEARCHED_LOG_RATE`, which are derived from `f64` rather than chosen.
- `optimize::find_brackets` replaced by `optimize::find_crossings`, which takes
  an explicit grid and distinguishes an exact zero from a sign change.
- `RESIDUAL_REL_TOL` and `DISTINCT_ROOT_TOL` are now public, so callers can
  reproduce the acceptance test on a rate they were handed.

`xirr()`, `xnpv()` and `xirr_all_roots()` keep their signatures.

### Added

- **`xirr_outcome()` / `XirrOutcome`** — the typed result described above.
- **Provable non-existence.** The cash flow is netted by year fraction, sorted,
  and zeros dropped before anything is solved. Zero sign changes proves no root
  exists in `(-1, ∞)`; the solver returns immediately instead of searching.
- **Log-rate root finding.** Phase 2 now solves `G(u) = Σ aᵢ·exp(-u·δᵢ)` with
  `u = ln(1 + r)`, bracketed by a grid spanning the whole representable domain
  and refined with Brent.
- `MIN_SEARCHED_LOG_RATE` / `MAX_SEARCHED_LOG_RATE` and a test asserting they
  equal `ln(2⁻⁵³)` and `ln(f64::MAX)`.
- Property test: 1,282 generated flows with exactly one netted sign change, all
  solved.
- Parity regression snapshot, 445 rows of raw `f64` bit patterns captured from
  commit `473b9ff`.

### Fixed

- **Roots above `1e12` were unreachable.** A cash flow netting to a small
  day-zero outflow has a real, unique IRR at `1e20` or higher. `xirr()` reached
  some of these through the spreadsheet path while `xirr_all_roots()` returned
  an empty list for the same input; both now agree. Outflows of `20001` and
  `nextafter(20000)` against the reference series returned `null` and now return
  `4.38e50` and `2.13e185`.
- **The residual tolerance was unsatisfiable near total loss.** It was relative
  to the *undiscounted* gross cash flow. At `r = -0.998` over nine years the
  discount factors reach `1e26`, so `XNPV` is a difference of terms around
  `1e30` and the smallest expressible residual is about `1e14` — against a
  tolerance of `1e-9 × 4e5`. Genuine roots found by the search were being
  discarded. The scale is now the larger of the gross and the gross discounted
  cash flow, which leaves the published audit condition exact for `r ≥ 0`.
- **`newton_to_residual`'s step tolerance was absolute** (`1e-12`), so it could
  only ever be met near the origin — one ULP at `r = 1e20` is ~16384. Now
  relative, floored at the old value.
- **Exact zeros manufactured brackets.** `f64::signum` reports `+1.0` for `0.0`,
  so a `signum` comparison reported a sign change wherever the function touched
  zero, and invented one wherever a value underflowed to zero.
- **Underflow could be read as a root.** At large `u` every discount factor
  underflows to `0.0` and a naive `G` evaluates to `0.0`. The objective is now
  scaled by its dominant term, so `G(u) == 0.0` can only mean real cancellation.

### Unchanged — and deliberately so

- `newton_excel_order` and every `EXCEL_*` constant.
- `powf` over `exp2(log2(a)·b)`.
- Year fractions measured from `dates[0]`, not `min(dates)`.
- Dates before the first date remain an error.
- Phase 1's result is still returned without a residual check.
- `RootPolicy::SpreadsheetCompat` output is byte-identical to `473b9ff` across
  all 445 snapshot rows.
- The robust path still never overrides a finite Phase 1 answer.

### Performance

Measured on the reference series, release build, single core. The robust path's
cost is dominated by Phase 1 *failing* first, which is pre-existing and
unchanged.

| Path                                              | ms/call |
| ------------------------------------------------- | ------- |
| Phase 1 alone, failing to `#NUM!`                 | 2.95    |
| Phase 1 + Phase 2 (default policy)                | 3.28    |
| Phase 2 log-rate enumeration alone                | 0.33    |
| Provably rootless input, enumeration              | 0.0005  |
| 1,000 payments, conventional (Phase 1 answers)    | 0.18    |
| 1,000 payments, full enumeration                  | 30.3    |

The last row is the one to know about: `xirr_all_roots` on a long schedule with
two or more sign changes pays for the full grid. `xirr()` does not reach it
unless the policy is `Lowest` or `ClosestToGuess`.
