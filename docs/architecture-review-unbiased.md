# Coil language implementation — unbiased architecture review

**Scope.** Read-only review of this repository’s language implementation: parser, typechecker, IL, codegen, archive, VM, host/FFI, modules, diagnostics, and tests. Conclusions come from source, tests, and in-repo docs only. Linear, GitHub Issues, and prior ticket titles were not consulted. User-facing docs live in the separate `coil-website` repo and were **not** treated as evidence except where this tree quotes them.

**Method.** Walk the crate DAG and the compile/execute path; sample the largest types (`Checker`, `Compiler`, `Machine`, `Pipeline`); cross-check internals docs against CLI and tests. Prefer a small number of high-confidence findings over nits.

**What this file is not.** A rewrite manifesto, a test-coverage lecture, or an implementation plan.

---

## 1. Current architecture (as the code actually is)

### 1.1 What Coil is

A `.hy` program is parsed to an AST, Hindley–Milner-checked on that AST, compiled to a **compile-time stack IL** with symbolic labels, optimized, lowered once to 8-byte `Byte` words (`common::Instruction` + operands), wrapped in a versioned rkyv archive (`.hyc`), and interpreted by a stack VM (`machine::Machine`). There is **no register VM**, **no HIR**, and **no separate name-resolution pass**.

```
source .hy
  → parser::Pratt::parse          parser/src/lib.rs
  → Expression AST                parser/src/ast.rs
  → Pipeline discovery / topo     compiler/src/pipeline.rs
  → attrs::expand_program         compiler/src/attrs.rs   (derive → synthetic impl)
  → Checker::check_program        compiler/src/typechecking/infer/checker.rs
  → monomorphize::plan_monomorphization   compiler/src/monomorphize.rs
  → Compiler::do_compile          compiler/src/codegen/compiler.rs
  → CodeBuf / IlOp                compiler/src/il/{codebuf,op,builder}.rs
  → finalize_bytecode
       treeshake → per-body opts → concat → single il::lower
  → ArchivedProgram               common/src/archive.rs   (major 2, minor 14)
  → Machine::execute              machine/src/vm.rs
```

The same story is documented in `docs/internals/pipeline.md`. That document is accurate on stages and IL pass order. It is **not** accurate on default CLI caching (see finding A6).

### 1.2 Crate layering

Workspace members (`Cargo.toml`): `common`, `parser`, `compiler`, `machine`, `reporting`, `coil-simd`, `coil-cli`, `coil-embed`, `coil-debug`, `coil-dissect`, `coil-fmt`, `coil-lsp`, plus the root `coil` binary.

```
reporting          (ErrorCode, ariadne / SARIF / LSP sinks)
common             (Instruction, Byte, Value, archive, FFI tags, package trailer)
parser ──────────► compiler ──────────► machine ──► coil-simd
                     │                      ▲
                     └────── depends on ────┘   (NativeFn, Heap, host table, test run)
coil-fmt  → parser + reporting
coil-lsp  → compiler + parser + reporting
coil-cli  → common + machine     (archive load, execute, helper re-exec — not clap)
coil-embed → coil-cli
root coil  → compiler + machine + parser + reporting + coil-cli
```

There is **no Cargo cycle**. `machine` does not depend on `compiler`. The conceptual cycle is the **HostInvoke id table**: `machine::build_standard_host_natives` assigns ids by registration order; `Pipeline::register_standard_host_natives` records the same ids on `Compiler::register_native_id`.

`docs/internals/README.md` still describes `coil-cli` as “shared CLI argument parsing.” Clap lives in root `src/cli.rs`. `coil-cli` is load/execute/package helpers (`coil-cli/src/lib.rs`).

### 1.3 Frontend: lexer, parser, AST

The parser is a Chumsky Pratt parser (`parser/src/lib.rs`, crate feature `chumsky` `pratt`). There is no standalone lexer crate. The AST is `parser::ast::Expression` — one large enum covering literals, operators, declarations (`Function`, `Class`, `EnumDecl`, `TypeClass`, …), modules (`Use`, `Module`), FFI surface (`Dload`, `Declare`, `Invoke`, `ExternBlock`), and even `Comment`. Types, patterns, and payloads are sibling enums in the same file.

There is no desugaring IR. Attribute expansion (`compiler/src/attrs.rs`) mutates this AST before typecheck. `coil-fmt` pretty-prints the same AST (`docs/internals/fmt.md`).

### 1.4 Names, types, codegen

**Name resolution is not a phase.** Disk modules are discovered by `Pipeline::enqueue_uses` walking `Expression::Use` / `Expression::Module`. Virtual modules (`compiler/src/typechecking/virtual_modules.rs`) win before `Manifest::resolve_use`. Lexical names and HM schemes live in `Checker` + `Env` (`typechecking/env.rs`). Codegen keeps a **second** alias/function table (`Compiler.aliases`, `Compiler.functions`, overload keys like `name#arity.id`).

