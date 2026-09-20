# AGENTS.md

Working notes for AI coding assistants (and humans in a hurry) modifying this
repository. Read this before changing anything under `crates/core/`.

---

## What this package is

`xirr-rs` computes XIRR and is built to return **the same rate Excel, Google
Sheets and LibreOffice Calc return**, including which root they pick when a cash
flow has several valid IRRs.

That parity is the product. It outranks mathematical elegance, performance, and
being "more correct" than a spreadsheet.

---

## The rule that catches most mistakes

> **`XNPV(r) = 0` can have several solutions, and spreadsheets do not choose
> among them by rule — they choose by iteration path.**

Any change that alters the search order changes which root is returned for
non-conventional cash flows. That is a difference of whole percentage points,
not rounding, and it will not show up in tests that only use conventional
cash flows.

Concretely: no heuristic reproduces spreadsheet behaviour. "Return the lowest
root" matches 37% of the time. "Return the root nearest the guess" matches 62%.
Copying the iteration matches 100%.

---

## Do not change these

| Location                                                                               | Why                                                                               |
| -------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------- |
| `optimize::newton_excel_order`                                                         | Verbatim port of `AnalysisAddIn::getXirr`. Its **defects are the specification**. |
| `EXCEL_EPS`, `EXCEL_MAX_ITER`, `EXCEL_MAX_SCAN`, `EXCEL_SCAN_START`, `EXCEL_SCAN_STEP` | Upstream constants. Must match exactly.                                           |
| The `&&` in `cont = rate_eps > EXCEL_EPS && value.abs() > EXCEL_EPS`                   | Weak convergence test. It is _supposed_ to sometimes stop at a non-root.          |
| `CashFlow::xnpv` using `powf`                                                          | `exp2(log2(a)*b)` loses ULP and changes which root is found.                      |
| `year_fractions` measuring from `dates[0]`                                             | Using `min()` breaks parity on ~10% of multiple-root inputs.                      |
| The fact that Phase 1's result is **returned** without a residual check                | Re-checking is what would let this print 200% where Excel prints 5%.              |
| `is_root` measuring the **netted** flow                                                | Un-netted, a cancelling day-zero pair hides an asymptote. See below.              |

If a change makes the code "better" by fixing one of the above, it is a
regression. Say so and stop.

### Returned vs. labelled — read this before touching `is_root`

The fourth row is narrower than it looks, and the distinction is the whole
design:

- **Returned.** `xirr()` hands back Phase 1's rate under both spreadsheet
  policies whether or not it is a root. Do not gate this. This is the row
  above.
- **Labelled.** `xirr_outcome()` runs `is_root` on that rate and reports
  `UnverifiedRate` instead of `Root` when it fails. Parity is preserved by
  labelling, never by filtering.

So "never claim a root we have not verified" and "always return what the
spreadsheet returned" are both true at once, and neither may be sacrificed for
the other. `RootPolicy::Lowest` and `ClosestToGuess` are the one exception —
they are documented as correctness over parity, so they return `NaN` rather
than fall back to an unverified rate.

---

## Safe to change

- Anything in `CashFlow::solve_robustly`, `CashFlow::roots` and
  `CashFlow::turning_points` (Phase 2). On the parity path these only run where
  a spreadsheet already returned `#NUM!`, so they can add an answer but never
  change one. Note they are no longer *only* reachable there: `xirr_outcome`
  also calls `roots()` to fill `MultipleRoots::all` and `UnverifiedRate::roots`,
  and the two correctness policies call it directly. Changing what `roots()`
  returns therefore changes reported output even when the returned rate does
  not move.
- `log_rate_grid` band widths and step sizes, `FALLBACK_SEEDS`,
  `FALLBACK_LOG_SEEDS`. The log-rate **bounds** are not tuning knobs: they are
  `ln(2^-53)` and `ln(f64::MAX)`, i.e. the range an `f64` rate can express, and
  a test asserts exactly that.
- `NettedFlow` and the Phase 0 existence test, provided zero sign changes keeps
  meaning "no root exists".
- Documentation, tests, error messages, the napi binding layer.
- `periodic.rs` and `private_equity.rs` — no parity contract.

---

## Where things live

```
crates/core/src/scheduled/xirr.rs   the algorithm + all XIRR tolerances
crates/core/src/optimize.rs         root finders (Newton, Brent, bracketing)
crates/core/tests/robust_solver.rs  existence, reachability, typed outcomes,
                                    and the even-sign-change property tests
crates/core/tests/verification.rs   the mathematical oracle: every rate we
                                    call a root must survive back-calculation
crates/core/tests/determinism.rs    cross-platform reference rates
crates/core/tests/fixtures/         parity snapshot captured from 473b9ff
crates/core/src/utils.rs            shared helpers for the non-XIRR paths
src/lib.rs                          napi binding: JS types, null vs throw
index.js, index.d.ts                GENERATED by `pnpm build`. Commit both.
crates/core/docs/ALGORITHM.md       full rationale with measurements
__test__/golden/                    expected values from a real spreadsheet
```

---

## Commits

