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
  ├─ CashFlow::new()          validate; compute year fractions and gross size
  ├─ checked_guess()          reject NaN and ≤ -1
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

PHASE 2  CashFlow::solve_robustly(guess)
  ├─ roots()  → optimize::find_brackets() + optimize::brentq()
  └─ multi-start optimize::newton_to_residual() from guess + FALLBACK_SEEDS
```

**Phase 1's result is returned without checking its residual.** This is the
crux of the parity contract. Re-checking is precisely what would let this
library print 200% where Excel prints 5%: we would reject the spreadsheet's
weakly-converged answer and substitute a "better" root that no spreadsheet
would ever show.

Phase 2 exists only to answer cash flows a spreadsheet cannot. It can _add_ an
answer; it can never _change_ one.

### The `CashFlow` type

`CashFlow` bundles amounts, year fractions and gross size. Before it existed
these were three parallel slices plus a tolerance threaded through every
function, with single-letter closures `f` and `fd` passed around. Methods on a
type that owns its data read like the mathematics.

---

## 6. Tolerances

All declared in `scheduled/xirr.rs`, each with its reason. **They are not
interchangeable.**

| Constant            | Value   | Measures        | Why                                                                                    |
| ------------------- | ------- | --------------- | -------------------------------------------------------------------------------------- |
| `DEFAULT_GUESS`     | `0.1`   | rate            | Spreadsheet default. Part of the public contract, since the answer is guess-dependent. |
| `RESIDUAL_REL_TOL`  | `1e-9`  | money, relative | Scaled by gross cash flow. Absolute tolerances made 1e12 unsolvable while 1e9 solved.  |
| `DISTINCT_ROOT_TOL` | `1e-7`  | rate            | Two `brentq` runs on one root from different brackets must dedupe.                     |
| `MAX_SEARCHED_RATE` | `1e6`   | rate            | Phase 2 upper bound.                                                                   |
| `EXCEL_EPS`         | `1e-10` | both, absolute  | **Do not touch.** Upstream `fMaxEps`.                                                  |
| `EXCEL_MAX_ITER`    | `50`    | —               | **Do not touch.** Upstream `nMaxIter`.                                                 |
| `EXCEL_MAX_SCAN`    | `200`   | —               | **Do not touch.** Upstream rescan limit.                                               |

The `EXCEL_*` constants live in `optimize.rs` beside the code that uses them and
must match upstream exactly.

---

## 7. Testing

Three layers, all required:

| Suite      | Location                                                 | Protects                                        |
| ---------- | -------------------------------------------------------- | ----------------------------------------------- |
| Unit       | `#[cfg(test)]` in each module                            | Individual functions                            |
| Edge cases | `crates/core/tests/edge_cases.rs`                        | Validation, numeric extremes, policy invariants |
| Golden     | `crates/core/tests/golden.rs`, `__test__/golden.spec.ts` | Spreadsheet parity                              |
| Binding    | `__test__/edge-cases.spec.ts`                            | Date marshalling, null vs throw, typed arrays   |

The golden corpus is 89 cases whose expected values come from a **real
spreadsheet engine**, never from this library. See `scripts/README.md` for
regeneration and for adding Excel and Google Sheets alongside LibreOffice.

Both golden suites read the same fixtures, so drift between the Rust core and
the napi layer shows up as one suite passing and the other failing.

---

## 8. If you are changing the solver

1. **Never modify `newton_excel_order` to be more correct.** Its defects are the
   specification.
2. **Run the golden suite before and after.** `cargo test -p xirr-core`. Any
   change in a golden value is a parity break, not an improvement.
3. **New tolerances go in `xirr.rs` with a documented reason.** Do not introduce
   a bare numeric literal into a comparison.
4. **Phase 2 changes are safe; Phase 1 changes are not.** Phase 2 only runs
   where a spreadsheet already failed.
5. **If you add cases to the corpus**, regenerate expectations from a
   spreadsheet, not from this code. A golden file derived from the
   implementation tests nothing.