**Typechecking** is Algorithm W in `Checker` (`infer/mod.rs` types, `infer/checker.rs` impl, ~17k lines). Unification is `typechecking/unify.rs`. Generics / dictionaries: `typechecking/generics.rs`. The checker is also the **codegen contract**: dozens of side tables (by `NodeId` and by source span) for overloads, dictionaries, for-in lowering, FFI tags, bound methods, pair/niche ABI, etc.

`NodeId`s are minted by a pre-walk (`typechecking/id.rs`) and consumed in lockstep by `infer`. When that lockstep fails, codegen falls back to span maps and `codegen_var_types` (name → `Ty`). Those fallbacks are load-bearing, not temporary — the struct comments say so.

**Monomorphization** (`compiler/src/monomorphize.rs`) is analysis-only. It specialises a **subset** of generic calls (ground Num/Ord/Eq-style bounds, caps `MAX_SPECIALIZATIONS_PER_FN = 8`, `MAX_TOTAL_SPECIALIZATIONS = 64`). Show/Length/user traits stay on dictionary passing. Identity is `MonoKey { fn_name: String, subst: Vec<String> }`.

**Codegen** is `Compiler` (`codegen/mod.rs` fields, `codegen/compiler.rs` ~16k lines). `do_compile` / `do_compile_inner` recurse on the AST the same way `infer_inner` does. Locals and operands share one VM buffer; `Compiler.expr_depth` exists so temps do not clobber live operand values. After all files: splice static/FFI/finalizer setup, tree-shake from `main` (and tests when requested), optimize, **one** `il::lower`.

### 1.5 IL, opts, bytecode

`IlOp` (`compiler/src/il/op.rs`) is a typed stack IL: `Load`, `Const`, `Bin`, fused `*Jmpf`/`*Return`, `HostInvoke`, plus residual `IlOp::Byte` for the long tail. Production jumps are symbolic `IlOp::Jump` / `IlOp::Entry` until lower assigns PCs. `CodeBuf` is the real emit façade; `IlBuilder` is a thinner label API with much of its public surface `#[allow(dead_code)]`.

Opts (`compiler/src/il/opt/`, plus `canon`, `gvn`, `licm`, `bounds`, …) are fail-closed on opaque `Byte`, unknown cursor, calls, and host. Two cursor analyses are **intentionally different**: `il::sp` (operand height) vs `il::tell` (shared-buffer floor after `STORE`). That split is asserted in `il/sp.rs` and `il/tell.rs` and differentially tested in `compiler/tests/cursor_model.rs`.

ISA: `common/src/opcode.rs`, `#[repr(u8)]`, append-only. `machine/src/opcode.rs` only re-exports. Archive load rule: same major, archive minor ≤ runtime minor (`ARCHIVE_MAJOR = 2`, `ARCHIVE_MINOR = 14`).

### 1.6 Runtime, memory, concurrency, FFI

`Value` (`common/src/value.rs`) is an **untagged** word. Heap objects (`machine/src/memory/heap.rs`) carry the real discriminant (`Object::Array`, `Instance`, `Enum`, `Coroutine`, `Stream`, …). GC is non-moving mark-and-sweep on an intrusive list; `Gc<T>` is `Copy`; `payload_mut(&self) -> &mut T` assumes a single-threaded mutator. Live GC is **in** `heap.rs`. `machine/src/memory/garbage/` is leftover sketch (`collector.rs` “Legacy… unused”).

Two host surfaces:

1. **HostInvoke / HostInvokeNiche** — stable integer ids, table built in `machine/src/host_natives.rs`. Virtual modules (`io`, `thread`, `env`, `gc`, packed LA, Vec helpers, …) compile to this, not new opcodes.
2. **User FFI** — `FfiLoad` / `DeclareFFI` / `FfiInvoke`, libloading + libffi, fail-closed `DloadGate` (`machine/src/ffi/`).

Concurrency: CPU work-stealing reactor (`machine/src/reactor.rs`) for `thread::spawn` / auto-par; IO poll reactor (`io_reactor.rs`); stackful coroutines (`MakeCoro` / `YieldCoro` / …). Cross-thread values are deep-copied (`PortableValue`), not shared heap pointers.

Embedding: packaged apps splice an archive onto `coil-embed` (`common/src/package.rs`). That “package” is **not** the spool/`coil.toml` `[package]` concept (`compiler/src/manifest.rs`).

### 1.7 Modules, stdlib, visibility

`coil.toml` (`Manifest`) supplies search `roots`, FFI allowlists, optional `[package]` / `[dependencies]` metadata. The compiler **does not fetch** dependencies; spool paths must appear in `[module].roots`. Two file conventions: `root/a/b/name.hy` vs `root/a/b.hy` containing `name`. Entry file uses the empty namespace.

