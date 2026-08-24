# The XIRR algorithm, and why it is shaped this way

This document is the reference for anyone — human or AI agent — modifying the
solver. It explains not just what the code does but which parts are load-bearing
for spreadsheet parity and therefore must not be "improved".

---

## 1. The problem

XIRR finds the rate `r` where the net present value of an irregular schedule is
zero:

```
XNPV(r) = Σᵢ amountᵢ × (1 + r)^(-tᵢ)  =  0
```

where `tᵢ` is the year fraction from the first payment to payment `i`.

For a **conventional** cash flow — money out, then money in, exactly one sign
change — Descartes' rule of signs guarantees at most one solution in
`(-1, ∞)`. Any correct solver agrees.

For a **non-conventional** cash flow — capital calls after distributions, a
recall, a clawback — there can be several. `[-1000, 3000, -2500, 600]` on annual
dates has three: **-57.19%**, **-21.92%** and **+79.59%**. All three are
mathematically valid IRRs. Reporting the wrong one is not a rounding error.

---

## 2. Why we imitate the iteration instead of applying a rule

Spreadsheets do not apply a selection rule. They run Newton's method from a
starting guess and return wherever it lands. The answer is **path-dependent**.

Measured over ~400 generated multiple-root cash flows, against a faithful port
of the reference implementation:

| Candidate policy                  | Agreement with the spreadsheet |
| --------------------------------- | ------------------------------ |
| Return the lowest root            | 37.4%                          |
| Return the root nearest the guess | 61.6%                          |
| **Reproduce the iteration**       | **100%**                       |

No predicate over the root set reproduces spreadsheet behaviour. This is the
single most important fact about this library: **parity is achieved by copying
the search order, not by choosing cleverly.**

---

## 3. The reference implementation

Microsoft publishes the contract but not the method: default guess 0.1,
iterate until within 0.000001%, `#NUM!` after 100 tries, ACT/365 fixed, dates
truncated to integers, `#NUM!` if any date precedes the first.

