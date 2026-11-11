# Internals

How coil is structured for contributors. End-user language docs live in [coil-website](https://github.com/ardax-corp/coil-website) (`src/content/docs/`; site routes `/docs/<path>` until a public domain is set).

| Document | Contents |
|----------|----------|
| [Pipeline](pipeline.md) | Parse → typecheck → HIR (inline, optimize) → MIR for every body → archive → native code or the MIR interpreter |
| [Archive history](archive-history.md) | Major 8 (the 2026-11 cleanup), major 7 (MIR and data only) and older minors (5–37) |
| [Dictionary passing](dictionary-passing.md) | Calling convention for trait-bounded generics that are not monomorphized |
| [Type unification](type-unification.md) | Unification rules of the checker, by type shape |
| [Comptime](comptime.md) | Code that runs while compiling: consts, `comptime` expressions and blocks, macros, folding; stages, limits, IO grants, result cache, tooling |
| [WebAssembly](wasm.md) | `coil` for wasm32-wasip1 (the website playground): 64-bit `Value` words, no FFI / sockets / OS threads, threads run when joined, `// @needs` test header, running the corpora under Node |
| [Compiler peak memory](compile-memory.md) | Where `coil compile`'s own peak RSS comes from per phase, how it was measured, what was cut (2026-10-27: spool 93 → 65 MiB) and what is left |
| [Build cache](build-cache.md) | #1881: `coil FILE` / `coil compile` reuse the archive of an unchanged program from `~/.cache/coil/build`; key, inputs, what is not cached, `--no-build-cache` |
| [Macros](macros.md) | User `derive` / attribute macros: `macro` model, `quote`, compile-time stage on the MIR interpreter, hygiene, sandbox |
| [HIR → MIR lowering plan](hir-mir-lowering-plan.md) | Done 2026-11 (plan and execution record): drop the emit cursor and phased emit, number frames from HIR, lowering asks for what it needs (no `HirMirInput` / `HirMirTables`), plan each body once |
| [MIR](mir.md) | Every body lowers from HIR to typed SSA, which every engine runs; the passes by level; two-word values |
| [MIR language islands](mir-islands.md) | Historical: COI-292 I0–I8 ladder that grew MIR coverage before every body went through MIR (M4/M5, 2026-10) |
| [Int overflow](int-overflow.md) | Int `+ - * / % **` and negation give the exact result or panic; where the engines, folds, optimizations, auto-par, SIMD and packed kernels keep that |
| [Locality and loop optimizations](locality.md) | `feature/locality`: vectorize nested and mixed loops, field scalar promotion, mod/ref summaries, array-order GC copying, prefetch, loop interchange; semantics rules and probe numbers |
| [Copy-on-write across calls](cow-calls.md) | who shares an argument or result at a call: summary bits `returns_arg` / `returns_inner`, moves into a call (`v = f(v)`, `self.f = g(self.f)`), lazy share for opaque callees (#2206); loop views of nested write paths (#2351) |
| [View classes](view-classes.md) | `view class` and `mut` fields: borrows a class can hold under copy-on-write; E0149 / E0150 / E0151 rules, COW handling, spliced views at zero cost |
| [Escape facts and early freeing](escape-memory.md) | `feature/escape-memory`: escape summaries across calls, freeing at scope end, static uniqueness, nursery; the GC stays as backstop |
| [Language quirks](language-quirks.md) | Locked Q1–Q9 (2026-09-10): `[T; N]` box-once, class field-SROA, grow type error, defined `i % N`, panic vs raise, roadmap Q6–Q9 |
| [`if` expression](if-expression.md) | #1877: `if … else …` / `if let … else …` as a value; primary precedence like `match`; E0137 for a missing `else`; reuses the `If` / `Match` HIR |
| [Q6 iterator protocol](q6-iterator-protocol.md) | Counted desugar for `for` (array / literal range / B5 locals / C2 param+returned Range / C2b heap-field + user `Iterator::next` + dict + coro) |
| [Q9 format / string](q9-format-string.md) | I4 reopen: SSA form of `STRING` / `PRINT` / `FORMAT` / `STRINGIFY`; R2 bytes HostInvoke; R3 maps; R4 unicode leftover |
| [Static trait methods](static-trait-methods.md) | #524: `static fn` in traits, `Owner::m` / `T::m` calls, return-type-directed bound resolution, per-field `#[derive(Default)]` |
| [Generic instances](generic-instances.md) | #550 / #551 / #552: `impl Trait for Box<T>` and `Box<T: Bound>`, instance contexts, context dictionaries, open-goal dictionary resolution, derives on generic types |
| [Per-type clones](generic-specialization.md) | #1417 / #1419: ground dictionaries built once into statics; per-type clones of other modules' generic methods and free functions (`ModuleCtx`); allocation numbers |
| [Userland text search](userland-text-search.md) | `split` / `find_all` / `replace_all` in Coil at near-native speed: per-step profile and measurements (feature/userland-loop) |
| [Const tables](const-tables.md) | #1561: `const T = readonly [...]` lookup tables: a local one built once on first use, constant-index reads folded, variable-index reads invariant in LICM / CSE; shared-heap workers read the spawner's statics; measurements |
| [Opt generalization](opt-generalization.md) | Historical: COI-333 A0 doctrine from the bytecode tiers, since deleted; **B0** audit + **B1** Q6–Q8 entry hygiene |
| [Limitations](limitations.md) | Known gaps, workarounds, and tracking |
| [Value representation](layout.md) | `rep(T)` per static type: niches, the two-slot call convention, shared generic bodies, the representation matrix |
| [Optimization roadmap](optimization-roadmap.md) | AOT/JIT plan; landed opts (#304–#318); hit-bench prove rule; PGO removed (#301). Extra benches: `gc_churn`, Option/Result churn, `iv_mul_sr` / `licm_nested_chains` / `tail_sibling` / `cse_*` / `dest_prop_field_alias` |
| [Packed byte arrays](packed-bytes.md) | #927: `Vec<byte>` / `Vec<bool>` one byte per element; lossless `Bytes` storage, `WORD_BYTE` construction stamp, byte copies for I/O / FFI / threads; measurements |
| [Heap identity](heap-identity.md) | Mapped slab + header poison for `find_object_by_addr` (COI-200) |
| [Incremental GC](gc-incremental.md) | COI-309 S4: safepoint mark (no write barrier) + lazy sweep; moving GC deferred |
| [GC evacuation](gc-evacuation.md) | Mostly-copying evacuation of sparse chunks (always on; `COIL_GC_COMPACT=0` off in unit tests); precise refs rewritten, ambiguous ones pin |
| [SIMD](simd.md) | `coil-simd`: packed LA and the lane kernels behind `VecLoop` and `ByteScan` |
| [Auto-par](auto-par.md) | Purity + parameterized AlwaysPar IPA (COI-366 F1 hop/grain); F2 broader admission (COI-368); loop IPA (COI-362 E4); C1/C2 shared-heap spawn (E6/E7); [branch-miss notes](auto-par-branch-misses.md) |
| [Optimizer hints](opt-hints.md) | `#[inline]` / `#[cold]` / `#[hot]`: surface, precedence, cold inference, `feature/opt-hints` steps (no PGO) |
| [Native code](native.md) | MIR to Cranelift, JIT and AOT; program start, panics, stack depth, roots and collections, host calls, tasks, generators, the driver |
| [MIR interpreter](mir-interp.md) | `--mir`, `--engine mir`: raw MIR as the reference output; the native ABI with frames of its own; comptime, debugger and parking on it; the engine plan |
| [Par lock hints](par-lock-hints.md) | F3 (COI-369): userland locks + E0805 call-bag escape hints (no compiler locks) |
| [Shared-heap sendability](shared-heap-sendability.md) | COI-363 C0 whitelist; C1 loop steal (E6); C2 expression IPA without PortableValue copy (E7) |
| [Shared-heap guard](shared-heap-guard.md) | #1892: debug / gc-stress check that a shared-heap job writes only objects allocated in its epoch (header epoch stamp, per-thread job scope); mutation-path inventory; what release relies on |
| [Contracts](contracts.md) | `requires` / `ensures`: syntax, typing, effect-free clauses, HIR checks, caller blame (`contract_fail`, HostInvoke **159**), `--contracts` levels, generated tests, `coil verify` (SMT) |
| [Tasks](tasks.md) | `task::scope` / `spawn` / `join` / `sleep` / `yield_now` / `cancel` / `timeout` / `shield` / `channel` / `blocking` on one runtime: scheduler, suspension points, child panics, cancellation, the `defer` unwinder, waits on other threads; HostInvoke **144–158** |
| [IO reactor](io-reactor.md) | Sync adapter waits + `wait_readable` / `wait_writable` (task-aware) / CPU help-steal; HostInvoke **119**/`stream_attach`, **120**/`stream_park`; clocks **121–123**; M1 math **125–135** (archive minor 5: `atan`…`tanh`). `PI`/`E`/`TAU` → coil-stdlib `num` |
| [Function keys](symbol-keys.md) | `symbol::fn_key()` (compile-time per-function, per-instantiation `int` key: what it hashes, inlining, generic clones, DCE, caches) and `symbol::next_id()` (process-wide counter, HostInvoke **179**) |
| [Collections split](collections-runtime-split.md) | Userland collections vs runtime primitives |
| [Key index](key-index.md) | `key_index::*` natives (HostInvoke 191–199): insertion-ordered hash index over `Vec`s for any `Hash + Eq` key; `find` / `eq_mode` for `int` and `string` keys |
| [Debug info](debug-info.md) | `source_files` / `debug_locs` / `debug_lines` / `fn_symbols` in `.ccoil`; `-g` levels; panic output |
| [Dissect](dissect.md) | `coil dissect`: in-memory MIR / HIR / AST dump |
| [Array pins](array-pin.md) | Array-handle pins: nothing of them survives at run time |
| [Debugger](debugger.md) | `coil debug` — GDB-style REPL / batch debugger |
| [Test runner](test-runner.md) | `coil test` — `coil-test` helper: discovery, `compile_fail/`, seeded order, reactor jobs (`-j`), line coverage (`--coverage`, lcov); `coil mutate` mutation testing |
| [Formatter](fmt.md) | `coil fmt` — AST pretty-printer for `.coil` |
| [LSP](lsp.md) | `coil lsp` — language server |
| [Test health report](test-health-report.md) | Historical flaky/broken-test notes |
| [String table migration](string-table-migration.md) | Completed migration note (retired `print` keyword) |
| [Grammar](grammar/) | tree-sitter grammar sources (`tree-sitter-coil`; `queries/highlights.scm`) |

## Crate map

| Crate | Role |
|-------|------|
| `parser` | Pratt parser and AST |
| `compiler` | HM typechecker, HIR (optimizations, layout plan), MIR (HIR → SSA, passes, archive format), pipeline |
| `machine` | Runtime: driver, heap/GC, tasks, reactor, FFI (libffi), host natives, the native ABI |
| `coil-native` | MIR to Cranelift, JIT and AOT objects |
| `coil-mir-interp` | The MIR interpreter |
| `coil-rt` | Runtime library an AOT executable links (`libcoil_rt.a`) |
| `common` | Values, archive format, host native table, startup ops, debug tables |
| `coil-simd` | Stable SIMD helpers (`std::arch`) for numeric / byte kernels |
| `coil-cli` | Shared CLI helpers: archive load, execute, git-style dispatch (`run_linked_package`, `try_run_embedded_with` for packaged apps) |
| `coil-args` | Shared clap flag groups (host grants, module roots, opt level) for `coil` and its helper binaries |
| `reporting` | Diagnostics rendering (ariadne) |
| `coil-embed` | JIT runner for packaged apps (`coil package --no-aot`): the runtime and Cranelift, calling `coil_cli::try_run_embedded_with`. Not an embedding library. |

`coil package` links an AOT executable by default, with the archive in its object; `--emit obj|lib` stops before that link; `--no-aot` appends the archive to the `coil-embed` runner. There is no supported embedding library API yet (that needs host-catalog + archive C-layout work, later).

Contributor invariants: [AGENTS.md](../../AGENTS.md).