**Virtual modules** (compiler-owned, not on disk): `prelude`, `prelude::ops`, `prelude::test`, `prelude::math`, `ffi`, `io` (+ net/fs), `string`, `thread`, `env`, `gc`. Time / regex / TLS / crypto / HTTP / collections are **extracted packages** (comments in `virtual_modules.rs`, `docs/internals/collections-vm-split.md`).

`Visibility` exists on class fields and impl methods (`parser::ast::Visibility`). Enforcement is incomplete (finding A5). Top-level `fn` has no module `pub`; `use path::name` is enough.

### 1.8 Diagnostics and incrementality

`reporting` owns `ErrorCode` (`E00xx` parse, `E01xx` names/types, …) and sinks. `Pipeline` drains checker messages. LSP (`coil-lsp`, `docs/internals/lsp.md`) keeps overlays and re-typechecks the project graph from the open entry; `ProjectIndex` (`compiler/src/project_index.rs`) is parse-based symbols plus checker lookup. **There is no incremental compile.** `source_cache` only avoids double-reads inside one compile. `.hyc` is a whole-program artifact.

### 1.9 Tests as a map of the design

| Location | What it actually pins |
|----------|------------------------|
| `tests/positive/*.hy` | Language surface that must run (`coil test`) |
| `tests/compile_fail/*.hy` | Parser/type diagnostics (`wildcard_import.hy` → E0124, exhaustiveness, FFI, …) |
| `tests/negative_runtime/` | Soft runtime failures |
| `compiler/tests/{pipeline,diagnostics,namespace,cursor_model,perf_metrics}.rs` | Multi-file, HostInvoke ids, cursor ≡ VM, opt metrics |
| `compiler/src/**` unit tests | HM, codegen goldens, including `Pipeline::compile_test` |
| `parser/src/tests/` | Pratt/AST |
| `machine/src/vm.tests.rs` | Dispatch / host / opcodes |
| `tests/*_cli.rs` | Default run must **not** write `out.hyc`; compile still does |

Extracted-package tests are **intentionally absent** (AGENTS.md). That is a product split, not an accidental hole in this repo — but it means “stdlib vs language core” cannot be validated here.

---

## 2. Architectural problems

Each item: what is wrong, where, why it matters, severity, a concrete direction (not a rewrite).

### A1. Two 16k-line AST walkers are the compiler

**What’s wrong.** There is no HIR. `Checker` (~17.2k lines in `checker.rs`, 100+ fields in `infer/mod.rs`) and `Compiler` (~15.9k lines in `compiler.rs`, ~74 fields in `codegen/mod.rs`) each contain a giant recursive match on `Expression`. `docs/internals/limitations.md` records that those frames previously overflowed into adjacent heap (oversized `match` arm locals). The split into `#[inline(never)]` helpers and `infer_depth` / `codegen_depth` guards is defense, not a boundary.

**Where.** `compiler/src/typechecking/infer/{mod.rs,checker.rs}`, `compiler/src/codegen/{mod.rs,compiler.rs}`. `Pipeline` (~2.3k lines) is an orchestrator, not a third walker.

**Why it matters.** Every new surface (pair ABI, auto-par, overload keys, FFI tags) becomes another field on both gods and another arm in both matches. Phase coupling is implicit: codegen re-walks IDs, consults span fallbacks, and can emit for ASTs the checker rejected (`compile_test`). Ownership of “what does this node mean?” is not in the tree; it is in parallel hash maps.

**Severity.** **High** (correctness and evolution cost; historically a crash class).

**Direction.** Introduce a **typed HIR** (or attributed AST) produced by check, with DefIds instead of `String` FQNs. Move `do_compile` to walk HIR only. Keep `Expression` as parse/fmt IR. Extract remaining oversized arms into modules by construct (`match`, `call`, `use`) as an incremental step — the depth guards stay until the frame is gone.

### A2. Name resolution happens three times and disagrees by design

**What’s wrong.** File graph: `Pipeline`. Types/bindings: `Checker` (`scope_bindings`, `disk_imports`, `Env`). Call targets: `Compiler.aliases` / `functions`, rebuilt for disk `use` in the `do_compile` Use arm. Virtual `use` is applied in check; disk FQN shape is probed against `functions` (`item_in_module` vs per-item file). `fn_arities` survives `check_program` clears of `fn_param_names` so imported `MakeFn` still packs arity — a comment on `Compiler.fn_arities` admits the two tables would otherwise diverge.

**Where.** `compiler/src/pipeline.rs` (`enqueue_uses`, `discover_all`), `Checker` fields in `infer/mod.rs`, `Compiler` fields in `codegen/mod.rs`, Use lowering in `codegen/compiler.rs`.

