# coil — AGENTS

coil: statically typed `.hy` → stack IL → `.hyc` archive → custom VM.

| Need | Read |
|------|------|
| Write / edit `.hy` | `.cursor/skills/coil-language` · [coil-website](https://github.com/ardax-corp/coil-website) `src/content/docs/` (`/docs/…`) |
| Compiler, VM, pipeline | `.cursor/skills/coil-contributor` · `docs/internals/` |
| Hangs, panics, breakpoints | `.cursor/skills/coil-debug` · `docs/internals/debugger.md` |
| Known gaps / workarounds | `docs/internals/limitations.md` |

## User preferences

- Tests: `cargo test --workspace --lib --tests --bins` (required gate; covers integration tests, skips Criterion benches). Bare optional stack: `cargo test --workspace --lib --tests --bins --no-default-features`. Tooling: `--features <dissect|debugger>` with full test. VM-wire suites (pipeline, perf_metrics, …): `cargo test -p compiler --features vm-wire --tests`. Leak smoke: `cargo build --bin coil --bin coil-test && (ulimit -v 65536; ./target/debug/coil test -j 1)`. GC stress (precise-root soundness): `cargo build -p coil-test --features gc-stress --target-dir target/gc-stress && ./target/gc-stress/debug/coil-test`. Soft CPU: `./scripts/poop_baseline.sh`.
- Large tasks: scoped sub-agents on disjoint modules.
- VM perf: alloc reduction, hot-loop tuning, bounds-check elimination, `promise!` — not benchmark-shaped opcodes unless universal.
- **Hit-bench prove:** if an opt is sound but flagship `.hyc` (`mandelbrot` / `tak` / `nsieve` / `binary_trees` / `fib`) are identical, add focused `examples/perf` hit benches and prove those. Do not skip merge solely because flagships did not change; skip only on hit-bench wash/regress. Flagships remain controls. Landed: InstCombine (#304), try flatten (#307), LICM+integer SR (#315), TailCall (#316), Local CSE (#317), DestProp (#318), MIR InstCombine (#329), MIR DestProp (#330), MIR IV SR (#331), MIR cross-block GVN/PRE (`mir_gvn_divf`).
- Language features: draft plans; full HM; update coil-website user docs (`src/content/docs/`) and `docs/internals/` here when needed; minimal runnable example.
- **Method-based APIs** — prefer inherent/`impl` methods over free functions for type-tied operations (stdlib, new language surface, codegen fixes). Free generic fns returning enums are fragile today; see `docs/internals/limitations.md`.
- Granular conventional commits; stage only related files.
- Prefer compiler virtual modules over userland for core interpreter machinery; extracted features (regex, TLS, HTTP, collections) live in separate repos (`ardax-corp/coil-regex`, `coil-tls`, `coil-http`, `coil-stdlib`).
- **Userland package tests** — demos, native builds, and integration tests for extracted packages stay in their repos, not coil-lang `compiler/tests` or CI.
- **VM vs `.hy` tests** — prefer `.hy` language tests (`tests/positive/`, `coil test`) over Rust VM bytecode tests when coverage overlaps; remove duplicates.
- `cargo build` builds `coil` + `coil-debug` / `coil-dissect` / `coil-fmt` / `coil-lsp` / `coil-test` / `coil-embed`. `coil test` re-execs `coil-test` and `coil mutate` re-execs `coil-test mutate` (test-only VM features live there, never in `coil`). `coil-embed` is the packaged-app runner (`coil package` prefers it); not an embed-the-VM library.
- IL inspection: `coil dissect` — no verbose debug-build dumps.
- `coil fmt`: preserve `//` and `///`; wrap long lines; trailing commas on multi-line lists.

## Invariants (do not break)

- **Append-only opcodes** (`common/src/opcode.rs`). New variants at end → bump archive **minor**, `promise!` in `machine/src/vm.rs`, `instruction_from_u8_covers_last_appended_variant`. ABI break → **major** (reset minor).
- **Virtual-module natives** via `HostInvoke` — host wiring in `machine/`. Leftover TLS/crypto/regex slots were dropped (holes collapse); they are not reserved panic stubs. Virtual-time names stay as panic stubs so later ids do not move. `stream_attach` / `stream_park` own **119** / **120**. Process clocks: `clock_wall_nanos` / `clock_mono_nanos` / `clock_sleep_ms` are **121** / **122** / **123** (`use clock::{…}`). Archive is **major 4 / minor 35**. Minor 35 appends `contract_fail` (**159**, `HostOp::Task`: a failed `requires` panics naming the caller; see `docs/internals/contracts.md`). Minor 34 appends the wait-condition natives `task_cond_new` / `task_cond_wait` / `task_cond_notify` (**156–158**, `HostOp::Task`, behind `task::channel`). Minor 33 persists `cleanup_ranges` (per-function `defer` cleanup pads the VM unwinder runs on a panic or a task cancel; older archives load with none) and appends `unwind_resume` (**152**, `HostOp::Unwind`) and `task_cancel` / `task_shield_enter` / `task_shield_exit` (**153–155**, `HostOp::Task`). Minor 32 appends the task scheduler natives `task_scope_open` … `task_yield` (**144–151**, `HostOp::Task`, run by the VM; see `docs/internals/tasks.md`). Minor 31 appends byte-offset `string` natives (**139–143**). Minor 30 persists `debug_lines` (line/column per debug loc, resolved at compile time, so packaged binaries never read their sources; older archives fall back to the source files). Minor 29 persists `static_word_kinds` (per static slot word kind: scalar statics are no roots, pointer statics are precise roots evacuation rewrites). Minor 28 appends `MakeArrayK` (array literal with the pointer element kind). Minor 27 appends `TagArrayKind` (array element word kind, stamped by `Vec::{new,with_capacity,from}$ptr` thunks for ground pointer elements; pointer kind only). Minor 26 flags precise frame-map slots that definitely hold a pointer (`PRECISE_SLOT_MUST`, bit 15; strip with `precise_slot_index`). Minor 25 appends `MakeEnumK` / `MakeEnumReturnK` / `MakeTupleK` / `DenseMakeK` (construction-site payload / element word kinds, 2 bits × first four words). Minor 24 persists `ClassWordKinds` (per-class field word kinds: scalar / pointer / unknown; marking skips scalar fields, pointer fields are precise references). Minor 23 appends `TagEnumType` (enum `fn drop()`, COI-26). Minor 22 adds `PreciseFrameMap::frame_words` (frame extent for allocating dense bodies) and `DenseCast` kind `CAST_F2I`. Minor 21 persists precise frame maps (`PreciseFrameMap`; heap-free frames skip the conservative stack scan). Minor 20 appends HostInvoke `stream_fd` (**138**) for `Stream.fd()`. Minor 19 appends `MakeEnumReturn` (COI-388 X3). Minor 18 appends `DenseIndexJmpf` (COI-379 S4). Minor 17 appends `DenseBinJmpf` (COI-377 S1). Minor 16 appends `DenseBin2` (COI-381 S2). Minor 15 appends HostInvoke `thread_spawn_shared` (**137**, COI-365 E6). Minor 14 persists S2b stack maps (COI-359 E1). Minor 13 persists `operand_stack_slots` (COI-358 E0). Minor 12 appends `DenseFieldLoad` / `DenseFieldStore` / `DenseMakeObject` (COI-356 D2). Minor 11 appends `DenseArrayPush` (COI-344 B6). Minor 10 appends dense-native heap ops (`DenseIndex` / `DenseStoreIndex` / `DenseArrayLen` / `DenseMake` / `DensePush`). Minor 9 appends compiler-only `VReduce` / `VFma` (S5b V1). Minor 8 appends compiler-only SIMD opcodes (`VLoad` / `VStore` / `VBin` / `VMove`; eight numeric lanes via `coil-simd`). Minor 7 appends HostInvoke `simd_axpy_reduce` (**136**) for MIR saxpy-reduce packs. Minor 6 appends MIR dense numeric opcodes (`DenseBin` … `DenseCast`). Minor 5 appends M1 `prelude::math` after `result_unit_probe` (**124**): `math_atan` / `atan2` / `asin` / `acos` / `log10` / `log2` / `cbrt` / `rem` / `sinh` / `cosh` / `tanh` are **125–135**. Frozen `math_sin` … `math_pow` stay **102–110**. `PI` / `E` / `TAU` live in coil-stdlib `num`, not here.
- **No PGO.** Removed (#301). Branch layout is heuristic. Do not revive `--pgo-*`, ingest, heat knobs, or `BranchProfile`.
- **Feature gates**: debugger `feature = "debugger"`; dissect `feature = "dissect"`; coverage `feature = "coverage"` (`coil-test` only) — on helper binaries, not default `coil`. They gate hook *state and APIs* only: the VM dispatch loop is `execute::<HOOKS>`, instantiated with hooks only while a debugger / coverage collector is attached, so a binary that carries the features through workspace unification (a bare `cargo build --release`) pays nothing per instruction (#558). Release still builds `coil` / `coil-embed` in their own cargo invocation.
- **Lint gate**: `cargo clippy --workspace --lib --tests --bins -- -D warnings` (also `--no-default-features`, `--features dissect`, `--features debugger`). `Gc::payload_mut` keeps `&self -> &mut T` and allows `clippy::mut_from_ref` on that method only.
- **Fuse-select (D4)**: one named pass on typed `IlOp` after concat (`fuse_select` → PC assign). Residual `Byte` is a cold refuse. No post-lower `adjust_target`, no production per-fn fuse.

- **IL** is instruction lowering + label resolution + fuse — not a semantic IR. DefIds / typed sidecar hold names, types, and call meaning. See `docs/internals/pipeline.md` (IL intent).
- **Single emit sink**: production codegen pushes through `CodeBuf` / `IlBuilder` only. Encode in `il::lower`.

Codegen / match / `STORE`: `.cursor/skills/coil-contributor/reference.md`. Pipeline: `docs/internals/pipeline.md`.

## Cloud agents

Pre-installed: `poop`, `valgrind`, `heaptrack`, `hyperfine`, `lua` (`.cursor/Dockerfile`). Use `--release` for benchmarks.

## Learned User Preferences

- Aim for a regular language: one spelling per construct; do not add case-specific optimization workarounds that only serve a bench.
- Dual syntax: drop C-style `for (init; cond; step)` (keep `while` and `for x in`); canonical length is `x.len()` with free `len()` as prelude sugar; canonical `readonly` is before the value (`readonly new C(...)`, `readonly [...]`).
- Linear tickets: one PR per issue, base on `main`, babysit until CI is green and merged before starting the next.

## Learned Workspace Facts

- Regularity target: one ground-call convention, panic on OOB (not `-1` / no-op); `a[i]` stays type `T`. `Option`/`Result` use niche / two-slot / boxed by shape (COI-92); arity-2 immediate products use two-slot `[a, b]` on direct `CALL`/`RETURN` (#302). Fuse opcodes are debt — rewrite existing ops rather than growing bench-shaped fuses.
