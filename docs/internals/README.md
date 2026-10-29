# Internals

How coil is structured for contributors. End-user language docs live in [coil-website](https://github.com/ardax-corp/coil-website) (`src/content/docs/`; site routes `/docs/<path>` until a public domain is set).

| Document | Contents |
|----------|----------|
| [Pipeline](pipeline.md) | Parse → typecheck → HIR (inline, optimize) → MIR for every body → dense or LIR bytecode → archive → execute |
| [Archive history](archive-history.md) | Older archive minors (5–37); `common/src/archive.rs` is the ledger for the current version |
| [Dictionary passing](dictionary-passing.md) | Calling convention for trait-bounded generics that are not monomorphized |
| [Type unification](type-unification.md) | Unification rules of the checker, by type shape |
| [Comptime](comptime.md) | Code that runs while compiling: consts, `comptime` expressions and blocks, macros, folding; stages, limits, IO grants, result cache, tooling |
| [WebAssembly](wasm.md) | `coil` for wasm32-wasip1 (the website playground): 64-bit `Value` words, no FFI / sockets / OS threads, threads run when joined, `// @needs` test header, running the corpora under Node |
| [Compiler peak memory](compile-memory.md) | Where `coil compile`'s own peak RSS comes from per phase, how it was measured, what was cut (2026-10-27: spool 93 → 65 MiB) and what is left |
| [Build cache](build-cache.md) | #1881: `coil FILE` / `coil compile` reuse the archive of an unchanged program from `~/.cache/coil/build`; key, inputs, what is not cached, `--no-build-cache` |
| [Macros](macros.md) | User `derive` / attribute macros: `macro` model, `quote`, compile-time VM stage, hygiene, sandbox |
| [Numeric MIR](mir.md) | MIR: every body lowers from HIR to SSA; dense and LIR tiers; pass history (P1–P12, I1–I8) |
| [MIR language islands](mir-islands.md) | Historical: COI-292 I0–I8 ladder that grew MIR coverage before every body went through MIR (M4/M5, 2026-10) |
| [Int overflow](int-overflow.md) | Int `+ - * / % **` and negation give the exact result or panic; where the VM, folds, optimizations, auto-par, SIMD and packed kernels keep that |
| [Escape facts and early freeing](escape-memory.md) | `feature/escape-memory`: escape summaries across calls, freeing at scope end, static uniqueness, nursery; the GC stays as backstop |
| [Language quirks](language-quirks.md) | Locked Q1–Q9 (2026-09-10): `[T; N]` box-once, class field-SROA, grow type error, defined `i % N`, panic vs raise, roadmap Q6–Q9 |
| [`if` expression](if-expression.md) | #1877: `if … else …` / `if let … else …` as a value; primary precedence like `match`; E0137 for a missing `else`; reuses the `If` / `Match` HIR |
| [Q6 iterator protocol](q6-iterator-protocol.md) | Counted desugar for `for` (array / literal range / B5 locals / C2 param+returned Range / C2b heap-field + user `Iterator::next` + dict + coro) |
| [Q9 format / string](q9-format-string.md) | I4 reopen: SSA + LIR reconstruct of `STRING` / `PRINT` / `FORMAT` / `STRINGIFY`; R2 bytes HostInvoke; R3 maps; R4 unicode leftover |
| [Static trait methods](static-trait-methods.md) | #524: `static fn` in traits, `Owner::m` / `T::m` calls, return-type-directed bound resolution, per-field `#[derive(Default)]` |
| [Generic instances](generic-instances.md) | #550 / #551 / #552: `impl Trait for Box<T>` and `Box<T: Bound>`, instance contexts, context dictionaries, open-goal dictionary resolution, derives on generic types |
| [Per-type clones](generic-specialization.md) | #1417 / #1419: ground dictionaries built once into statics; per-type clones of other modules' generic methods and free functions (`ModuleCtx`); allocation numbers |
| [Userland text search](userland-text-search.md) | `split` / `find_all` / `replace_all` in Coil at near-native speed: per-step profile and measurements (feature/userland-loop) |
| [Const tables](const-tables.md) | #1561: `const T = readonly [...]` lookup tables: a local one built once on first use, constant-index reads folded, variable-index reads invariant in LICM / CSE; shared-heap workers read the spawner's statics; measurements |
| [Opt generalization](opt-generalization.md) | Historical: COI-333 A0: MIR default + one object story + dense-native + cost gate; A4 measurement; **B0** audit + **B1** Q6–Q8 entry hygiene |
| [Denser MIR leftovers](mir-dense-leftovers.md) | Historical: post A0–C3 kick list (D0 `nsieve` keep; D1 class maps; D2 dense field; D3 boxed multi-payload match; Q9 `main` fuse) |
| [MIR deopt / debugger](mir-deopt.md) | COI-299 I7: stop/deopt edges; VM debugger stays source of truth |
| [Specialize refuse](specialize-refuse.md) | Historical: hard walls vs Q6–Q9 ladders / cost gate; B1–B9; C1 self two-slot CALL; **S8** fusion not gated on specialize keep |
| [IL lowering](../../compiler/src/il/opt/README.md) | No IL optimization passes remain (removed 2026-10): where each former pass went, `sp` vs `tell`, fuse-select contract |
| [Limitations](limitations.md) | Known gaps, workarounds, and tracking |
| [Value representation](layout.md) | `rep(T)` per static type: niches, the two-slot call convention, shared generic bodies, the representation matrix |
| [Optimization roadmap](optimization-roadmap.md) | AOT/JIT plan; landed opts (#304–#318); hit-bench prove rule; PGO removed (#301). Extra benches: `gc_churn`, Option/Result churn, `iv_mul_sr` / `licm_nested_chains` / `tail_sibling` / `cse_*` / `dest_prop_field_alias` |
| [VM dispatch alignment](dispatch-alignment.md) | Why `coil` and `coil-embed` ran the same code at different speeds; `-align-loops=64` (not enabled); PGO + BOLT tried |
| [Threaded execute dispatch](threaded-dispatch.md) | COI-373–376 G0–G3 A/B record (table / hotmatch removed); G3 hot/cold I-cache split |
| [Packed byte arrays](packed-bytes.md) | #927: `Vec<byte>` / `Vec<bool>` one byte per element; lossless `Bytes` storage, `WORD_BYTE` construction stamp, byte copies for I/O / FFI / threads; measurements |
| [Heap identity](heap-identity.md) | Mapped slab + header poison for `find_object_by_addr` (COI-200) |
| [Incremental GC](gc-incremental.md) | COI-309 S4: safepoint mark (no write barrier) + lazy sweep; moving GC deferred |
| [GC evacuation](gc-evacuation.md) | Mostly-copying evacuation of sparse chunks (always on; `COIL_GC_COMPACT=0` off in unit tests); precise refs rewritten, ambiguous ones pin |
| [SIMD](simd.md) | `coil-simd` — packed LA + V0/V1 `V*` opcode backend |
| [Auto-par](auto-par.md) | Purity + parameterized AlwaysPar IPA (COI-366 F1 hop/grain); F2 broader admission (COI-368); loop IPA (COI-362 E4); C1/C2 shared-heap spawn (E6/E7); [branch-miss notes](auto-par-branch-misses.md) |
| [Par lock hints](par-lock-hints.md) | F3 (COI-369): userland locks + E0805 call-bag escape hints (no compiler locks) |
| [Shared-heap sendability](shared-heap-sendability.md) | COI-363 C0 whitelist; C1 loop steal (E6); C2 expression IPA without PortableValue copy (E7) |
| [Shared-heap guard](shared-heap-guard.md) | #1892: debug / gc-stress check that a shared-heap job writes only objects allocated in its epoch (header epoch stamp, per-thread job scope); mutation-path inventory; what release relies on |
| [Contracts](contracts.md) | `requires` / `ensures`: syntax, typing, effect-free clauses, HIR checks, caller blame (`contract_fail`, HostInvoke **159**), `--contracts` levels, generated tests, `coil verify` (SMT) |
| [Tasks](tasks.md) | `task::scope` / `spawn` / `join` / `sleep` / `yield_now` / `cancel` / `timeout` / `shield` / `channel` / `blocking` on one VM: scheduler, suspension points, child panics, cancellation, the `defer` unwinder, waits on other threads; HostInvoke **144–158** |
| [IO reactor](io-reactor.md) | Sync adapter waits + `wait_readable` / `wait_writable` (task-aware) / CPU help-steal; HostInvoke **119**/`stream_attach`, **120**/`stream_park`; clocks **121–123**; M1 math **125–135** (archive minor 5: `atan`…`tanh`). `PI`/`E`/`TAU` → coil-stdlib `num` |
| [Function keys](symbol-keys.md) | `symbol::fn_key()` (compile-time per-function, per-instantiation `int` key: what it hashes, inlining, generic clones, DCE, caches) and `symbol::next_id()` (process-wide counter, HostInvoke **179**) |
| [Stack bounds](stack-bounds.md) | Operand stack sizing, run-time growth, and the optional `#[max_depth]` |
| [Collections VM split](collections-vm-split.md) | Userland collections vs VM primitives |
| [Debug line table](debug-info.md) | `source_files` / `debug_locs` in `.hyc` |
| [Superinstruction candidates](superinstructions-candidates.md) | Historical: COI-378 S0: fused-op inventory, S1–S8 refine notes, extras that do not need specialize-keep |
| [Opcodes](opcodes.md) | Selected bytecode ops behind builtins |
| [Dissect](dissect.md) | `coil dissect` — in-memory bytecode / IL / AST dump |
| [Array pins](array-pin.md) | Array-handle pins: only the prologue `ArrayPin` is still emitted; `IndexPin*` / `StoreIndexPin*` are retired |
| [Debugger](debugger.md) | `coil debug` — GDB-style REPL / batch debugger |
| [Test runner](test-runner.md) | `coil test` — `coil-test` helper: discovery, `compile_fail/`, seeded order, reactor jobs (`-j`), line coverage (`--coverage`, lcov); `coil mutate` mutation testing |
| [Formatter](fmt.md) | `coil fmt` — AST pretty-printer for `.hy` |
| [LSP](lsp.md) | `coil lsp` — language server |
| [Test health report](test-health-report.md) | Historical flaky/broken-test notes |
| [String table migration](string-table-migration.md) | Completed migration note (retired `print` keyword) |
| [Grammar](grammar/) | tree-sitter grammar sources (`tree-sitter-coil`; `queries/highlights.scm`) |

## Crate map

| Crate | Role |
|-------|------|
| `parser` | Pratt parser and AST |
| `compiler` | HM typechecker, HIR (optimizations, layout plan), MIR (HIR → SSA, dense and LIR emit), stack-op encoding, pipeline |
| `machine` | VM, heap/GC, FFI (libffi), host natives |
| `common` | Opcodes, values, archive format |
| `coil-simd` | Stable SIMD helpers (`std::arch`) for numeric / byte kernels |
| `coil-cli` | Shared CLI helpers: archive load, VM execute, git-style dispatch (`try_run_embedded` for packaged apps) |
| `coil-args` | Shared clap flag groups (host grants, module roots, opt level) for `coil` and its helper binaries |
| `reporting` | Diagnostics rendering (ariadne) |
| `coil-embed` | Packaged-app runner: a small bin that calls `coil_cli::try_run_embedded`. **Not** an embed-the-VM library. |

`coil package` concatenates a `.hyc` onto a runner (`coil-embed` when present, otherwise the full `coil` binary). There is no supported embed-the-VM library API yet (that needs host-catalog + archive C-layout work, later).

Contributor invariants: [AGENTS.md](../../AGENTS.md).