**Why it matters.** Lambda/defer must rebind file-level imports after isolation (`disk_imports`); tests exist because this was a real hole. Overloads, methods, and mono clones each invent another string key (`Owner::method`, `name#2.0`). There is no DefId, so inlining, tree-shake, and LSP definition lookup each re-implement resolution (`ProjectIndex` + `Checker::lookup_for_codegen_span`).

**Severity.** **High**.

**Direction.** Bind names once into interned `ModuleId`/`DefId` during check (even if still stored on the AST). Codegen and LSP should only consume those ids. Kill `aliases` as a second resolver; keep a debug name table if needed for dissect.

### A3. `compiler` depends on `machine`: the frontend owns the host

**What’s wrong.** `compiler/Cargo.toml` depends on `machine`. `pipeline.rs` imports `Heap`, `NativeFn`, `FfiType`, `HostClosureFn`, builds the standard host table, wires `ThreadProgram`, and constructs `Machine::<128>` to run programs in-crate. Typechecking FFI layouts talks `machine::CStructLayout`. Tests in `infer.tests.rs` import `ENV_WIRING` / `FS_WIRING`.

**Where.** `compiler/src/pipeline.rs` (top imports; `register_standard_host_natives`; `wire_host_natives`; run helpers ~1767+), `compiler/Cargo.toml`.

**Why it matters.** The compile crate cannot be reasoned about as “language only.” Host signatures, reactor defaults, and VM types leak into typecheck/codegen tests. A VM change can fail the compiler crate for non-language reasons. Conversely, HostInvoke id assignment is a **runtime table** that the typechecker must shadow.

**Severity.** **High** (layering), not a user-facing blocker.

**Direction.** Split a tiny `coil-host-abi` (or put ids/signatures in `common`): names, arities, `FfiType`, registration order. `compiler` depends on that. `machine` implements it. `Pipeline::run_*` test helpers move to an integration crate or `compiler` `dev-dependencies` only — production `Pipeline` should not construct `Machine`.

### A4. Host capability is an append-only integer ISA with two deletion policies

**What’s wrong.** Host natives are not a named interface in the archive. Ids are “index in `build_standard_host_natives`.” Removed **time** slots remain **panic stubs** (`TIME_REMOVED`, `push_removed_time_stubs`) so `stream_attach` / `stream_park` stay 120 / 121. Removed TLS/crypto/regex slots **collapsed**, bumping archive minor (11, 14). Both policies live in one table. `NATIVE` opcode is a deprecated no-op (`common/src/opcode.rs`).

**Where.** `machine/src/host_natives.rs` (module docs and `build_standard_host_natives`), `common/src/archive.rs` minor notes, `machine/src/vm.rs` `Instruction::HostInvoke` / `HostInvokeNiche`.

**Why it matters.** Extracting a package from the language core is an ABI event. Panic stubs are a language-level landmine if a stale archive still emits those ids. Collapse requires every runtime to bump together. There is no typed “host module version” separate from `ARCHIVE_MINOR`.

**Severity.** **High** for anyone evolving virtual modules; **medium** for day-to-day language work.

**Direction.** Freeze a **named** host catalog in the archive (string or stable uuid → id map) or version the host table independently of opcode minor. Stop mixing collapse vs stub: new removals should be “unknown id → language error” (see W1), not panic or silent skip. Keep 120/121 by recording them explicitly, not by padding with dead time.

### A5. Visibility is syntax and storage, not a module system

**What’s wrong.** `Visibility::Public/Private` is parsed and stored (`Checker.classes`, `Checker.methods`, `inherent_method_visibility`). Codegen uses it for **inlining** (`Compiler::callee_is_visible_for_inline`), not as an access error. Test `class_visibility_recorded_for_future_member_access` (`infer.tests.rs`) states the future-phase intent. Top-level functions are freely `use`d. `Checker.methods`’s own field comment still says methods are registered “for a future phase” even though `checker.rs` reads that map in many places — the comment is stale; the **access** rule is what is missing.

**Where.** `parser/src/ast.rs` (`Visibility`), `infer/mod.rs` (`methods`, `classes`), `checker.rs` (`inherent_method_visibility`), `codegen/compiler.rs` (`callee_is_visible_for_inline`), `infer.tests.rs`.

**Why it matters.** Callers can treat `pub` as safety. It is currently a pretty-printer / inliner hint. Cross-module encapsulation is “do not `use` the name.”

**Severity.** **Medium** (language-product hole). Not a VM blocker.

**Direction.** Enforce private fields/methods in `Checker` on `Access` / `Call` with a stable `ErrorCode`. Keep the inline gate as a consequence of the same query. Add `tests/compile_fail/` cases. Decide explicitly whether top-level `fn` stays universally exportable.

