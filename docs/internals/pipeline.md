# Compilation and execution pipeline

How a `.hy` program becomes running bytecode on the VM.

## Stages

1. **Parse** — the Pratt parser in `parser/` reads `.hy` source into an AST.
   Derives (built-in ones included) and attribute macros run in the VM right after
   discovery and splice their output before typechecking ([Macros](macros.md));
   `compiler/src/attrs.rs` records them and checks built-in attributes.
2. **Typecheck** — constraint-based inference in `compiler/src/typechecking/`. Generics are explicit (`fn f<T>`, `class Cell<T>`); `let` bindings are monomorphic (not Algorithm W). Diagnostics are source-anchored via ariadne. After infer, [`purity`](../../compiler/src/typechecking/purity.rs) records per-`DefId` [`EffectFlags`](../../compiler/src/typechecking/purity.rs) (host/FFI, escaping heap mutation, yield, `thread::spawn`, `gc::collect` / finalizers, I/O, attach/park; unknown is impure). Prelude math / packed LA / `simd_axpy_reduce` are empty (pure); clocks / IO / GC / FFI stay impure. LICM / fuse-select / index facts read those names from the sidecar — a proven-pure `CALL` is not a length or hoist barrier; an impure `CALL` is. MIR LICM uses the same bits on HostInvoke edges (I6): impure hosts never hoist; W4 dense emit stays the closed math/packed/axpy allowlist. [`index_facts`](../../compiler/src/typechecking/index_facts.rs) records `0 <= i < arr.len()` and length-stable helper-parameter facts on the same sidecar; a function's prologue emits `ArrayPin` for those parameters ([array-pin.md](array-pin.md)). Each type has one value representation (pointer niches for heap `Option` / heap-heap `Result`, a two-slot `[payload, tag]` convention on direct `CALL`/`RETURN` for immediate pairs, boxed `ObjEnum` otherwise), decided by [`layout::rep`](../../compiler/src/layout/rep.rs) for every consumer: [layout.md](layout.md). Checker and codegen key per-node facts by the pre-walk's exact `NodeId` (address + span), not the drifting pre-order counter. HIR marks proven indexes in-bounds from those facts and MIR emits them as `IndexUnchecked`; `hir::bounds` also proves counted loops after inlining. Yield clears facts. Ground generic helpers (bounded or unbounded) monomorphize to a direct `CALL` of a cloned body; escaped `let f = id` keeps `MakePolyFn` / `CallIndirect` / `BoxValue`.
3. **HIR** — `hir::build` turns the AST plus checker facts into HIR (`compiler/src/hir/`, a typed, desugared tree; `coil dissect --hir`). The typed inliner and the HIR passes (`hir::unroll`, `hir::fold`, `hir::bounds`, `hir::sink_return`, `hir::enum_sroa` / `hir::tuple_sroa` / `hir::readonly_sroa`) run there. `codegen/emit_hir.rs` then **plans** each body: it decides value layouts (pair locals, boxed niches, records, stack arrays), dictionaries and call targets, and records them as tables for MIR lowering. It no longer emits instructions; each body's stack IL is a placeholder that only binds its labels. A body HIR cannot lower is an internal error (`OptimizeOptions::require_mir`); there is no AST codegen and no stack-lowering fallback.
4. **MIR** — `mir::lower_from_hir` (`mir/from_hir.rs`) lowers every body from HIR plus those tables to typed SSA ([mir.md](mir.md)). Values cross function boundaries in their [layout.md](layout.md) representation; a two-slot pair moves two words on a direct `CALL`/`RETURN` (`CALL` bit 31 / `RETURN` operand `2`). Non-escaping enum, tuple and class locals are scalar-replaced into slots.
5. **Tiers and lower** — after multi-file link, `finalize_bytecode` **tree-shakes** unused function bodies (it follows MIR call targets from `main`, and `test_cases` when `--include-tests`), then `IlModule::optimize_and_flatten` picks each body's tier, leaf first:
   - **dense**: MIR passes (CSE/GVN, LICM, InstCombine, DestProp, IV strength reduction, SIMD packs) and dense 3-address opcodes (`DenseBin` … `DenseCast`), kept only when its cost beats the LIR form and every callee it `CALL`s is dense;
   - **LIR**: MIR SROA/CSE, then `mir::emit_lir` writes VM stack ops (`IlOp`) from the SSA. This is what most bodies run.

   A `defer` body (pinned) takes LIR with its cleanup pad. The bodies are concatenated and lowered once by `il::lower`: fuse-select with **label / abs-jump barriers** (plus `*Return` join-on-window[0]), assign PCs once, encode `Vec<Byte>`. No optimization pass rewrites the stack ops; the IL is only the encoding step. Enabled fuse patterns: const fold, `BinSlotImm` / `BinSlotSlot` (including const-left commute `CONST; LOAD; int-bin`, COI-384), `CmpJmpf` / `LogNotJmpf` / `BinSlotImmJmpf` / `BinSlotSlotJmpf`, `BinSlotImmStore` / `BinSlotSlotStore` (the latter takes float ops as well as int — archive minor 2), packed multi-slot `LOAD`/`STORE` (`n≤3`), `LoadReturnSlot` / `ConstReturnImm` / `BinReturn`. CALL/TailCall/MakeCoro/CodePtr use `IlOp::Entry`; production abs JMP is rejected in lower. Inlining happens only in the HIR. GC frame maps for dense and LIR bodies are built from their MIR ([mir-stack-maps.md](mir-stack-maps.md)). Compiler-made glue (instance-clone and dictionary-adapter thunks, stream `Read`/`Write` shims, static and FFI initializers) is still written as stack IL directly in `codegen/compiler.rs`.
