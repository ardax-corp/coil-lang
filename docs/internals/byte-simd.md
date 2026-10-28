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

| Case | main `bb3996a` | step 1 | step 3 | step 4 |
|------|----------------|--------|--------|--------|
| Coil loop, find a byte | 6 270 000 | 1 900 000 | 11 000 | 2 600 |
| Coil loop, find `"needle"` | 7 480 000 | 2 120 000 | 225 000 | 125 000 |
| stdlib `find_in`, byte | 3 300 | 3 060 | 1 950 | 3 300 |
| stdlib `find_in`, `"needle"` | 3 450 | 2 480 | 2 400 | 4 000 |
| stdlib `split` on the sparse line | 36 800 | 35 500 | 36 000 | 52 500 |

Through step 3 the stdlib rows call the `memchr` native; from step 4 they
are plain Coil loops (the native's numbers on the step 4 binary: 1 600,
2 400 and 35 600).

On main the plain loop costs about 96 ns per byte, about 1 900 times the
native. After step 3 the byte loop is about 0.17 ns per byte, 5 to 6
times the native `memchr`. The substring loop stops at every `n` (once
per 64 bytes in this line) and runs the scalar compare there, so it
costs about 220 ns per candidate: a first-byte filter, where `memmem`
filters on two bytes. The stdlib rewrite (step 4) filters on two bytes
in plain Coil.

Step 4 tuned the VM side: a scan that is too short for 64 lanes returns
before decoding its body, each stream loads once per block, the range
checks are hoisted out of the block loop, and the two commonest bodies
(one stream against a byte, two offsets of one string against two
bytes) run as tight loops over the string. A dense `s[i]` also pins its
string per register, so a loop reading two strings stops looking them up
on every read. The stdlib's plain-loop search then runs within 2 times
the native for a byte and the two-byte filter, and `split` within
1.5 times (each piece pays a call and a scan setup).

A first attempt kept the 64-byte loop in MIR (a `ByteMask` instruction
per block, then `== 0`, `+ 64` and a range test as ordinary ops). That
cost about 7 dispatches and 95 ns per 64 bytes (find a byte: 98 000 ns),
so the loop moved into the VM as one `VScan` op.

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

`coil_simd::bytes64` runs each op as four 16-byte SSE2 / NEON steps (the
targets' baseline, so no runtime dispatch). Its `scalar` module is the
reference in tests and the fallback on other targets.

### 3. Vectorising early-exit loops (skip-ahead)

A new MIR pass (`mir/byte_scan.rs`, dense tier) looks for a loop with an
`i64` induction variable stepped by 1 and a bound test, where an
iteration can be a *no-op*: the path from the loop body to the latch
reads bytes at `arr[iv + inv]`, compares them, and changes nothing but
the induction variable. The pass derives the lane predicate `P` = "this
iteration does something" from the body's branches (so `&&` / `||`
chains and their `bool` φs work) and puts a scan in front of the old
header:

```text
G:  i' = φ(init, i + 1 from latch), other φs likewise
    i2 = ByteScan(P, from i', while i' <= bound - 64)
H:  the old loop, now entered at i2, its latch going back to G
```

`ByteScan` is one MIR instruction and one VM op, `VScan`. Its body is
the predicate as byte-lane `VLoad` / `VBin` words, run by the VM in a
loop without dispatch: from `i'`, 64 lanes at a time, it stops at the
first lane where `P` holds, at the bound, or where a read would leave
its string, and returns that position. The strings are resolved once per
scan. Vector registers live only inside the op, so no vector value
crosses MIR instructions and register allocation does not change.

The scalar loop runs only at candidates and near the end, so the
semantics (bounds panics, side effects, other loop values) are the
original loop's. The same rule covers:

- find a byte (`s[i] == c`), and byte classes (`c >= 48 && c <= 57`,
  `c == a || c == b`, `c != x`),
- the first-byte filter of a substring search, with the full compare
  only at candidates,
- the inner compare loop (`while j < m && s[i + j] == p[j]`): two byte
  streams, skipping lanes that match,
- counting loops (`if s[i] == c { n = n + 1; }`), whose count is
  unchanged on a skipped iteration.

The range setup it adds sits in the loop's preheader; loop-invariant
code motion runs again after the pass, so for an inner loop it moves
out of the outer one.

Integer edges: the pass computes `bound - 64` and stream offsets with
ordinary trapping `i64` arithmetic in the preheader, so a bound within
64 of `i64::MIN` traps where the scalar loop would not. No string or
array is that long, and a hand-written bound that low runs no iterations
anyway.

### 4. Userland search on it

coil-stdlib's `Pattern for string` (`find_in`, `is_prefix_of`, ...),
`rfind` and `lines` are plain loops over `s[i]`; `find_in` filters
candidates on the needle's first two bytes. With them within 2 times the
native (see the table above), the `string::find_from` / `rfind` /
`match_at` / `byte_at` natives left the language (Dimitar, 2026-10-23):
userland reads bytes with `s[i]`. The VM keeps their HostInvoke rows for
older archives, and `string_byte_at` is still what `s[i]` lowers to.

