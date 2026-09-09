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
- MIR S5a V0 — `VLoad` / `VStore` / `VBin` / `VMove` on stride-1 numeric
  stores (`compiler/src/mir/vectorize.rs`). VM glue in `machine/src/simd.rs`
  calls `coil_simd::lanes` only.
- MIR S5b V1 — `VReduce` (left-fold add into a scalar slot) and `VFma`
  (mul-then-add, two IEEE roundings). Same refuse map as V0. Prove:
  `scan` in `vec_scan.hy`, saxpy store in `vec_axpy.hy`.
- String intern table lookup (`Heap` hash map) uses `bytes::eq` for key
  compares.

Matrix `*` remains matmul; Hadamard `*` on bare tuples/arrays uses
`packed_vec_arith`.

## Extending

Add ISA-specific modules under `coil-simd/src/{x86_64,aarch64}/`, keep a scalar
reference in `scalar.rs`, and dispatch from `kernels.rs` / `bytes.rs`. Prefer
runtime feature checks over forcing wider ISAs at compile time.
