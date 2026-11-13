# Compilation and execution pipeline

How a `.coil` program becomes running code.

## Stages

1. **Parse** — the Pratt parser in `parser/` reads `.coil` source into an AST.
   Derives (built-in ones included) and attribute macros run on the MIR interpreter
   right after discovery and splice their output before typechecking ([Macros](macros.md));
   `compiler/src/attrs.rs` records them and checks built-in attributes.
2. **Typecheck** — constraint-based inference in `compiler/src/typechecking/`. Generics are explicit (`fn f<T>`, `class Cell<T>`); `let` bindings are monomorphic (not Algorithm W). Diagnostics are source-anchored via ariadne. After infer, [`purity`](../../compiler/src/typechecking/purity.rs) walks the AST for each function's [`EffectFlags`](../../common/src/effects.rs) (host/FFI, escaping heap mutation, yield, `thread::spawn`, `gc::collect` / finalizers, I/O, attach/park; unknown is impure) and keeps the functions that cannot change an array's length (`LengthStability`). Only index facts read that set: a length-stable call keeps a bounds proof; any other call ends it. Everything after the checker (purity for LICM and CSE, auto-par, `uses {…}`, copy-on-write, escape) reads the HIR effect summaries (`hir::effects`). Prelude math / packed LA / `simd_axpy_reduce` are empty (pure); clocks / IO / GC / FFI stay impure. MIR LICM uses the same bits on `HostInvoke` edges: impure hosts never hoist. [`index_facts`](../../compiler/src/typechecking/index_facts.rs) records `0 <= i < arr.len()` and length-stable helper-parameter facts on the same sidecar. Each type has one value representation (pointer niches for heap `Option` / heap-heap `Result`, a two-slot `[payload, tag]` convention on direct calls and returns for immediate pairs, boxed `ObjEnum` otherwise), decided by [`layout::rep`](../../compiler/src/layout/rep.rs) for every consumer: [layout.md](layout.md). Checker and codegen key per-node facts by the pre-walk's exact `NodeId` (address + span), not the drifting pre-order counter. HIR marks proven indexes in-bounds from those facts and MIR takes them unchecked; `hir::bounds` also proves counted loops after inlining. Yield clears facts. Ground generic helpers (bounded or unbounded) monomorphize to a direct call of a cloned body; an escaped `let f = id` stays a polymorphic function value, called through `CallIndirect`, its words unboxed.
3. **HIR** — `hir::build` turns the AST plus checker facts into HIR (`compiler/src/hir/`, a typed, desugared tree; `coil dissect --hir`). The typed inliner (a call to a mono or method clone, a generic class's methods included, splices the generic body at the clone's types; a ground `hash()` of a string plans as its `hash_string` `HostInvoke`, not the `Hash__string__hash` thunk) and the HIR passes (`hir::unroll`, `hir::fold`, `hir::bounds`, `hir::sink_return`, `hir::enum_sroa` / `hir::tuple_sroa` / `hir::readonly_sroa`) run there. `codegen/emit_hir.rs` **plans** each body: it decides value layouts (pair locals, boxed niches, records, stack arrays), dictionaries and call targets, which MIR lowering asks for as it reaches each node (`mir::LowerCx`). A body is planned as built, then once more as the passes left it (`Compiler::hir_rewrite`); between passes only its calls are resolved again (`hir_resolve`), for the next inlining round and for early frees. A rewritten body that does not plan is made again with each inlining round planned, then with no inlining, then with no early frees, each step counted (`--opt-stats`, `hir inline: refused`). A body HIR cannot lower is an internal error (`finalize_bodies` panics on a clean compile with one).
4. **MIR** — `mir::lower_from_hir` (`mir/from_hir.rs`) lowers every body from HIR and that plan to typed SSA ([mir.md](mir.md)). Values cross function boundaries in their [layout.md](layout.md) representation; a two-slot pair moves two words on a direct call and return. Non-escaping enum, tuple and class locals are scalar-replaced into slots.
5. **Finalize** — after multi-file link, `finalize_bodies` **tree-shakes** unused function bodies (`reachable_bodies` follows MIR call targets from `main`, startup bodies and `test_cases` with `--include-tests`), collects the locations the bodies use, and hands the bodies to an open capture (`capture_native_bodies`), which runs the MIR passes on each ([mir.md](mir.md#passes-by-level)). Each body gets its identity, `label.0 + 1` (`0` names nothing), and the startup list is built (`common::StartupOp`, [native.md](native.md#program-start)). `Pipeline::compute_consts` then evaluates `const`s and folds calls on the interpreter: `Compiler::fold_sites` lists pure one-word calls with constant arguments in the captured MIR, and `apply_call_folds` replaces each with its value. That rewrites the MIR, so it changes what runs. `--fold-calls off` turns it off, and flagship benches need that, or `main` becomes a constant.
6. **Archive** — `coil compile` writes a versioned `ArchivedProgram` (packed `major.minor` in `common/src/archive.rs`), MIR and data only, to `out.ccoil` (or `-o`). Default `coil file.coil` compiles and runs **in memory** and does not read or write `out.ccoil`. See [Debug line table](debug-info.md). **Policy:** `.ccoil` does not record grants. Host grants come from CLI flags / `Pipeline` `HostGrants` at **compile**. Ungranted host calls **fail the compile** ([Capabilities](#capabilities)); granted programs keep the real host and FFI ops. **The artifact is the grant:** `coil run out.ccoil` and packaged executables do **not** re-apply allow flags; if the program has the op, it runs. `dload` still checks native sha256 (`--dload-pin`) / `--dload-trusted` integrity and treats `--ffi-search-path` dirs as locators. `dload("c")` stays denied even if flagged. Non-const `dload` paths are a compile error.
7. **Execute** — one of two engines runs the MIR, under `machine::driver::Driver` over a `Runtime`:
   - **native code**: Cranelift, JIT at start by default, AOT with `coil package` ([native.md](native.md));
   - **the MIR interpreter** (`coil-mir-interp`, [mir-interp.md](mir-interp.md)): compile-time code, macros, `coil debug`, wasm, `--mir`, `--engine mir`, `--cow-report` and `coil mutate`.

   The driver runs the startup, then `main` (or each `test("…")` case under `coil test`), from the in-memory compile or from a loaded `.ccoil`.

```
AST + types → HIR (inline, HIR passes) → layout plan → MIR (SSA) → MIR passes → .ccoil → Cranelift or the interpreter
```

Names, types and call meaning live in **DefIds**, the **typed sidecar** and HIR. Optimizations live in HIR and MIR ([mir.md](mir.md)). Host `Option` / `Result` values pack once at `HostInvoke` (`host_enum`).

## Optimization levels

`Pipeline::set_opt_level` / CLI `-O` / `--opt-level` select an [`OptLevel`](../../compiler/src/opt/opt_level.rs) preset that fills `OptimizeOptions` (and typed-inlining budgets). Default is **Standard**. The switches gate HIR passes, HIR lowering and the MIR passes.

| Level | CLI | HIR passes | MIR passes |
|-------|-----|------------|------------|
| None | `-O0`, `none` | constant folding only; no inlining | `sroa`, `phi_sroa` |
| Basic | `-O1`, `basic` | None + small-budget inlining (same module only) | folds: SCCP, InstCombine, DestProp (`mir_fold`), `sroa`, `phi_sroa` |
| Standard | `-O2`, `standard` (default) | fold, enum / tuple / readonly class scalar replacement, bounds proofs, unroll, `sink_return`, early-exit layout; cross-module inlining | folds + CSE, LICM, IV strength reduction, GVN/PRE and the loop rewrites (`mir_loop`) |
| Aggressive | `-O3`, `aggressive` | Standard passes with a larger inline budget | as Standard |
| Size | `-Os`, `size` | Standard without loop unroll or `sink_return`, smaller inline budget | as Standard |
| Debug | `-Og`, `debug` | constant folding only (no scalar replacement, unroll or inlining) | `sroa`, `phi_sroa` |

MIR passes run in `mir::specialize::optimize_mir` ([mir.md](mir.md#passes-by-level)). The interpreter's raw capture at `-O0` skips even `sroa` and `phi_sroa` ([mir-interp.md](mir-interp.md#the-reference-output)).

`None ⊂ Basic ⊂ Standard ⊂ Aggressive` on enable flags. Size and Debug are independent axes.

Branch layout is **on** at Standard, Aggressive and Size (`branch_optimization`) and happens in HIR lowering: early exits are laid out after the body (`HirPlan::cold_ok`). Layout is heuristic only (a returning arm is cold). **There is no PGO** ([#301](https://github.com/ardax-corp/coil-lang/pull/301)): no `--pgo-*` flags, ingest, instrumentation, heat knobs or `BranchProfile`. Do not revive that plumbing. An `if` / `else` ending a loop body jumps straight to the back edge.

### Environment knobs

A few switches no level sets are on at every level
(`level_free_knobs` in [`opt_level.rs`](../../compiler/src/opt/opt_level.rs));
`Compiler::set_opt_level` keeps their values across a level change.

| Switch | Field | Off means |
|--------|-------|-----------|
| `--auto-par on\|off` | `auto_par` | no automatic fork-join (`Pipeline::set_auto_par`) |
| `Pipeline::set_hir_inline` (unit tests only) | `hir_inline` | no typed inlining |
| `COIL_HIR_BISECT=N` | `hir_bisect` | (a number, unset by default) only the first `N` bodies HIR lowering admits in the process lower; the `N`th is named on stderr. Debug builds only |

Compile-time call folding: `--fold-calls on|off` or
`Pipeline::set_call_fold` (see `call_fold`). In the compiler's own unit
tests, `COIL_HIR_INLINE_WHY` prints why a call was not inlined; other
builds do not read it.

Iterative optimization (COI-130) was removed: re-running the pipeline miscompiled loop tests (LICM is not idempotent) and no opt level enabled it. The pipeline runs once.

Optimization statistics (COI-131) are **off** by default (CLI `--opt-stats` and `--opt-stats-json`). When enabled, it records HIR lowering counters (bodies planned and refused, bodies lowered to MIR), typed-inlining call sites (`hir_inlined`) and compile-time work. Human text or a single JSON object is printed to stderr after a source compile.

The archive version is recorded only in `common/src/archive.rs` ([older minors](archive-history.md)). If an optimization is sound but flagship `.ccoil` do not change, prove it on focused `examples/perf` hit benches ([roadmap](optimization-roadmap.md#hit-bench-prove-rule)).

## Capabilities

Every `HOST_NATIVES` row (`common/src/host.rs`) states the capabilities it
needs (`common::Caps`: read, write, net, env, exit, attach). `open` is
refined by a literal mode (`"r"` → read; `w` / `a` / `x` → write; `+` or a
non-literal mode → both, `common::open_mode_caps`).

HIR lowering tags each gated `HostInvoke` with the call's
`DebugLoc` and records it in `Compiler.gated_host_calls`. Before
`finalize_bodies`, `Compiler::capability_violations`
(`compiler/src/codegen/capabilities.rs`) walks the MIR call graph
(`reachable_bodies`, shared with the tree-shaker) from `main`, the
test cases and the startup bodies, and
reports each reached call whose caps are not in `HostGrants::caps()`, with
the chain that reaches it:

```
error[E0414]: `io::open` requires `--allow-read`: reached from `main` → `load`
```

Starting a program, exit and attach keep their codes (`E0406`, `E0407`,
`E0408`); read / write / net / env are `E0414`. Only `process_spawn` is
gated: it sits in the embedded `process` module, so the chain names the
user's call into it (`main` → `process::exec` → …). Only what the program reaches counts, so importing a module with an
unused `env::exit` helper needs no grant. A build with no `main` and
no tests (a library) reports nothing. Two gates stay at the call site in
typecheck, because they have no MIR call to tag: the `stream.attach(…)` method
form and FFI (`dload`, process-exec symbols). Macro sub-programs are granted
everything: the macro host refuses host calls at run time (`E0119`).

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

## Archives (`out.ccoil`)

Default `coil file.coil` compiles and runs in memory. It does **not** read or write `out.ccoil` (integration tests assert this). An existing `out.ccoil` beside the entry is left untouched; if it looks stale the CLI may warn, but the in-memory compile still runs.

`coil compile [file.coil]` writes `out.ccoil` (or `-o`). `coil run out.ccoil` loads that archive and **warns** when recorded sources are newer; it does **not** rebuild. Missing / corrupt / incompatible archives fail instead of compiling from source.

Incrementality (fingerprint-based reuse of a compile) is a later design, not current CLI behavior.

## Multi-file programs

The pipeline discovers dependencies via `use` / `mod` under the `--root` search roots, compiles each file with a namespace prefix into one shared table of bodies, and finalizes once after linking. The **entry file** uses the empty namespace. Each module is compiled after the modules it imports (`worklist_in_dependency_order`), so modules that import each other, directly or through others, are one error before any is checked: E0154 names the cycle (`a -> b -> a`) at the import that starts it (`Pipeline::import_cycle`, #2536). See [Modules](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/language/modules.md) and [Project config](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/language/project-config.md).