The closest open implementation built for Excel compatibility is
`AnalysisAddIn::getXirr` in `scaddins/source/analysis/financial.cxx`, shared by
Apache OpenOffice and LibreOffice. We port the Apache OpenOffice copy, which is
Apache-2.0 (LibreOffice's is MPL-2.0, file-level copyleft — avoid).

```c
fResultRate = guess (default 0.1);  if (fResultRate <= -1) throw
fMaxEps = 1e-10;  nMaxIter = 50;  scan limit = 200

do {
    if (nIterScan >= 1) fResultRate = -0.99 + (nIterScan - 1) * 0.01;
    do {
        fResultValue = f(fResultRate);
        fNewRate     = fResultRate - fResultValue / f'(fResultRate);
        fRateEps     = |fNewRate - fResultRate|;
        fResultRate  = fNewRate;
        bContLoop    = (fRateEps > fMaxEps) && (|fResultValue| > fMaxEps);
    } while (bContLoop && ++nIter < nMaxIter);
    if (!finite(...)) bContLoop = true;
} while (bContLoop && ++nIterScan < 200);
if (bContLoop) throw;   // -> #NUM!
```

Ported verbatim in `optimize::newton_excel_order`.

> ⚠️ This is a clean-room reimplementation, not Microsoft's code. Microsoft
> documents 100 tries; LibreOffice uses 50 inner iterations × 200 rescans.
> Residual edge cases may differ between real Excel and Calc. Generating
> `expected_excel.csv` from an actual copy of Excel is the outstanding
> validation step — see `scripts/README.md`.

---

## 4. Five details that are load-bearing

Each of these was measured. Changing any one silently breaks parity on some
fraction of inputs, usually only the multiple-root ones, which is exactly the
subset nobody has test coverage for.

### 4.1 The weak convergence test

```rust
cont = rate_eps > EXCEL_EPS && value.abs() > EXCEL_EPS;
```

The `&&` means the loop stops as soon as **either** the step **or** the residual
is small. That is a poor criterion — it can terminate on a flat stretch at a
point that is not a root, which is why spreadsheets sometimes report a nonsense
IRR.

**Do not fix this.** Reproducing it is the job. `xnpv()` is exported so callers
can check the residual themselves and decide.

### 4.2 The rescan grid is fixed-step

`-0.99 + (n - 1) × 0.01` for `n` in `1..200`, i.e. a 0.01 grid over
`[-0.99, +0.99]`. A geometric or wider grid finds more roots — and lands on
different ones.

Consequence: a true IRR below **-99%** is unreachable by Phase 1. A cash flow of
`[-1000, +1]` has an IRR of -99.898% and every spreadsheet reports `#NUM!`.
Phase 2 finds it; `SpreadsheetCompat` does not.

### 4.3 The reference date is `dates[0]`, not `min(dates)`

Both give the same roots. But using `min()` rescales the objective by
`(1 + r)^k`, and since that factor is itself a function of `r`:

```
F(r) = (1+r)^k · N(r)     ⇒     F'/F = k/(1+r) + N'/N
```

the Newton step differs, so the trajectory lands in a different basin.

**Measured: breaks parity on 10.6% of multiple-root inputs.**

The same reasoning is why input order is rejected rather than sorted — see §4.5.

### 4.4 `powf`, not `exp2(log2(a) × b)`

The fast-power shortcut loses a few ULP. Against Phase 1's **absolute** 1e-10
epsilon, those ULP can change which root the iteration converges to.

**Measured: 4 divergences in 2,484 samples.** `fast_pow` is retained in
`utils.rs` for the periodic `irr`/`npv` paths, which have no parity contract.

### 4.5 Dates before `dates[0]` are an error

Spreadsheets raise `#NUM!` rather than reordering. If we sorted, a caller with
unsorted input would get a rate that quietly disagrees with their workbook and
no indication why. Erroring is the honest behaviour.

---

## 5. Architecture

```
xirr(dates, amounts, guess, day_count, policy)
  │
  ├─ CashFlow::new()          validate; year fractions; gross size; netted form
  ├─ checked_guess()          reject NaN and ≤ -1
  │
  ├─ PHASE 0  NettedFlow::shape()
  │             Net by year fraction, drop zeros, count sign changes.
  │             0 changes → no root can exist. Return without searching.
  │
  ├─ PHASE 1  CashFlow::solve_like_a_spreadsheet(guess)
  │             → optimize::newton_excel_order()
  │             Always runs. NaN means "a spreadsheet would show #NUM!".
  │
  └─ dispatch on RootPolicy
       SpreadsheetCompat       → Phase 1 result, verbatim, even if NaN
       SpreadsheetThenRobust   → Phase 1 if finite, else PHASE 2   [default]
       Lowest                  → roots()[0],           else Phase 1
       ClosestToGuess          → closest_to(roots()),  else Phase 1

PHASE 2  CashFlow::solve_robustly(guess)   — entirely in log-rate space
  ├─ roots()  → log_rate_grid() + optimize::find_crossings() + brentq()
  └─ multi-start optimize::newton_to_residual() from guess + fallback seeds
```

**Phase 1's result is returned without checking its residual.** This is the
crux of the parity contract. Re-checking is precisely what would let this
library print 200% where Excel prints 5%: we would reject the spreadsheet's
weakly-converged answer and substitute a "better" root that no spreadsheet
would ever show.

Phase 2 exists only to answer cash flows a spreadsheet cannot. It can _add_ an
answer; it can never _change_ one.

---

## 5a. Phase 0: what the sign pattern proves

`XNPV(r) = 0` is a polynomial in `x = 1/(1 + r) = e^-u`, so Descartes' rule of
signs applies — but **to the netted series, not the input**. Two payments on
the same date share a `δ` and are therefore one coefficient of that polynomial
however they were entered. The series is netted by year fraction, sorted
ascending, and zeros are dropped before anything is counted.

Two limits then settle existence:

```
lim(u → +∞) sign G(u) = sign(first amount)      (smallest δ dominates)
lim(u → -∞) sign G(u) = sign(last amount)       (largest  δ dominates)
```

| Netted sign changes | What is proved                                     |
| ------------------- | -------------------------------------------------- |
| `0`                 | **No root exists.** Every term keeps one sign.      |
| `1`                 | **Exactly one root exists.**                        |
| odd `n`             | The two limits differ ⇒ **at least one root exists**, and a bracket spanning the domain must contain it. |
| even `n > 0`        | `0, 2, … n` roots. Nothing is proved.               |

The odd case is what makes the guarantee possible: a grid that spans the whole
domain **cannot** fail to bracket a root, so bisection cannot fail to find one.
Failure then proves the root is outside the representable range, not that the
search was too coarse.

`validate_positive_negative` still runs first and still rejects flows with no
positive or no negative *raw* amount, because a spreadsheet does. Phase 0
catches the ones that pass it and still cannot have a root — such as
`-11000` and `+20000` on the same date, which nets to `+9000`.

---

## 5b. Phase 2: why log-rate space

Substituting `u = ln(1 + r)`:

```
G(u)  = Σ aᵢ · exp(-u · δᵢ)
G'(u) = Σ -δᵢ · aᵢ · exp(-u · δᵢ)
r     = expm1(u)
```

The whole of `r ∈ (-1, f64::MAX]` maps into `u ∈ [-36.7368, 709.7827]`, and
those two numbers are `ln(2⁻⁵³)` and `ln(f64::MAX)` — representability
boundaries, not tuning constants. A bounded grid at fixed cost therefore
reaches **every rate an `f64` can express**, which is exactly what the previous
`MAX_SEARCHED_RATE = 1e12` ceiling could not do at any setting.

This matters more than it sounds. A cash flow whose netted day-zero position is
a small outflow has a real, unique, well-defined IRR at `r ≈ 1e20` or higher:

| Netted day-0 | `ln(1+r)` | `r`       |
| ------------ | --------- | --------- |
| `-1`         | 116.607   | 4.38e50   |
| `-382`       | 46.926    | 2.40e20   |
| `-1000`      | 36.027    | 4.43e15   |
| `-80000`     | 2.429     | 10.34     |

`1e12` is `u ≈ 27.6`. Everything above the third row was unreachable.

Log space also fixes the tolerance model. One ULP at `r = 1e20` is ~16384, so
no absolute step test can be satisfied there — raising iteration counts never
helped, because the problem was conditioning, not budget.

### The scaling trick

`scaled_g` multiplies both `G` and `G'` by `exp(u·δ_ref)`, where `δ_ref` is
whichever end of the schedule carries the largest exponent at that `u`. Every
exponent becomes `≤ 0`.

This is not an optimisation, it removes two failure modes outright:

1. **No overflow.** Every term is bounded by `|aᵢ|`, at any `u`, so the domain
   never has to be clipped to keep the objective evaluable.
2. **No underflow artifact.** Without scaling, every discount factor underflows
   to exactly `0.0` at large `u` and `G` evaluates to `0.0` — which bisection
   reads as a root. With scaling the `δ_ref` term is exactly `a_ref·exp(0)`, so
   `G(u) == 0.0` can only mean genuine cancellation.

A positive constant factor changes neither the sign of `G` nor the location of
its roots, and it cancels out of the Newton step `G/G'` because both components
carry it. Nothing downstream needs to know.

`optimize::find_crossings` additionally treats an exact zero as a root rather
than a sign change, because `f64::signum` reports `+1.0` for `0.0` and a naive
`signum` comparison manufactures a bracket wherever the function touches zero.

---

## 5c. Typed outcomes

`xirr()` returns `f64` and uses `NaN` for every non-answer. `xirr_outcome()`
returns `XirrOutcome` instead, which names them:

| Variant                | Meaning                                                    |
| ---------------------- | ---------------------------------------------------------- |
| `Root(r)`              | One rate solves the flow.                                   |
| `MultipleRoots{..}`    | Several do. `selected` is the policy's pick, `all` lists them. |
| `NoRootExists`         | **Proved** by Phase 0. An input problem.                    |
| `DidNotConverge`       | A root may exist; the solver did not produce one.           |
| `SpreadsheetNumError`  | `#NUM!` faithfully reproduced under `SpreadsheetCompat`.    |

The last is not redundant. `[-1000, +1]` a year apart makes every spreadsheet
report `#NUM!`, and its IRR is -99.898%. Calling that `NoRootExists` would be
false and calling it `DidNotConverge` would blame a solver that never ran.

`selected` agrees with one element of `all` to within `DISTINCT_ROOT_TOL`, but
is not guaranteed to be the same `f64`: it comes from the spreadsheet's Newton
iteration in rate space and `all` from Brent in log-rate space, and two correct
algorithms land on adjacent floats.

### The `CashFlow` type

`CashFlow` bundles amounts, year fractions and gross size. Before it existed
these were three parallel slices plus a tolerance threaded through every
function, with single-letter closures `f` and `fd` passed around. Methods on a
type that owns its data read like the mathematics.

---

## 6. Tolerances

All declared in `scheduled/xirr.rs`, each with its reason. **They are not
interchangeable.**

| Constant                 | Value      | Measures          | Why                                                                                     |
| ------------------------ | ---------- | ----------------- | --------------------------------------------------------------------------------------- |
| `DEFAULT_GUESS`          | `0.1`      | rate              | Spreadsheet default. Part of the public contract, since the answer is guess-dependent.  |
| `RESIDUAL_REL_TOL`       | `1e-9`     | money, relative   | Scaled by the larger of the gross cash flow and the gross **discounted** cash flow.     |
| `DISTINCT_ROOT_TOL`      | `1e-7`     | rate              | Two `brentq` runs on one root from different brackets must dedupe.                      |
| `DISTINCT_ROOT_U_TOL`    | `1e-7`     | log-rate          | The same idea, scale-free. Does the real work; the rate-space one is a second pass.     |
| `MIN_SEARCHED_LOG_RATE`  | `ln(2⁻⁵³)` | log-rate          | Representability, not policy. Below it, `1 + r` is not a distinct `f64` above `-1`.     |
| `MAX_SEARCHED_LOG_RATE`  | `ln(f64::MAX)` | log-rate      | Representability, not policy. Above it, `expm1` overflows.                              |
| `EXCEL_EPS`              | `1e-10`    | both, absolute    | **Do not touch.** Upstream `fMaxEps`.                                                   |
| `EXCEL_MAX_ITER`         | `50`       | —                 | **Do not touch.** Upstream `nMaxIter`.                                                  |
| `EXCEL_MAX_SCAN`         | `200`      | —                 | **Do not touch.** Upstream rescan limit.                                                |

The `EXCEL_*` constants live in `optimize.rs` beside the code that uses them and
must match upstream exactly.

### Why the residual scale is a maximum of two things

`RESIDUAL_REL_TOL × gross_size` was the previous rule. It is correct for
`r ≥ 0`, where every discount factor is `≤ 1` and the discounted flow can only
be smaller — so the published audit condition stays exact there.

It is **wrong** near total loss. A nine-year flow at `r = -0.998` discounts by
`(1+r)⁻⁹ ≈ 1e26`, so `XNPV` is a difference of terms around `1e30` and the
smallest residual an `f64` can express is about `1e14` — eleven orders of
magnitude above a tolerance of `1e-9 × 4e5`. That test cannot be met by any
solver at any iteration count, and a real root found by the grid was being
discarded because of it. This is the same defect as an absolute epsilon, one
level up: the yardstick was itself scale-dependent.

Taking the larger of the two floors means the tolerance is never tighter than
the floating-point noise of the quantity actually being measured.

### `MAX_SEARCHED_RATE` is gone

It was `1e12`, i.e. `u ≈ 27.6`. It made `xirr()` and `xirr_all_roots()`
disagree — the former returned `2.4e20` for a flow the latter reported as
having no roots at all. The log-rate bounds replace it, and they are derived
from `f64` rather than chosen.

---

## 7. Testing

Three layers, all required:

| Suite      | Location                                                 | Protects                                        |
| ---------- | -------------------------------------------------------- | ----------------------------------------------- |
| Unit       | `#[cfg(test)]` in each module                            | Individual functions                            |
| Edge cases | `crates/core/tests/edge_cases.rs`                        | Validation, numeric extremes, policy invariants |
| Robust     | `crates/core/tests/robust_solver.rs`                     | Existence, reachability, typed outcomes, perf   |
| Golden     | `crates/core/tests/golden.rs`, `__test__/golden.spec.ts` | Spreadsheet parity                              |
| Binding    | `__test__/edge-cases.spec.ts`                            | Date marshalling, null vs throw, typed arrays   |

### The parity snapshot

`crates/core/tests/fixtures/spreadsheet_compat_473b9ff.csv` holds the raw `f64`
bit pattern of every `SpreadsheetCompat` result at commit `473b9ff` — 89 golden
cases x 5 guesses = 445 rows. It was produced by *running that commit*, not by
running the current code, so it cannot drift with what it protects.

Bit patterns rather than decimal, because a decimal round-trip hides exactly
the one-ULP divergences that break parity on multiple-root flows. Any change to
Phase 1 fails this test, which is the intent.

The golden corpus is 89 cases whose expected values come from a **real
spreadsheet engine**, never from this library. See `scripts/README.md` for
regeneration and for adding Excel and Google Sheets alongside LibreOffice.

Both golden suites read the same fixtures, so drift between the Rust core and
the napi layer shows up as one suite passing and the other failing.

---

## 8. If you are changing the solver

1. **Never modify `newton_excel_order` to be more correct.** Its defects are the
   specification.
2. **Run the golden suite and the parity snapshot before and after.**
   `cargo test -p xirr-core`. Any change in a golden value, or any row of
   `spreadsheet_compat_473b9ff.csv` that no longer reproduces bit-for-bit, is a
   parity break rather than an improvement.
3. **New tolerances go in `xirr.rs` with a documented reason.** Do not introduce
   a bare numeric literal into a comparison.
4. **Phase 2 changes are safe; Phase 1 changes are not.** Phase 2 only runs
   where a spreadsheet already failed.
5. **Do the arithmetic in log-rate space.** Anything added to Phase 0 or Phase 2
   that reasons in rate space will reacquire the conditioning bug: absolute
   tolerances are unsatisfiable at large `r`, and discount factors overflow at
   `r` near `-1` over long horizons. `u` is bounded, so neither happens.
6. **A tolerance must be relative to the quantity it measures.** Not to the
   input, not to a constant. This has now been the root cause twice.
5. **If you add cases to the corpus**, regenerate expectations from a
   spreadsheet, not from this code. A golden file derived from the
   implementation tests nothing.