### 5. Backward loops (#1968)

A count-down loop (`i = i - 1`, or `i + -1`, tested `i >= lo` / `i > lo`
in either operand order or as the exit test) gets the same scan with the
`VScan` flag `simd::SCAN_BACKWARD` (archive minor 48; it takes bit 6 of
the length field, so a body is at most 63 words). From `p = from` it runs
the body over lanes `p - 63 ..= p` while the lowest one passes the bound
and every read stays in its string, stops at the highest lane where `P`
holds (`63 - lzcnt(mask)`), else steps `p -= 64`. coil-stdlib's
`Text::rfind` is that loop with the needle's first and last bytes as the
filter.

A backward byte search over 1 MB (`while i >= 0 { if s[i] == b ... i = i - 1; }`,
2000 passes, the byte at offset 0): 55.9 s before, 0.30 s with the scan.

### 6. Per-match cost (#1976)

`a && b` lowered to a join block holding only `φ(false, b)` and a branch
on it: `DenseCmp; LOAD; JMPF` per candidate, leaving the hot streak. MIR
`thread_bool_phis` (`mir/bool_thread.rs`) makes each way in branch for
itself, so the test is one fused compare-branch, and `LaneExpr::not`
pushes a negation through `&` / `|` when that costs no nodes, so the
threaded two-byte filter is `(s0 == k0) & (s1 == k1)` again (the VM's
`scan_pair` shape).

Callgrind, 20k pieces of 62 bytes joined by `" "`, 5 passes (instructions
per match, compile subtracted): `split(" ")` 6450 → 6408,
`replace_all("xyz", "XYZ!")` 11455 → 10863. What is left per match is
the calls into `find_at` / `span_text` and the slice's allocations (the
string and its `Result`), not the scan.

### 7. `Vec<byte>` streams

The libraries (json, csv, url, deflate, http, `bytes::*`) loop over
`Vec<byte>`, not `string`. A byte array read is a plain MIR `Index`, and
MIR types do not say what an array holds, so HIR lowering marks each
`Index` whose element is `byte` (a `Vec<byte>` or `[byte; N]`) in
`MirFunc::byte_loads`. `byte_scan` takes those as streams beside
`string_byte_at`; the VM's `VScan` already loads packed and word byte
arrays.

An array, unlike a string, can change while the loop runs: the
iterations that run scalar may store into it or push to it. Each scan
reads the array as it is when the scan starts, and an array bound's
`len()` is taken again at every scan instead of once in the preheader
(LICM still hoists it when nothing in the loop resizes an array). An
out-of-range read panics, as `string_byte_at`'s does, and the scan stops
short of it, so the scalar loop still raises it.

The walk's masks also fold back the branches of a long `&&` chain: `or`
pulls out the terms two `&` chains share (`(x & a) | (x & !a)` is `x`),
and `and` drops a chain the other side already holds. Without this a
four-way whitespace test (`c != 32 && c != 9 && c != 10 && c != 13`)
built a 37-node mask, past the 24-node limit, and never scanned (on
`string` too).

`examples/perf/vec_byte_scan.hy`, `coil` release, ns per call:

| Case | main `435021f` | after |
|------|----------------|-------|
| find a byte, 1 MiB | 13 100 000 | 22 600 |
| match length, 32 KiB (deflate) | 513 000 | 3 000 |
| skip JSON whitespace, 1 MiB | 9 120 000 | 227 000 |

String loops are unchanged (find a byte, a substring and a two-byte
filter on 64 KiB, within noise).

## Steps

| Step | PR | Change |
|------|----|--------|
| 0 | #1951 | This plan and `examples/perf/text_scan_long.hy` |
| 1 | #1953 | `s[i]` dense: `DenseIndex` flag `HEAP_STR_BYTE`, archive minor 45; the plain loop goes from 96 to 29 ns per byte |
| 2 | #1954 | VM byte lanes (`VLoad` `TY_U8`, `VBin` `M_*` / `B_CMP` / `BS_CMP`), `VMask`, dense `UNARY_CTZ` / `UNARY_BLSR`; `coil_simd::bytes64` (SSE2 / NEON baseline, no dispatch); archive minor 46 |
| 3 | #1963 | MIR `byte_scan` pass: `ByteScan` / VM `VScan` (archive minor 46); find a byte 6.3 ms to 11 µs per 64 KiB |
| 4 | | coil-stdlib `text` search in plain loops (coil-stdlib, two-byte filter); VM scan fast paths, string pins |
| 5 | | Website docs; feature branch to `main` |
| 6 | | `Vec<byte>` streams (`MirFunc::byte_loads`); `&&` chains fold back |
