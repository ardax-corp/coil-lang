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
| `a << n`, `a >> n` with `n` outside `0..=63` | `shift count out of range` |

`MIN % -1` is `0`. A shift keeps no overflow check on its value (bits shifted
out are dropped), only on its count; a count known at compile time outside
`0..=63` is error E0131 (coil-lang#1076). Bitwise operators do not trap. Floats keep
IEEE behaviour (`inf`, `NaN`). Checked, wrapping, saturating and
overflowing arithmetic are userland traits in coil-stdlib's `arith` module
(`Checked`, `Wrapping`, `Saturating`, `Overflowing`;
[coil-stdlib#29](https://github.com/ardax-corp/coil-stdlib/pull/29)). There
is no overflow mode flag (`coil verify --overflow` was removed).

`byte` arithmetic wraps modulo 256 instead
([coil-lang#882](https://github.com/ardax-corp/coil-lang/issues/882)): a
`byte` is the same immediate word as an `int`, so HIR build masks the int
result of `+ - * <<` and unary `-` with `& 255` (`build::wrap_byte`), and
lowers `**` to a square-and-multiply over the exponent's 8 bits that masks
every step (`build::byte_pow`), so it never reaches an int overflow. The
checker warns E0130 (`ByteWrap`) on each of these operators and their
compound forms. An element-wise operator on `byte` elements (`[byte; N]`,
a tuple of bytes, `Vec<byte>`) wraps each element the same way and gives
the same warning ([coil-lang#2401](https://github.com/ardax-corp/coil-lang/issues/2401)):
the MIR lowering masks every element (`Lower::aggregate`, with
`Lower::byte_pow` for `**`), and the packed kernel masks its results when
the meta word's byte bit is set (`packed_vec_arith`, bit 28). Linear
algebra on byte cells does too
([coil-lang#2403](https://github.com/ardax-corp/coil-lang/issues/2403)):
`Matrix` `+`, `-`, negation and `*`, `matmul`, `dot` and `cross` wrap each
result (a `dot` or a product cell wraps its sum once, which is the same
value as wrapping every step), in the kernels of `packed_la.rs` and in the
unrolled lowering. Later work
may prove a range was checked first and turn an unchecked wrap into an
error.

## One definition

`common::int_arith` (`add`, `sub`, `mul`, `neg`, `div`, `rem`, `pow`)
returns `Result<i64, IntTrap>`. It defines every edge: the compiler's folds
and the runtime's kernels call it, and the engines reach the same traps and
the same `IntTrap` messages.

## Engines

- Native code (`coil-native`): overflow checks in line, branching to a trap
  (`TRAP_OVERFLOW`, `TRAP_DIV_ZERO`, `TRAP_SHIFT`); `**` calls `coil_native_ipow`.
  `native::NativePanic` turns a trap code into the `IntTrap` message.
- MIR interpreter (`coil-mir-interp/src/exec.rs`): checked Rust arithmetic,
  with the same trap codes.
- Vector lanes: `lanes.rs` and `simd.rs`. Int lanes are checked per lane, and a
  reduce is a checked left fold in element order, so it panics on the
  first partial sum the scalar loop would.
- Packed vector / matrix kernels: `packed_la.rs` return a `Result`, and the
  host closure turns a trap into `FfiError::IntTrap`, which the host call
  raises as a plain panic. Dot products and matmul cells sum in index order.

## Compiler

- Constant folds (HIR `fold`, MIR `instcombine`, `const_fold`,
  `const_eval`) fold only an exact result; an op that would trap stays for
  run time.
- Rewrites that would move, add or drop a trap are off for ints:
  - `x * 2^n` stays a multiply (a shift does not trap); a `byte` still shifts.
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
