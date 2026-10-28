# SIMD (`coil-simd`)

Stable-Rust helpers over [`std::arch`](https://doc.rust-lang.org/stable/std/arch/)
for dense numeric and byte kernels. Coil does **not** use nightly
`portable_simd`.

## Why a workspace crate

HostInvoke packed linear algebra (`machine/src/packed_la.rs`) already batches
matrix work off the opcode dispatcher. Contiguous `f64` / `i64` loops are the
right place for SIMD — not new bytecode ops (see AGENTS.md: prefer kernel
tuning over benchmark-shaped opcodes).

`coil-simd` keeps the unsafe ISA details out of the VM and exposes a small
safe API:

| API | Role |
|-----|------|
| `detect()` / `SimdLevel` | Cached runtime probe (`SSE2` / `AVX2` / `AVX-512` / `NEON` / scalar) |
| `dot_f64` / `dot_i64` | Dot products |
| `matmul_f64` / `matmul_i64` | Row-major GEMM (`C = A·B`) |
| `zip_{add,sub,mul,neg}_{f64,i64}` | Element-wise zip / negate (`zip_div_f64` too) |
| `scale_f64` / `scale_i64` | Broadcast multiply (`out[i] = a[i] * s`) |
| `axpy_reduce_f64` | Counted `s = (s + a*x) + y; x += dx` (P12 HostInvoke) |
| `lanes::fold_add_*` / `fmadd_*` | V1 horizontal left-fold add; conservative mul-then-add FMA |
| `bytes::eq` / `bytes::xor` | Byte equality and XOR |
| `zip_i64_op` / `zip_i64_not` / `zip_f64_mask` | Matrix cell compares, bitwise ops, intersect, and diff |

## Dispatch

- **x86_64:** SSE2 is ABI baseline; AVX2 (+FMA when present) and AVX-512
  (`avx512f` + `avx512dq` + `avx512bw`) are selected at runtime via
  `is_x86_feature_detected!` — no `RUSTFLAGS=-C target-cpu=…` required for
  correctness. Partial AVX-512 (missing DQ/BW) falls back to AVX2.
- **aarch64:** NEON.
- Tiny inputs fall back to scalar to avoid call overhead.

`i64` multiply stays scalar on SSE2/AVX2 (no general 64-bit `mullo`); AVX-512DQ
enables vectorized `i64` dot / matmul. Zip add/sub/neg vectorize on all levels.

## Measuring SIMD vs scalar

```bash
cargo run -p coil-simd --release --example simd_report
cargo bench -p coil-simd --bench simd_vs_scalar
```

`simd_report` compares public runtime-dispatched kernels against `coil_simd::scalar`
on the host ISA (`SimdLevel`). Release builds often auto-vectorize the scalar
loops; for a stricter non-SIMD baseline:

```bash
RUSTFLAGS="-C llvm-args=-vectorize-loops=false -C llvm-args=-vectorize-slp=false" \
  cargo run -p coil-simd --release --example simd_report
```

On an AVX-512 host (Ryzen 7 7700), representative speedups with auto-vec
**disabled** were roughly: `dot_f64` **~4–8×**, `matmul_f64` **~1.6–3.7×**,
`matmul_i64` **~2.4–3.8×**, `zip_add_f64` **~1.5–3.4×**. `bytes::eq` does not
beat Rust/`memcmp` slice equality.

## Callers today

- `machine::packed_la` — `packed_dot` / `packed_matmul` / `packed_matrix_zip` /
  `packed_matrix_neg` / **`packed_vec_arith`** (1-D aggregate `+ - * /` zip,
  broadcast, and unary `-` when static length ≥ 8; smaller shapes still unroll)
- MIR P12 — `simd_axpy_reduce` HostInvoke (**136**) from specialized counted
  saxpy-reduce loops (`compiler/src/mir/pack.rs`). Compiler rewrite only.
- MIR `vec_loops` (`compiler/src/mir/vec_loop.rs`, dense tier) — a
  counted stride-1 store loop (`a[i + k] = value(i)`) or add / mul
  reduction (`s = s ⊕ term(i)`) over `i64` / `f64`, anywhere in a
  function, becomes one `VecLoop` MIR instruction and one `VLoop` op
  (archive 6.2). `vectorize.rs` matches the loop and reads its body as a
  tree: loads `a[i + k]`, splats of invariants, the index, `+ - * /`,
  negation, an int-to-float cast of an invariant, and `VFma`
  (mul-then-add, two IEEE roundings). A guard in front of the loop checks
  every read and write of lanes `i0 .. n` is in range; `VLoop` runs the
  whole 8-lane blocks with its body words (`VLoad` / `VStore` / `VBin` /
  `VFma` / `VReduce`) decoded once and no dispatch per block, and the
  scalar loop takes the last lanes. A failed guard runs the loop as
  written. `VReduce` folds left (float sums keep their order; an int
  fold traps on the first partial that overflows). Store loops keep the
  scalar order however the arrays alias: every read before the first
  store, one store offset `S`, no read at an offset below `S`. VM glue
  in `machine/src/simd.rs` calls `coil_simd::lanes` only.
  `examples/perf/vec_reduce.hy`, 8192 elements, release, ns per call
  (main `435021f` → after): `sum` (the function is only the loop, which
  the old whole-function vectoriser took) 22 300 → 5 150, the same sum
  after an early return 66 900 → 5 150, `dot` 36 300 → 23 900, a
  scaled store 99 300 → 32 300.
- Archive 6.3 adds the reductions through one `if`
  (`vectorize::match_cond_reduce_on`): a select `if x > m { m = x; }`
  (`<`, `<=`, `>=`, either operand order, int or float) folds with
  `REDUCE_TAKE + pred`, lane by lane in order, so ties, NaN and signed
  zeros come out as the scalar loop's; a conditional add
  `if c { n = n + t; }` on `int` adds `c ? t : 0`, the compare as `0` /
  `1` lanes (`VBin` `ICMP` / `FCMP + pred`) times `t`. Every lane runs
  the vector body, so the value a select takes is the one the body
  compared (an arm may compute it again), a conditional term cannot trap
  (a load, an invariant or the index), and a float compare is used only
  as written (taken on its false side it would differ at a NaN). Bit
  folds `REDUCE_AND` / `OR` / `XOR` and bit lanes (`VBin` `IAND64` /
  `IOR64` / `IXOR64`) cover `h = h ^ x[i]` and `out[i] = a[i] & m`.
  Same probe: max 68 000 → 5 900, count of one value 69 000 → 19 600,
  xor 63 000 → 7 500.
- String intern table lookup (`Heap` hash map) uses `bytes::eq` for key
  compares.
- `packed_matrix_zip` / `packed_matrix_neg` also lower matrix compares
  (`==` `!=` `<` `<=` `>` `>=`), bitwise `&` `|` `^` `<<` `>>` `~`, and
  `intersect` / `diff`. Compare, intersect, and diff return a `byte` mask of
  `0`/`1`. Integer kinds other than arithmetic `>>` use AVX2 (4-wide) on
  AVX2 and AVX-512 hosts; `>>` and float masks stay scalar and match IEEE
  / arithmetic shift. Shift counts use the low 6 bits.

- MIR `bulk_push` (`compiler/src/mir/bulk.rs`, both tiers) — a counted
  loop whose whole body is `out.push(src[i])` or `out.push(k)` becomes
  one HostInvoke, `vec_extend_range` (**174**) or `vec_fill` (**175**),
  archive 6.1. A guard in front of the loop checks
  `0 <= i0 < n <= src.len()` (`n < src.len()` for `<=`); the call leaves
  `i` at the bound and the loop is entered there, so it exits at once
  with its values. A failed guard runs the loop as written (its
  out-of-range panic included). `vec_extend_range` appends nothing when
  `out` is `src`, so a loop that grows what it reads stays scalar. A
  packed byte range is one `memcpy`. `examples/perf/vec_bulk.hy`,
  release, ns per call (main `435021f` → after): copy 1 MiB of bytes
  17 400 000 → 280 000, fill 1 MiB 7 500 000 → 208 000, copy 128 Ki ints
  2 156 000 → 267 000.

Matrix `*` remains matmul; Hadamard `*` on bare tuples/arrays uses
`packed_vec_arith`.

## Extending

Add ISA-specific modules under `coil-simd/src/{x86_64,aarch64}/`, keep a scalar
reference in `scalar.rs`, and dispatch from `kernels.rs` / `bytes.rs`. Prefer
runtime feature checks over forcing wider ISAs at compile time.