### A6. Docs and CLI disagree on what “compile cache” is; nothing is incremental

**What’s wrong.** `docs/internals/pipeline.md` § Cache and rebuild: re-running the same CLI entry reuses `out.hyc`, auto-recompiles when stale. Tests (`tests/default_run_cli.rs`) require default `coil file.hy` **not** to read or write `out.hyc`. `coil run out.hyc` warns on staleness (`src/main.rs` `archive_staleness`) and does **not** rebuild. LSP re-typechecks the whole graph (`docs/internals/lsp.md`). `Pipeline.source_cache` is intra-compile only.

**Where.** `docs/internals/pipeline.md`, `src/main.rs`, `tests/default_run_cli.rs`, `coil-lsp/src/main.rs`, `compiler/src/pipeline.rs`.

**Why it matters.** Contributors will “fix” cache bugs that are not bugs. LSP cost scales with whole-program HM. There is no per-module `.hyc` / fingerprint, so IDE and CLI cannot share incremental work.

**Severity.** **Medium** (DX / architecture honesty). The compiler model (whole-program, one lower) is internally consistent.

**Direction.** Fix `pipeline.md` to match CLI (artifact vs in-memory run). If incrementality is desired, define **module fingerprints** (source hash + import graph) and cache **typed HIR or unlowered IL per module**, still lowering once — do not pretend `.hyc` is rustc incremental.

### A7. Dual type representations and dual kinds

**What’s wrong.** `parser::ast::Kind` and `compiler::typechecking::kind::Kind` are parallel enums (`Type` / `Constraint` / `Arrow`). Option/Result lowering is a **matrix** of pointer niche, boxed heap enum, and two-slot `[payload, tag]` pair, selected by flags on `Compiler` (`force_heap_option`, `force_niche_option`, `compiling_pair_mode`, `pair_value_context`) plus `pair_return_kinds: RefCell<HashMap<String, Option<bool>>>`. The memo exists because “an unmemoized query can answer differently for a body and for a later caller” (`codegen/mod.rs` on `pair_return_kinds`).

**Where.** `parser/src/ast.rs`, `compiler/src/typechecking/kind.rs`, `Compiler` ABI flags in `codegen/mod.rs`, `pair_return_kind` in `compiler.rs`.

**Why it matters.** Dual `Kind` will drift. The Option ABI is a **phase-ordered compilation effect**, not a type. Callers and callees can disagree if pinning is missed. Host/FFI/coroutine boundaries each convert (`OptionNicheToHeap`, `HostInvokeNiche`, …). This is the same class of leak as stringly `MonoKey`.

**Severity.** **Medium** (ABI; high if you change Option lowering without the pin).

**Direction.** One `Kind` type in `parser` or `common`, converted once. Compute pair/niche **per DefId** during check and store it on HIR; codegen must not re-derive from a filling type env.

### A8. Feature flags are mostly tooling — except empty `pratt` and HostInvoke-as-architecture

**What’s wrong.** `debugger` / `dissect` / `vm_profile` correctly live on helper binaries and optional modules (`coil-debug`, `coil-dissect`, `Machine` debug hooks). Default features are `[]`. That is sound. `compiler` still declares `features.pratt = []` with no code behind it (`compiler/Cargo.toml`). HostInvoke id stability (A4) is the real “feature flag used as architecture”: extracting time/TLS/crypto changed the **language ABI**, not a Cargo feature.

**Where.** `compiler/Cargo.toml`, `machine/Cargo.toml`, `machine/src/host_natives.rs`.

**Severity.** **Low** for `pratt`; **see A4** for host extraction.

**Direction.** Delete unused `pratt` feature. Treat host-table changes as archive/host-catalog version, not as “we turned a virtual module off.”

---

## 3. Weak implementations

### W1. `HostInvoke` failure does not produce a language value

**What’s wrong.** On `Instruction::HostInvoke`, `Err` and unknown id `eprintln` in debug and **do nothing** in release. No value is pushed. The opcode pops the id and args tuple first. Later ops then read the wrong stack slot. `Ok(None)` is overloaded: IO park (`take_pending_io_park`) vs “no value.”

**Where.** `machine/src/vm.rs` (`Instruction::HostInvoke` match, ~3224–3295). Contrast `NativeFn::invoke` → `Result<Option<Value>, FfiError>` in `machine/src/ffi/registry.rs`.

**Why it matters.** Host errors are not `Result` at the language level. A mismatched id after a host-table change (A4) is silent memory corruption in release, not a diagnostic. This is weaker than user FFI, which has `FfiError` paths.

**Severity.** **High** (runtime contract).

**Direction.** HostInvoke must always push a defined value or trap: `Result` encoding, `panic` opcode, or `HALT` with a message. Unknown id is a hard error. Reserve `Ok(None)` exclusively for park, and `promise!` that a park request is pending.

