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
  negation, casts between int and float lanes (`VBin` `I2F` / `F2I`,
  archive 6.11; a cast of an invariant is done once in front), selects
  (`VBin` `SELECT`, archive 6.11, below), and `VFma`
  (mul-then-add, two IEEE roundings). A guard in front of the loop checks
  every read and write of lanes `i0 .. n` is in range; `VLoop` runs the
  whole 8-lane blocks with its body words (`VLoad` / `VStore` / `VBin` /
  `VFma` / `VReduce`) decoded once and no dispatch per block, and the
  scalar loop takes the last lanes. A failed guard runs the loop as
  written. `VReduce` folds left (float sums keep their order; an int
  fold traps on the first partial that overflows). A loop may store and
  fold in one body (`feature/locality` step 2): the stores run before the
  fold. A block stores only once its other words ran; a block that traps
  runs again lane by lane, so a panic leaves the scalar loop's stores.
  Store loops keep the
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
- Archive 6.11 if-converts a store loop through one `if`
  (`vectorize::match_if_store_on`): a φ at the join
  (`if x < 0.0 { x = -x; } out[i] = x;`) is `cond ? from_then :
  from_else`, a store in each arm to the same element is one store of a
  select, and a store in one arm alone keeps the element on the other
  side. Every lane runs both arms, so an arm may only compute what
  cannot trap (float arithmetic, bit ops, compares, casts, loads the
  guard covers, and `int` `+ - *` whose operand ranges keep it in
  range, like `(c as int) + 32` on a byte); any other `int` add in an
  arm keeps the loop scalar. The mask
  is the compare as written (`0` / `1` lanes, `&&` of compares as
  `IAND64`), so a NaN takes the side the scalar branch takes. A body
  word computes each distinct node once (`emit::vec_loop_body`) and
  reuses a register after its last reader, falling back to the tree
  when that still needs more than the eight registers.
  `examples/perf/vec_lanes.hy`, 8192 elements: abs 170 → 76 µs, clamp
  with a store per arm 178 → 72 µs.
- Archive 6.11 also runs a counted loop whose body is one
  `out.push(value(i))`, or a push in each arm of an `if` (a select of the
  two values), as a `VLoop` append (`vectorize::match_push_on`, `VStore`
  flag `STORE_PUSH`): each block appends its 8 lanes once its other words
  ran, and a block that traps runs again lane by lane, so a panic leaves
  the pushes before it. The guard also checks `out` is none of the
  arrays the body reads nor the bound's array, and the invariant ranges
  an arm's ranged add rests on (`(c as int) + delta` with `delta` in
  range). `vec_lanes.hy`: `out.push(a[i] * 3 + 1)` 284 → 69 µs, the
  `shift_ascii` shape on `Vec<byte>` 504 → 127 µs.
- Archive 6.13 lets a map-push or store map read a `string`
  (`vectorize::string_reads`, #2150): each `s[i + k]` is a block of its
  own (`string_byte_at` and the branch to its panic) at the head of the
  body, and is read as an int lane (`VLoad` of the string's bytes). The
  guard puts every lane in `0 ..= s.len()`, so no read in those lanes
  takes its panic branch; each read must be one of the loads the guard
  checks. Covers `text::shift_ascii` and `ascii::to_lower` / `to_upper`.
  `vec_lanes.hy`, 8192 bytes: `shift_str` about 530 → 120 µs,
  `lower_str` (a store per arm) about 390 → 115 µs; `shift_ascii` over
  1 MiB about 80 → 15 ms.
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
  `src` may also be a `string` (`out.push(s[i])`, or `s[i] as int`):
  its read is a block of its own (`string_byte_at` and the branch to its
  panic), and the call appends the bytes. A runtime that does not copy a
  string source answers `i0`, so the loop runs scalar there and no
  archive bump is needed. Copy 1 MiB of a string 32 000 000 → about
  150 000 (`feature/simd-lanes` step 5).
- MIR `copy_until` (`compiler/src/mir/copy_until.rs`, dense tier,
  before `byte_scan`) — a loop that pushes each byte it reads until a
  byte stops it (`if c == "," || c == "\n" { break; } out.push(c);`)
  gets a guarded scan and copy in front of it: the test's blocks cloned
  into a loop that changes nothing but its index (so `byte_scan` takes
  it), then one `vec_extend_range` of the run, and the loop entered at
  the stop, where it leaves through its own `break`. The test may only
  read `src[i]`, compare and mask, and the guard keeps the reads in
  range (`0 <= i0 < n <= src.len()`). `vec_bulk.hy`: copy 1 MiB up to a
  quote 41 000 000 → about 300 000 (`feature/simd-lanes` step 6).

Matrix `*` remains matmul; Hadamard `*` on bare tuples/arrays uses
`packed_vec_arith`.

## Extending

Add ISA-specific modules under `coil-simd/src/{x86_64,aarch64}/`, keep a scalar
reference in `scalar.rs`, and dispatch from `kernels.rs` / `bytes.rs`. Prefer
runtime feature checks over forcing wider ISAs at compile time.