PR titles are conventional commits — `feat:`, `fix:`, `perf:`, `docs:`,
`chore:`. CI checks the title on every PR.

The title becomes the squash commit subject, and that subject is the only thing
the release tooling ever reads. `feat` / `fix` / `perf` become CHANGELOG lines
and drive the version bump; everything else is hidden. A breaking change gets
`!` after the type (`feat!:`) or a `BREAKING CHANGE:` footer.

Versions are never edited by hand. See [`RELEASING.md`](./RELEASING.md).

---

## Before you claim a change works

```bash
cargo test -p xirr-core             # unit + edge cases + golden + verification
cargo test -p xirr-core --release   # the parity snapshot is release-sensitive
cargo fmt -- --check
cargo clippy --workspace --all-targets
pnpm build                          # regenerates index.js / index.d.ts
pnpm lint
pnpm test                           # golden + edge cases through the binding
```

`--workspace --all-targets`, not bare `cargo clippy`. The root manifest has its
own `[package]`, so a bare run lints only the napi crate and treats `xirr-core`
as an ordinary dependency — **the entire test suite goes unlinted**. A
deny-by-default lint sat in `tests/robust_solver.rs` through a full green CI run
because of this.

**Any change to a golden value is a parity break, not an improvement.** The
fixtures in `__test__/golden/expected_libreoffice.csv` came from LibreOffice
Calc, not from this code. If your change alters one, you have broken the thing
the package exists to do.

---

## Common traps

**Leap years.** ACT/365F counts actual days and divides by 365, so a one-year
holding period spanning a leap year is `366/365` of a year and the annualised
rate is _not_ the round number you expect. `-1000 → +1200` across 2020 gives
**19.94%**, not 20%. Two of the edge-case tests exist specifically to pin this.

**`index.js` is generated and committed.** Adding a `#[napi]` function to
`src/lib.rs` is not enough — run `pnpm build` and commit the regenerated
`index.js` and `index.d.ts`, or the new export will not exist at runtime. CI
has a drift check for this.

**TypeScript types come from Rust.** `index.d.ts` is overwritten on every
build. To change a TS type, use `#[napi(ts_arg_type = "...")]` in `src/lib.rs`.
Hand-editing `index.d.ts` will be silently reverted.

**The package is CommonJS.** No `"type": "module"`, `tsconfig` says
`"module": "CommonJS"`. Do not use `import.meta` in tests; resolve paths from
`process.cwd()`.

**Tolerances are not interchangeable.** `RESIDUAL_REL_TOL` bounds a
**dimensionless ratio** — `|XNPV|` over the sum of the term magnitudes at that
rate, on the netted flow. It is not relative to gross cash flow; it used to be,
and that was a defect in both directions. `DISTINCT_ROOT_TOL` measures rates.
`EXCEL_EPS` is absolute and belongs to upstream. Do not collapse them, and do
not put a bare numeric literal in a comparison — add a named constant in
`xirr.rs` with a documented reason.

**The residual test is a ratio, not a size.** A root is a rate at which the
terms of `XNPV` *cancel*, so the test is how much cancelled:

```
rho(r) = |SUM a_i (1+r)^-d_i| / SUM |a_i (1+r)^-d_i|      on the netted flow
```

`rho` near 0 is a root; near 1 nothing cancelled and the "sum" is just its one
surviving term — an **asymptote**. No absolute or gross-relative threshold
separates those two, and two earlier rules failed in opposite directions:
`REL_TOL * gross` rejected real roots near total loss, and
`REL_TOL * max(gross, discounted_gross)` accepted `r = 2.4e13` on a flow with
no root at all. Netting is load-bearing, not an optimisation: un-netted, that
same rate scores `rho = 2e-13` and passes. `tests/verification.rs` pins this.

**Parity only covers ACT/365F.** That is the only convention spreadsheet
`XIRR()` implements. Any other `day_count` gives a valid rate that no
spreadsheet agrees with; do not add golden expectations for those.

---

## If you are adding test cases

Expected values must come from a spreadsheet, never from this library. A golden
file derived from the implementation tests nothing. See `scripts/README.md` for
regenerating the corpus, including how to add Excel and Google Sheets alongside
LibreOffice.

**There is a second, independent kind of test, and it is not a golden file.**
`tests/verification.rs` asserts against the *mathematics* — back-calculate
`XNPV` at the answer and check it cancels — with no expected values and no
engine. This is not a violation of the rule above; it exists because a corpus
comparison structurally cannot catch a whole class of defect. The weak
convergence test is common to every spreadsheet engine, so when it stops at a
non-root **they all agree on the same wrong answer**, and a parity test sees
nothing. Measured: on flows containing a same-day pair that cancels, 57% of
spreadsheet answers fail verification and 26% of those have a genuine root the
spreadsheet missed entirely.

So: parity tests say "we match the engines", verification tests say "the engines
and we are both right". Do not collapse them, and do not delete the second for
lacking a spreadsheet source.

Only `expected_libreoffice.csv` is checked in. `golden.rs` and `golden.spec.ts`
name the engines they skipped rather than skipping silently — if you make the
skip quiet again, Excel parity goes back to being untested while the suite
still reads green.