### W2. `Instruction::from(u8)` is a transmute; safety is a comment on `promise!`

**What’s wrong.** `From<u8> for Instruction` is `unsafe { transmute(value) }` (`common/src/opcode.rs`). Release dispatch relies on `promise!(*bc as u8 <= Instruction::StoreIndexPinUnchecked as u8)` (`machine/src/vm.rs`). A stale ceiling makes **later** opcodes UB, as the comment says. Unchecked index opcodes (`IndexUnchecked`, pin twins) are documented UB if the compiler’s proof is wrong.

**Where.** `common/src/opcode.rs` (`From<u8>`), `machine/src/vm.rs` (ceiling), opcode comments in `opcode.rs`.

**Why it matters.** The ISA has no invalid-opcode trap. Archive versioning is the only fence. That is a coherent performance choice, but the invariant is **distributed** (append variant → bump minor → bump `promise!` → test `instruction_from_u8_covers_last_appended_variant`). Miss one and release is UB, not a panic.

**Severity.** **High** for ISA evolution; **low** if the three-site ritual is always followed (tests exist).

**Direction.** Keep transmute on the hot path if measured, but decode through a `try_from` in debug and when loading archives (reject bytes above last known). Generate the ceiling from the enum so it cannot drift. Treat compiler bugs that emit unchecked ops as a `debug_assert` even in “proven” loops when `cfg(debug_assertions)`.

### W3. Typecheck is optional on a supported compile entry point

**What’s wrong.** `Pipeline::compile_test` documents that it **ignores typecheck messages** so `fizbuz_runs_to_completion` can compile an example the checker rejects (`return;` parsed as a variable). Production `compile_src` does check; this API still exists and emits bytecode.

**Where.** `compiler/src/pipeline.rs` (`compile_test`).

**Why it matters.** It proves codegen is not a function of well-typed HIR. Goldens can lock in ill-typed lowering. Combined with A1, this is how “the typechecker said no” and “the VM ran it” coexist.

**Severity.** **Medium**.

**Direction.** Move fizbuz to an explicitly ill-typed fixture under `compiler/tests` that asserts **diagnostics**, or parse-fix the example. Delete `compile_test` or `#[cfg(test)]` gate it with a name that cannot be mistaken for a language guarantee.

### W4. Stringly IL/keys and residual `Byte`

**What’s wrong.** Specialization identity is `Vec<String>` (`MonoKey`). Module/FQN/overload keys are strings. `IlOp::StorePop` **encodes** `Instruction::STORE` (`il/op.rs`); the VM still has deprecated `StorePop` for old archives (`common/src/opcode.rs`). `IlOp::Byte` remains the escape hatch; opts fail closed on it, so anything left as `Byte` is deoptimized and opaque. `IlBuilder` public API is largely unused.

**Where.** `compiler/src/monomorphize.rs`, `compiler/src/il/op.rs`, `compiler/src/il/builder.rs`, `common/src/opcode.rs`.

**Why it matters.** Two names for one store confuse readers and tests. Residual `Byte` is a second ISA inside the IL. String keys make renaming and hygiene coincidental.

**Severity.** **Medium**.

**Direction.** Rename `IlOp::StorePop` to `Store` in IL (keep VM discriminant). Inventory remaining `Byte` emitters and lift or accept them as a documented “cold” set. Intern type names for `MonoKey` (`Ty` intern ids).

### W5. Dead GC layer and other dual paths

**What’s wrong.** `machine/src/memory/garbage/collector.rs` is an unused block-marking sketch (`unsafe` pointer walk). `garbage/mod.rs` says alternate helpers are unused. Live GC is `heap.rs`. Tombstone opcodes `DATA`, `PRINT`, `NATIVE` remain in the enum. `Compiler.module_items` is “legacy; disk `::*` no longer expands.”

**Where.** `machine/src/memory/garbage/`, `common/src/opcode.rs`, `codegen/mod.rs` (`module_items`).

**Why it matters.** New contributors will “fix” the sketch collector. Tombstones are justified by append-only ISA; the unused GC module is not.

**Severity.** **Low** (dead code) / **medium** if someone wires the sketch.

**Direction.** Delete or `#[cfg(never)]` the unused garbage modules. Keep opcode tombstones. Remove `module_items` if no reader remains.

### W6. Glob `use` is specified in-tree and rejected by the checker

**What’s wrong.** `coil.toml.example` documents `use foo::bar::*;` as loading a file and importing every top-level name. `reporting::ErrorCode::WildcardImport` (E0124) and `tests/compile_fail/wildcard_import.hy` reject `use io::*;`. Pipeline discovery still **mentions** globs when walking uses (`pipeline.rs`). Parser accepts the syntax.