6. **Archive** — `coil compile` wraps bytecode in a versioned `ArchivedProgram` envelope (packed `major.minor` in `common/src/archive.rs`) and writes `out.hyc` (or `-o`). Default `coil file.hy` compiles and runs **in memory** and does not read or write `out.hyc`. See [Debug line table](debug-info.md). **Policy:** `.hyc` does not record grants (no rkyv major for grants). Host grants come from CLI flags / `Pipeline` `HostGrants` at **compile**. Ungranted host calls **fail the compile** ([Capabilities](#capabilities)); granted programs still emit the real `HostInvoke` / `FfiLoad`. **The artifact is the grant:** `coil run out.hyc` and packaged `coil-embed` do **not** re-apply allow flags; if the bytecode has the op, it runs. `dload` still checks native sha256 (`--dload-pin`) / `--dload-trusted` integrity and treats `--ffi-search-path` dirs as locators. `dload("c")` stays denied even if flagged. Non-const `dload` paths are a compile error.
7. **Execute** — the VM in `machine/` runs `main` (or each `test("…")` case under `coil test`), from the in-memory compile or from a loaded `.hyc`.

```
AST + types → HIR (inline, HIR passes) → layout plan → MIR (SSA) → dense or LIR stack ops → fuse-select / PCs → Vec<Byte> → .hyc → VM
```

Where each former stack-IL pass went, and the fuse-select contract: [`compiler/src/il/opt/README.md`](../../compiler/src/il/opt/README.md).

## IL intent

The stack IL (`compiler/src/il/`) is the **encoding step**, not a semantic IR and not where code is generated:

1. **Produce** — `mir::emit_lir` and the dense emitter write VM-shaped [`IlOp`](../../compiler/src/il/op.rs)s with **symbolic labels** (`Jump` / `Label` / `Entry`), not absolute PCs. Only compiler-made glue (see stage 5) is written as stack IL by hand.
2. **Lower** — one `il::lower`: **fuse-select**, **assign PCs once**, encode `Vec<Byte>`. Production rejects residual absolute JMP; there is no post-lower peephole / `adjust_target` hot path.

Names, types, and call meaning live in **DefIds**, the **typed sidecar** and HIR. Optimizations live in HIR and MIR ([mir.md](mir.md)); `il/opt/` only holds `OptimizeOptions`, opt levels, label helpers and statistics ([`il/opt/README.md`](../../compiler/src/il/opt/README.md)). Classes and heap values use the VM `Value` word; `FORMAT` / `STRING` / `STRINGIFY` / `PRINT` run on LIR (dense refuses table ops). Host Option / Result values pack once at `HostInvoke` (`host_enum`).

## Optimization levels

`Pipeline::set_opt_level` / CLI `-O` / `--opt-level` select an [`OptLevel`](../../compiler/src/il/opt/opt_level.rs) preset that fills `OptimizeOptions` (and typed-inlining budgets). Default is **Standard**. The switches gate HIR passes, HIR lowering and the MIR tiers.

| Level | CLI | HIR passes | MIR passes |
|-------|-----|------------|------------|
| None | `-O0`, `none` | constant folding only; no inlining | none |
| Basic | `-O1`, `basic` | None + small-budget inlining (same module only) | folds: SCCP, InstCombine, DestProp (`mir_fold`) |
| Standard | `-O2`, `standard` (default) | fold, enum / tuple / readonly class scalar replacement, bounds proofs, unroll, `sink_return`, early-exit layout; cross-module inlining | folds + CSE, LICM, IV strength reduction, GVN/PRE, saxpy-reduce pack and vectorizer (`mir_loop`) |
| Aggressive | `-O3`, `aggressive` | Standard passes with a larger inline budget. (`seek_back_edge` was removed: its residual `Seek` blocked MIR dense and made `-O3` 1.3–2.5× slower than `-O2` on loop benches.) | as Standard |
| Size | `-Os`, `size` | Standard without loop unroll or `sink_return`, smaller inline budget | as Standard |
| Debug | `-Og`, `debug` | constant folding only (no scalar replacement, unroll or inlining) | none |

MIR passes run in `mir::specialize` ([mir.md](mir.md#passes-by-level)). At every level a body still lowers to MIR and takes dense or LIR by the same gates; what emission needs runs regardless of level: SROA's reuse of a stored array, deopt maps, `tidy` after a fold, the dense cost gate.

`None ⊂ Basic ⊂ Standard ⊂ Aggressive` on enable flags. Size and Debug are independent axes.

Branch layout (COI-128) is **on** at Standard, Aggressive and Size (`branch_optimization`) and happens in HIR lowering: an `if` arm or two-arm niche `match` arm that always returns is moved after the function's fall-through return (`CodeBuf::move_exits_to_end`), and the jump that skipped it inverts, so the code after the exit falls through. Layout is heuristic only (a returning arm is cold). **PGO was removed** ([#301](https://github.com/ardax-corp/coil-lang/pull/301)): no `--pgo-*` flags, ingest, instrumentation, heat knobs, or `BranchProfile`. Do not revive that plumbing.

Basic-block reordering (COI-129) and jump threading also happen in HIR lowering: the cold-exit layout above covers the `else` arm and a pair `match`'s returning last arm, and an `if` / `else` ending a loop body jumps straight to the back edge. The stack-IL `block_reordering` and `jump_thread` passes were removed 2026-10.

### Environment knobs

A few switches no level sets are on by default and turned off by an
environment variable, for bisecting a miscompile or measuring one pass.
Each is an `OptimizeOptions` field: `OptLevel::options` reads the
variable once when it builds the options (`env_knobs` in
[`opt_level.rs`](../../compiler/src/il/opt/opt_level.rs)), the passes read
the field, and `Compiler::set_opt_level` keeps the field's value across a
level change. `0` / `false` / `off` / `no` turn a switch off; anything
else, or unset, leaves it on.

| Variable | Field | Off means |
|----------|-------|-----------|
| `COIL_HIR_INLINE` | `hir_inline` | no typed inlining (`Pipeline::set_hir_inline` overrides) |
| `COIL_HIR_INLINE_PAIR` | `hir_inline_pair` | callees returning a two-word `Option` / `Result` / enum stay calls |
| `COIL_HIR_INLINE_HEAP` | `hir_inline_heap` | callees with heap locals stay calls |
| `COIL_HIR_INLINE_LOOPS` | `hir_inline_loops` | callees with loops stay calls |
| `COIL_HIR_SPECIALIZE` | `hir_inline_specialize` | a too-big callee is not retried with its constant arguments bound |
| `COIL_HIR_INLINE_MONO` | `hir_inline_mono` | mono clone callees stay calls |
| `COIL_HIR_MATCH_TREE` | `hir_match_tree` | nested matches test each row in order, not by outer tag |
| `COIL_HIR_PAIR_LOCALS` | `hir_pair_locals` | an enum local built in place is boxed, not two slots |
| `COIL_MIR_SCCP` | `mir_sccp` | no MIR SCCP (the other `mir_fold` passes still run) |
| `COIL_AUTO_PAR` | `auto_par` | no automatic fork-join (`Pipeline::set_auto_par(false)` too) |
| `COIL_HIR_BISECT=N` | `hir_bisect` | (a number, unset by default) only the first `N` bodies HIR lowering admits in the process lower; the `N`th is named on stderr |

`COIL_CALL_FOLD=0` turns compile-time call folding off through
`Pipeline::call_fold` instead (see `call_fold`). `COIL_HIR_WHY`,
`COIL_HIR_MIR_WHY` and `COIL_HIR_INLINE_WHY` only print why a body or
call was refused; they change no code.

Iterative optimization (COI-130) was removed: re-running the pipeline miscompiled loop tests (LICM is not idempotent) and no opt level enabled it. The pipeline runs once.

Optimization statistics (COI-131) are **off** by default (`collect_stats` / CLI `--opt-stats` and `--opt-stats-json`). When enabled, it records each body's final tier (dense / LIR, with the dense refusal reasons) and HIR lowering and inlining counters (typed-inlining call sites increment `functions_inlined`). Human text or a single JSON object is printed to stderr after a source compile. Collection clones the op buffer only while the flag is on.

The IL is **compile-time only**. The archive version is recorded only in `common/src/archive.rs` ([older minors](archive-history.md)). If an optimization is sound but flagship `.hyc` do not change, prove it on focused `examples/perf` hit benches ([roadmap](optimization-roadmap.md#hit-bench-prove-rule)).

## Capabilities

Every `HOST_NATIVES` row (`common/src/host.rs`) states the capabilities it
needs (`common::Caps`: read, write, net, env, exec, exit, attach). `open` is
refined by a literal mode (`"r"` → read; `w` / `a` / `x` → write; `+` or a
non-literal mode → both, `common::open_mode_caps`).

HIR lowering tags each gated `HostInvoke` with the call's
`DebugLoc` and records it in `Compiler.gated_host_calls`. Before
`finalize_bytecode`, `Compiler::capability_violations`
(`compiler/src/codegen/capabilities.rs`) walks the IL call graph
(`il::reachable_functions`, shared with the tree-shaker) from `main`, the
test cases, code outside function bodies and static / FFI initializers, and
reports each reached call whose caps are not in `HostGrants::caps()`, with
the chain that reaches it:

```
error[E0414]: `io::open` requires `--allow-read`: reached from `main` → `load`
```

Exec / exit / attach keep their codes (`E0406`–`E0408`); read / write / net /
env are `E0414`. Only what the program reaches counts, so importing a module
with an unused `env::exec` helper needs no grant. A build with no `main` and
no tests (a library) reports nothing. Two gates stay at the call site in
typecheck, because they have no IL call to tag: the `stream.attach(…)` method
form and FFI (`dload`, process-exec symbols). Macro sub-programs are granted
everything: the macro VM refuses host calls at run time (`E0119`).

Flags: `--allow-read`, `--allow-write`, `--allow-net`, `--allow-env`,
`--allow-exec`, `--allow-exit`, `--allow-attach`, `--allow-ffi-exec`, and
`-A` / `--allow-all` for all of them (not `dload`). `Pipeline::grant_capability`
/ `grant_all` are the library form.

`dload` integrity is flags too: `--dload-pin STEM=SHA256` (the library's
hash) and `--dload-trusted STEM` (no hash check); `coil package` / `coil
natives dump` take `--ffi-native name=…,version=…,path=…` per native
library. A packaged app looks for each library in the
natives cache, beside the executable and in `lib/` there; getting it there is
the user's job.

## Archives (`out.hyc`)

Default `coil file.hy` compiles and runs in memory. It does **not** read or write `out.hyc` (integration tests assert this). An existing `out.hyc` beside the entry is left untouched; if it looks stale the CLI may warn, but the in-memory compile still runs.

`coil compile [file.hy]` writes `out.hyc` (or `-o`). `coil run out.hyc` loads that archive and **warns** when recorded sources are newer; it does **not** rebuild. Missing / corrupt / incompatible archives fail instead of compiling from source.

Incrementality (fingerprint-based reuse of a compile) is a later design, not current CLI behavior.

## Multi-file programs

The pipeline discovers dependencies via `use` / `mod` under the `--root` search roots, compiles each file with a namespace prefix into one shared IL buffer, and **lowers once** after linking. The **entry file** uses the empty namespace. See [Modules](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/references/modules.md) and [Project config](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/references/project-config.md).

## Opcode discipline

New `Instruction` variants are **append-only** (preserve `#[repr(u8)]` discriminants). Additive changes bump the archive **minor**; incompatible changes bump the **major** (reset minor). Loaders accept the same major when the archive minor is ≤ the runtime minor. The release VM `promise!` ceiling must track the last variant. Selected builtin-related opcodes are listed in [opcodes.md](opcodes.md).
