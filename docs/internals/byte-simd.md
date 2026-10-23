# Byte-lane SIMD

Plain Coil byte loops (`while i < n { if s[i] == c { ... } i = i + 1; }`)
should run close to the host's `memchr` / `memmem`. The compiler does this
itself. There are no SIMD types in the language and no search natives:
coil-stdlib's `text` search is written as ordinary loops, and the compiler
vectorises them.

The work happens on the feature branch `feature/byte-simd`, one PR per
step.

## What exists before this work

- `V*` opcodes (`VLoad` / `VStore` / `VBin` / `VMove` / `VReduce` /
  `VFma`): 8 lanes of `i64` / `f64` in eight 64-byte vector registers,
  run by `coil-simd` with runtime ISA dispatch (SSE2 / AVX2 / AVX-512 /
  NEON). `mir/vectorize.rs` emits them only for counted map and reduce
  loops, and only when the whole function is that loop.
- `s[i]` on a string is a `HostInvoke string_byte_at` per byte, plus a
  `< 0` test for the out-of-bounds panic.
- `text::Pattern for string` calls the `string::find_from` native
  (`memchr::memmem`).

## Baseline

`examples/perf/text_scan_long.hy`: a 64 KiB line with the needle at the
end, and a 64 KiB line with a `,` every 1 KiB. `coil` release, x86-64
container, ns per call:

| Case | main `bb3996a` | step 1 |
|------|----------------|--------|
| Coil loop, find a byte | 6 270 000 | 1 900 000 |
| Coil loop, find `"needle"` | 7 480 000 | 2 120 000 |
| stdlib `find_in` (native), byte | 3 300 | 3 060 |
| stdlib `find_in` (native), `"needle"` | 3 450 | 2 480 |
| stdlib `split` on the sparse line | 36 800 | 35 500 |

The plain loop costs about 96 ns per byte, about 1 900 times the native.

## Design

### 1. `s[i]` as a dense op

A string byte read becomes a dense-native index (`DenseIndex` with a
string flag) instead of a `HostInvoke`. This speeds up every byte loop
and the scalar tail of a vector loop.

### 2. Byte lanes in the VM

A vector register is 64 bytes, so byte lanes reuse it as 64 `u8` lanes:

- `VLoad` with a byte type loads 64 bytes from a string or a `Vec<byte>`
  (packed or word storage).
- `VBin` byte kinds: compare two vectors (`==`, `<` unsigned, ...), and
  compare a vector against a scalar slot. The scalar form takes the full
  `i64`, so `s[i] as int == b` with `b` outside `0..=255` is all false,
  as the scalar loop is.
- Mask logic: `and`, `or`, `andnot` on lane masks.
- `VMask`: movemask of 64 lanes into an `i64` slot (bit `k` = lane `k`).
- Dense unaries `ctz` (count trailing zeros) and `blsr` (clear the
  lowest set bit), both non-trapping.

`coil-simd` runs each op as four 16-byte SSE2 / NEON steps, with AVX2 and
AVX-512 used when the host has them. A scalar reference stays in
`scalar.rs`.

### 3. Vectorising early-exit loops (skip-ahead)

A new MIR pass (`mir/byte_scan.rs`, dense tier) looks for a loop with an
`i64` induction variable stepped by 1 and a bound test, where an
iteration can be a *no-op*: the path from the loop body to the latch
reads bytes at `arr[iv + inv]`, compares them, and changes nothing but
the induction variable. The pass derives the lane predicate `P` = "this
iteration does something" from the body's branches (so `&&` / `||`
chains work) and puts a skip-ahead block in front of the old header:

```text
H': i' = φ(init, i + 1 from latch, i' + 64 from S1), other φs likewise
    if i' + 64 > bound or a read would leave its array: go to H(i')
S0: m = ByteMask(P, lanes i' .. i' + 64)
    if m == 0: S1: i' + 64, back to H'
S2: i2 = i' + ctz(m), go to H(i2)
H:  the old loop, now entered at i2, its latch going back to H'
```

The scalar loop runs only at candidates and near the end, so the
semantics (bounds panics, side effects, other loop values) are the
original loop's. The same rule covers:

- find a byte (`s[i] == c`),
- the first-byte filter of a substring search, with the full compare
  only at candidates,
- the inner compare loop (`while j < m && s[i + j] == p[j]`): two byte
  streams, skipping lanes that match,
- counting loops (`if s[i] == c { n = n + 1; }`), whose count is
  unchanged on a skipped iteration.

`ByteMask` is one MIR instruction producing an `i64`. Vector registers
live only inside its expansion, so no vector value crosses MIR
instructions and register allocation does not change.

### 4. Userland search on it

coil-stdlib's `Pattern for string` (`find_in`, `is_prefix_of`, ...) is
rewritten as plain loops. Whether the `string::find_from` / `rfind` /
`match_at` natives are then removed is decided on the numbers.

## Steps

| Step | PR | Change |
|------|----|--------|
| 0 | #1951 | This plan and `examples/perf/text_scan_long.hy` |
| 1 | | `s[i]` dense: `DenseIndex` flag `HEAP_STR_BYTE`, archive minor 45; the plain loop goes from 96 to 29 ns per byte |
| 2 | | VM byte lanes (`VLoad` `TY_U8`, `VBin` `M_*` / `B_CMP` / `BS_CMP`), `VMask`, dense `UNARY_CTZ` / `UNARY_BLSR`; `coil_simd::bytes64` (SSE2 / NEON baseline, no dispatch); archive minor 46 |
| 3 | | MIR `byte_scan` skip-ahead pass |
| 4 | | coil-stdlib `text` search in plain loops |
| 5 | | Website docs; feature branch to `main` |