**Where.** `coil.toml.example` (Glob imports section), `reporting/src/codes.rs`, `tests/compile_fail/wildcard_import.hy`, `compiler/tests/diagnostics.rs`.

**Why it matters.** Dual grammar: example config vs typechecker. Discovery vs check can still enqueue files for a program that will fail E0124.

**Severity.** **Medium** (product honesty). Easy to fix.

**Direction.** Delete or rewrite the example section to match E0124. If globs are a future feature, say “not implemented” in the example, not “loads `foo/bar.hy`.”

### W7. Purity / auto-par is a second, name-based compiler

**What’s wrong.** After typecheck, `typechecking/purity.rs` walks the AST with **unqualified Identifier** callee names, skips impl methods because `join(self.thread)` would look like a self-call, and feeds auto-par (`docs/internals/auto-par.md`, `par_profit.rs`, `loop_par.rs`). That analysis does not use `Checker` resolutions.

**Where.** `compiler/src/typechecking/purity.rs` (module docs and `analyze_recursive_fns`), `docs/internals/auto-par.md`.

**Why it matters.** A transform that forks OS work (`thread` reactor) is gated on a syntactic approximation sitting in the typechecking **folder** but not on the type environment. False purity → data races / IO in parallel; false impurity → missed opts (safer). The code chooses skip-methods conservatism, which is honest, but the layering is wrong.

**Severity.** **Medium**.

**Direction.** Run purity on resolved call graphs (DefIds / `call_site` tables already on `Checker`). Keep fail-closed for host/FFI/yield. Do not infer purity from identifier spelling.

### W8. `codegen_var_types` and span dual maps: incomplete NodeId lockstep shipped as complete

**What’s wrong.** `Checker` documents span + name side tables “when pre-walk IDs are misaligned.” Tests (`infer.tests.rs`) note free-fn args still go through the name side-table (`assign_fn_arg_node_ids` deferred). Codegen match arms push `mono_codegen_var_types` overlays so arm-local types do not clobber each other.

**Where.** `infer/mod.rs` (`cache`, `codegen_types_by_span`, `codegen_var_types`, `call_site_dicts_by_span`, …), `codegen/mod.rs` (`mono_codegen_var_types`).

**Why it matters.** The typed-compiler invariant (“infer and emit visit the same nodes”) is known false for some parameters. Workarounds look like a finished design.

**Severity.** **Medium**.

**Direction.** Finish ID assignment for free-fn params or stop claiming lockstep. Prefer HIR (A1) so lookup is by node identity, not span/`String`.

### W9. Collections/stdlib gaps encoded as language law

**What’s wrong.** `docs/internals/collections-vm-split.md` states remaining holes: nested generics in array types fail parse; **free** `fn f<T>(T) -> Option<T>` corrupts payloads; methods returning `Option` are OK. That is an implementation accident treated as API law (`AGENTS.md` method-based APIs). The VM does not have map opcodes; HashMap is userland — that split is coherent. The generic-enum-return bug is not.

**Where.** `docs/internals/collections-vm-split.md` (“Known language gaps”), codegen unbox/`PolyFn` paths, `tests/positive` covering method/`Option` more than free generic enum returns.

**Why it matters.** The language looks like it has generics and `Option`; a normal functional helper is unsafe. Tests in this repo will not catch stdlib `get → Option` if that lives in coil-stdlib.

**Severity.** **High** as a language hole; **medium** inside this repo because the failure mode is documented and methods are the supported path.

**Direction.** Fix unbox/pair/niche for free generic enum returns (one ABI, tests in `tests/positive/`). Until then, typecheck should **reject** the unsupported shape instead of compiling corrupting bytecode.

### W10. `Gc::payload_mut` and shared-stack unsafety are real, but commented

**What’s wrong.** `payload_mut` mutates through `&self` (`heap.rs`). `Stack` uses `promise!` + unchecked indexing. These **do** have invariant comments (single-threaded VM; cursor vs height). Clippy is not the lint gate because of `payload_mut`. This is weaker than encoding `!Sync` on `Machine` / `Heap` in a way that makes the API hard to misuse from workers.

**Where.** `machine/src/memory/heap.rs` (`Gc::payload_mut`), `machine/src/memory/stack.rs`, `machine/src/thread.rs` (isolated worker `Machine`s).

**Severity.** **Low** given isolation of worker machines; **medium** if anyone shares `Heap` across threads.

**Direction.** Keep comments. Make `Heap`/`Gc` `!Sync` (if not already via `Cell`/raw pointers). Do not add a second GC.

---

## 4. Cross-cutting themes

These five explain most of the local findings.

1. **Side tables instead of a typed IR.** NodeId lockstep, span maps, `codegen_var_types`, string FQNs, `MonoKey`, pair-ABI `RefCell` — all compensate for compiling the parse tree twice (A1, A2, A7, W4, W8).

