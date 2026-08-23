# Compilation and execution pipeline

How a `.hy` program becomes running bytecode on the VM.

## Stages

1. **Parse** — the Pratt parser in `parser/` reads `.hy` source into an AST.
2. **Typecheck** — Algorithm W (Hindley–Milner) in `compiler/src/typechecking/` infers types and reports source-anchored diagnostics via ariadne.
3. **Codegen (stack IL)** — `compiler/src/lib.rs` emits a compile-time stack IL (`compiler/src/il/`) with **symbolic labels** for jumps/joins. Control flow uses `IlOp::Jump` / `IlOp::Label` instead of absolute PCs. Ground heap-only `Option<T>` values use a pointer niche; statically known unary `Option`/`Result` returns can use a two-slot pair until a boxed boundary.
4. **Lower** — after multi-file link, `finalize_bytecode` **tree-shakes** unused function bodies (eager builtin dict thunks / unreferenced user fns) reachable only from `main` (and `test_cases` when `--include-tests`), then rebuilds an owning **`IlModule`** from the flat emit stream: per-body opts (`jump_thread`, `dead_block` after JMP/RETURN, `stack_dce`, `mem_fwd`/`copy_prop`/`dead_store`, **`canon`** (operand-order: `Const;Load` → `Load;Const` + commutative load/load slot order; int `ConstPool` demote when safe; Known-SP; `CanonStats`; no float reassoc), `algebraic`, optional **`cast_spill`** (default on — spill `CastIntToFloat` ahead of float chains; fuse stage0 accepts `LOAD;CONST`), `licm` (which first runs **`il::bounds`**: in a loop whose addressed arrays provably keep their length — element writes do, `ArrayPush`/calls do not — the invariant `LOAD a; ArrayLen; STORE t` triple and the `CONST; STORE t` pair behind `a[i] = 0` move to the preheader, whose store floors the cursor at `t + 1`; no bounds check is removed), **`escape_analysis`**: fail-closed scalarization of non-escaping `MakeArray` into consecutive frame slots (GC roots; calls/returns/fields/`ArrayPush` keep the heap object), **`return_convoy`**: identical immediate `LOAD`/`CONST` into a return-label cluster → `*Return` (`JMP`, or `JMPF`/`JMPT` value-under-cond, or all-`JumpIfMatch`; not mixed across classes; cond/match/jump-only preds need Known join SP), **`bin_join_convoy`**: identical plain binop / `BinSlot*` tails → `BinReturn` or one shared `BinSlot*` before `RETURN` (same pred/SP gates)) + CFG GVN (whose Dup-CSE is then re-expanded to a second `LOAD` so fuse-select still sees both binop operands) + **`slot_promote`** (drops a `STORE t` reached with the cursor at `t + 1` — TOS already *is* slot `t` — plus the reload run in front of a `TailCall`; only when every remaining reference to the slot goes too, and after GVN so no slot is left with a def only a push provides), then **`multi_op_join_convoy`** on the concatenated buffer (SP join gates need whole-module height context; `JMP`/`JMPF`/`JMPT`/`JumpIfMatch` preds allowed) and finally **`invert_guard_branch`** (`JMPF A; JMP B; A:` → `JMPT B`, refused when the condition would fuse into a `*Jmpf` — there is no `*Jmpt`), then a **single** `il::lower`: fuse-select with **label / abs-jump barriers** (plus `*Return` join-on-window[0]), assign PCs once, encode `Vec<Byte>`. Hot-path ops are **typed `IlOp` variants** (`Load`, `Const`, `Bin`, `BinSlot*`, `*Return`, …) lifted on `CodeBuf::push` absorb; long-tail typed forms also include `HostInvoke` / `Print` / `GetField` / `SetField` / `ConstPool`; residual `IlOp::Byte` is rare (FORMAT/FFI/…). Enabled fuse patterns: const fold, `BinSlotImm` / `BinSlotSlot`, `CmpJmpf` / `LogNotJmpf` / `BinSlotImmJmpf` / `BinSlotSlotJmpf`, `BinSlotImmStore` / `BinSlotSlotStore` (the latter takes float ops as well as int — archive minor 2), packed multi-slot `LOAD`/`STORE` (`n≤3`), `LoadReturnSlot` / `ConstReturnImm` / `BinReturn`. CALL/TailCall/MakeCoro/CodePtr/MakePolyFn use `IlOp::Entry`; production abs JMP is rejected in lower — no post-lower peephole/`adjust_target` hot path. Tiny direct-call inlining judges candidacy on **IL emitting ops**, can expand a sole `ConstReturnImm`/`LoadReturnSlot`/`BinReturn` body or a pure ≤3-op micro-body, and remaps `BinSlotImm`/`BinSlotSlot` slots through caller temps. Per-function fuse-select was measured with no clear win on the `examples/perf/` baselines — production keeps a **single** fuse/PC lower after concat. Per-function `IlFunc` metadata (name, entry label, code span) is recorded on `CodeBuf` alongside `fn_bytecode_spans`.
5. **Archive** — bytecode is wrapped in a versioned `ArchivedProgram` envelope (packed `major.minor` in `common/src/archive.rs`) and written to `out.hyc` (or another path via `compile -o`). See [Debug line table](debug-info.md).
6. **Execute** — the VM in `machine/` loads the archive and runs `main` (or each `test("…")` case under `coil test`).

