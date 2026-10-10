# Int overflow

Coil `int` arithmetic is exact or it panics, in every build
([coil-lang#852](https://github.com/ardax-corp/coil-lang/issues/852)):

| Operation | Panics with |
|-----------|-------------|
| `a + b`, `a - b`, `a * b`, `-a`, `a += b`, `a++` … whose result is outside `int` | `integer overflow` |
| `a / b` with `a == MIN`, `b == -1` | `integer overflow` |
| `a / 0`, `a % 0` | `division by zero` |
| `a ** b` that does not fit | `integer overflow` |
| `a ** b` with `b < 0` | `negative exponent` |

`MIN % -1` is `0`. Shifts and bitwise operators do not trap. Floats keep
IEEE behaviour (`inf`, `NaN`). Wrapping, checked and saturating arithmetic
are explicit operations in coil-stdlib
([coil-stdlib#27](https://github.com/ardax-corp/coil-stdlib/issues/27)).

Before this, a release VM wrapped silently and a debug VM aborted the
process, and `x / 0` aborted in both.

## One definition

`common::int_arith` (`add`, `sub`, `mul`, `neg`, `div`, `rem`, `pow`)
returns `Result<i64, IntTrap>`. Everything below calls it, so the VM and the
compiler agree on every edge.

## VM

- Stack ops (`ADD` … `Pow`, `INC` / `DEC`, `Dyn*`): `exec_rest.rs`
  (`int_bin!`, `int_trap!` → `runtime_panic`).
- Fused ops (`BinSlot*`, `BinReturn`): `fused::eval_bin` returns a
  `Result`; the giant match panics, a dense streak sets `panic_msg`.
- Dense MIR ops: `dense::eval_bin` / `eval_unary`, including the peeked
  trailing `DenseBin` in a streak.
- `V*` SIMD lanes: `simd.rs`. Int lanes are checked per lane, and a
  reduce is a checked left fold in element order, so it panics on the
  first partial sum the scalar loop would.
- Packed vector / matrix kernels: `packed_la.rs` return a `Result`, and the
  host closure turns a trap into `FfiError::IntTrap`, which `HostInvoke`
  raises as a plain panic. Dot products and matmul cells sum in index order.

## Compiler

- Constant folds (HIR `fold`, MIR `instcombine`, `const_fold`,
  `const_eval`) fold only an exact result; an op that would trap stays for
  run time.
- Rewrites that would move, add or drop a trap are off for ints:
  - `x * 2^n` stays a `MUL` (a shift does not trap); a `byte` still shifts.
  - MIR IV strength reduction is float-only.
  - `-(-x)` → `x` is float-only.
  - MIR LICM hoists an int `+ - *` only from the loop header, ahead of
    anything observable; MIR PRE never moves one.
  - HIR LICM counts int arithmetic as trapping, and eager `&&` / `||` does not
    evaluate int arithmetic that the short circuit would skip.
- Known gap: the HIR inliner still treats int `+ - *` as pure when it hoists
  a call ahead of an operand
  ([coil-lang#856](https://github.com/ardax-corp/coil-lang/issues/856)).
- Auto-par loops: see [Auto-par](auto-par.md#loop-ipa-chunked-fork-join-over-an-induction-range).
- `coil verify` assumes every int `+ - *` and negation that a path goes past
  did not overflow.