2. **One shared operand/local buffer is the language’s memory model.** `sp` vs `tell`, `expr_depth`, slot promotion refusals, match bindings skipping `STORE`, array pins on frames — IL opts are really **cursor proofs**. That is internally rigorous (`cursor_model.rs`) and also why SSA rename is refused: the machine is not an SSA machine (limitations / `il/tell.rs`).

3. **The host is a second, integer ISA.** Virtual modules, extracted packages, panic stubs vs collapsed holes, silent HostInvoke errors (A3, A4, W1). Language evolution is archive-minor choreography.

4. **Whole-program, one lower, opts as production.** Tree-shake, fuse-select, pin/unchecked index, auto-par, PGO plumbing — all assume a full program image. There is no incremental story (A6). Feature flags on Cargo are not how the ISA is sliced; IL pass flags (`OptLevel`, `iterative_optimization`) are.

5. **In-repo docs and examples are a second grammar.** Glob imports (W6), `out.hyc` cache (A6), `coil-cli` role (README), `methods` “future phase” comment vs actual uses (A5), `pipeline.md` vs `default_run_cli.rs`. The code often has the later decision; the docs keep the earlier one.

---

## 5. What is actually solid

The review is not a complaint list. These parts match the comments and the tests.

**Crate DAG and opcode discipline.** `common` owns the ISA and archive; `machine/src/opcode.rs` does not fork it. Append-only `Instruction`, packed `ARCHIVE_MAJOR/MINOR`, loader compatibility, and the `promise!` ceiling (when updated) are a real ABI process. Residual tombstone opcodes (`DATA`, `StorePop` discriminant) are the cost of that process, not sloppiness.

**Fail-closed IL.** Bounds/pin rewrites, escape analysis, slot promotion, copy-prop, and GVN document refusals and keep `Byte`/host/yield as barriers. `compiler/tests/cursor_model.rs` treating cursor mismatch as memory corruption is the right threat model. Preferring HostInvoke over benchmark-shaped opcodes is consistently applied (packed LA in `machine/src/packed_la.rs`, collections in userland).

**Diagnostics crate.** Stable `ErrorCode`s, multiple sinks, compile_fail harness keyed on codes (`E0124`, exhaustiveness, …). Parser and typechecker share reporting rather than stringly `eprintln`.

**Virtual vs extracted split (runtime).** Time/regex/TLS/crypto are not in `VirtualModules`; HostInvoke leftovers are documented in `host_natives.rs`. Package IO is `stream_attach` / `stream_park` + `dload`, not a VM TLS object. FFI `DloadGate` + lockfile hashes are fail-closed relative to “dlopen anything.”

**Concurrency isolation.** Worker `Machine`s, `PortableValue` copies, `Reactor::shutdown` at end of `run_with_pool`, attach handshake not nest-stealing the peer — these are consistent with a single-threaded mutator GC. Coroutines vs OS jobs are distinct.

**Tooling binaries.** Debugger/dissect/fmt/lsp as sibling re-execs with feature gates, default features `[]`, and tests that default `coil` does not write `out.hyc`, are clean product architecture.

**Tests that pin contracts, not just coverage.** Wildcard import compile_fail vs example glob (the test is the true spec). `tell_symbolic_il_matches_bytecode`. HostInvoke id / lazy compiler init tests in `compiler/tests/pipeline.rs`. Visibility test that **admits** the future pass. Those are evidence of self-awareness, even when the product is incomplete.

**Parser as a crate.** Pratt parser + AST + fmt + LSP can share `Expression` without pulling the VM (except LSP via `compiler`). That boundary is real.

---

## 6. Access notes

- This tree was fully readable: `parser/`, `compiler/`, `machine/`, `common/`, `reporting/`, CLI helpers, `docs/internals/`, `tests/`, `examples/`.
- **Not in this repo:** `coil-website` user docs, `coil-stdlib` / `coil-regex` / `coil-tls` / `coil-http` / `coil-crypto` / `coil-time` implementations. Stdlib-vs-core conclusions stop at the virtual-module registry and the collections split doc.
- **Not used:** Linear, GitHub Issues, PR discussion. In-repo docs that mention ticket ids were read as comments, not as a backlog to match.

---

## 7. Suggested reading order (for the next person)

1. `docs/internals/pipeline.md` then `compiler/src/pipeline.rs` (`compile`, `enqueue_uses`).
2. `typechecking/infer/mod.rs` (`Checker` fields) — the real compiler state.
3. `codegen/mod.rs` (`Compiler` fields) + `il/op.rs` + `il/module.rs` (`optimize_and_flatten`).
4. `common/src/opcode.rs` + `machine/src/vm.rs` dispatch + `host_natives.rs`.
5. `compiler/tests/cursor_model.rs` and `tests/default_run_cli.rs` — where docs and machine meet.