```
AST + HM → Stack IL (labels) → IL opts → lower/fuse-select → Vec<Byte> → .hyc → VM
```

## Optimization levels

`Pipeline::set_opt_level` / CLI `-O` / `--opt-level` select an [`OptLevel`](../../compiler/src/il/opt/opt_level.rs) preset that fills `OptimizeOptions` (and tiny-inline budgets). Default is **Standard** — the same pass set as before this flag existed.

| Level | CLI | IL passes |
|-------|-----|-----------|
| None | `-O0`, `none` | algebraic / const-fold only |
| Basic | `-O1`, `basic` | None + jump threading, DCE, copy/mem forwarding |
| Standard | `-O2`, `standard` (default) | all currently-on production passes |
| Aggressive | `-O3`, `aggressive` | Standard + `seek_back_edge` and a larger inline budget |
| Size | `-Os`, `size` | Standard without loop unroll or shared-return cloning |
| Debug | `-Og`, `debug` | Basic only (no slot promotion, escape SROA, unroll, or GVN) |

`None ⊂ Basic ⊂ Standard ⊂ Aggressive` on enable flags. Size and Debug are independent axes.

Branch layout (COI-128) is **on** by default (`branch_optimization`). It inverts a terminating then-arm after `JMPF`/`JMPT` only when operand height is Known at the jump and along the moved arm, and mints a module-wide-unique label for the cold region so later functions cannot bind the same id. A profile that says the fall-through is hot leaves the layout alone.

Basic-block reordering (COI-129) is **on** by default (`block_reordering`). It only sinks jump-only terminating blocks that are not fall-through successors and are not unconditional-jump joins, so labels and branch polarity stay put.

Iterative optimization (COI-130) is **off** by default (`iterative_optimization`). When enabled, the full IL pass pipeline re-runs until a round is a no-op or `max_optimization_iterations` (default 10, clamped to 1..=10). That lets late rewrites such as `invert_guard_branch` expose DCE for a later round without changing one-shot production behavior.

The IL is **compile-time only**; the VM implements the representation-boundary and numeric-chain opcodes and archive minor 5 records them.

## Cache and rebuild

Re-run the same CLI entry without deleting `out.hyc` to reuse the cached compile. Delete `out.hyc` (or bump the archive major/minor past what this runtime accepts) to force a fresh compile. The CLI recompiles automatically when the archive is missing, corrupt, incompatible with this runtime, older than any recorded source (including `use`d modules), or was built for a different entry file.

## Multi-file programs

With a `coil.toml`, the pipeline discovers dependencies via `use` / `mod`, compiles each file with a namespace prefix into one shared IL buffer, and **lowers once** after linking. The **entry file** uses the empty namespace. See [Modules](../references/modules.md) and [Project config](../references/project-config.md).

## Opcode discipline

New `Instruction` variants are **append-only** (preserve `#[repr(u8)]` discriminants). Additive changes bump the archive **minor**; incompatible changes bump the **major** (reset minor). Loaders accept the same major when the archive minor is ≤ the runtime minor. The release VM `promise!` ceiling must track the last variant. Selected builtin-related opcodes are listed in [opcodes.md](opcodes.md).
