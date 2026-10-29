# coil contributor reference

## Key files

| File | Role |
|------|------|
| `common/src/opcode.rs` | `Instruction` enum — append-only |
| `common/src/archive.rs` | Packed `ARCHIVE_MAJOR`/`ARCHIVE_MINOR`, archive envelope |
| `machine/src/vm.rs` | Main dispatch loop, `promise!` opcode ceiling |
| `compiler/src/codegen/` | Codegen driver (`Compiler`, `finalize_bytecode`, `do_compile`) |
| `compiler/src/lib.rs` | Crate root: re-exports, `#[macro_use] mod codegen` |
| `compiler/src/hir/` | HIR build, typed inliner, optimization passes (fold, bounds, unroll, sroa, sink_return; CSE and LICM live in `compiler/src/mir/`) |
| `compiler/src/codegen/emit_hir.rs` | Per-body layout / dictionary / call plan handed to MIR lowering (emits no instructions) |
| `compiler/src/mir/` | `from_hir` (HIR → SSA for every body), MIR passes, dense emit, LIR emit (`emit_lir`) |
| `compiler/src/il/` | IL ops, tier selection per body (`module.rs`), lower, fuse-select (no optimization passes) |
| `compiler/src/typechecking/infer/` | HM `Checker` (`mod.rs` types + `checker.rs` impl) |
| `parser/src/` | Pratt parser |
| `machine/src/packed_la.rs` | LA ops via HostInvoke (no LA opcodes); uses `coil-simd` |
| `coil-simd/` | Stable `std::arch` SIMD (dot/matmul/zip, `bytes::eq`) |

## Codegen / VM invariants

- **`STORE`**: pops TOS into slot(s); `cursor = max(cursor, slot + 1)`. Match bindings skip store (`UNPACK` / `JUMP_IF_MATCH`). `StorePop` is deprecated alias — compiler never emits. Packed multi-slot `LOAD`/`STORE`: `[31:24]=n` (1..=3), three slot bytes; `n==0` → wide single slot in low 24 bits.
- **Stack IL**: the encoding of what dense / LIR emit from MIR; symbolic labels until `finalize_bytecode` → per-body tier pick + **single** `il::lower` after concat.
- **Type aliases**: scoped; same-frame duplicates are errors; inner may shadow outer.
- Packed LA (`dot`/`matmul`/`Matrix`) → `HostInvoke` in `machine/src/packed_la.rs` (no LA opcodes); kernels go through `coil-simd`.

## Codegen notes

- `BlockBuilder`: thin wrapper over `IlBuilder` labels; no absolute PC patching in emitters.
- **Method-based APIs** — prefer inherent/`impl` methods over free functions for type-tied ops (stdlib, new language surface). Free generic fns returning enums are codegen-fragile; see `docs/internals/limitations.md`.
- `hir::fold`: scalar const folding and algebraic identities; `if` on a literal keeps one branch; `hir::unroll`: full unroll of counted loops ≤8 trips.
- Typed inlining in HIR (`hir::inline`); `TailCall` for eligible self- and sibling-cycle recursion ([#316](https://github.com/ardax-corp/coil-lang/pull/316)). Two-slot `?` flatten is codegen ([#307](https://github.com/ardax-corp/coil-lang/pull/307)), not InstCombine.
- Match: arms without nested tests dispatch on the outer tag (`JumpIfMatch`, last arm `Unpack`); payload field `k` binds at `payload_base + k`. A match with a literal or constructor nested in a payload lowers arm by arm (`compile_match_sequential`): scrutinee in a slot, per-arm tests that push opened payloads above it, misses pop back and fall into the next arm. Nested niche `Option`/`Result` words are tested by layout (`0` / bit 0), not `JumpIfMatch`.
- Enum fields: `LoadField` (index); typed class fields and record fields: `LoadField` / `SetField` with slot operand (a record's slots are its fields sorted by name; see `docs/internals/mir.md` Records).
- **Return layout:** `typechecking::return_layout::two_word_kind` classifies two-slot returns on *direct* `CALL`/`RETURN` (`Option<int>` / immediate-Ok `Result` / arity-≤1 user payload enums as `[payload, tag]`; arity-2 immediate products as `[a, b]`). `CALL` bit 31 / `RETURN` operand `2`. Niches stay one word. See [limitations.md](docs/internals/limitations.md) COI-92.
- **Repr requests (`ReprCtx`):** a consumer that wants one expression as a `[payload, tag]` pair or forced niche/heap raises `self.repr` (`unbox_enum_context`, `force_*`) right before compiling it. `do_compile` hands that to the node (`repr_here`, read via `repr_now()`) and resets it for the node's operands. Only `Group` / `Expr` / one-item `Fragment`, a block's tail, and match arm bodies (not the scrutinee) forward it. Never read the raw `self.repr.*` fields to decide a node's own layout.
- **HostInvoke enum bits `[17:16]`:** `0` boxed `ObjEnum`, `1` Option pointer-niche / `Result<(), E>` (heap `E`), `2` heap-heap Result. Pack once via `machine::host_enum`.

## VM / values

- Static slots: `LoadStatic`/`StoreStatic`; count in archive envelope.
- Coroutines: `MakeCoro`, `ResumeCoro`, `YieldCoro`, `YieldFromCoro`, `DoneCoro`.
- `panic` aborts VM; test harness treats as failure.
- GC: addr index O(1) lookup; mark walks intrusive list.

## Typechecker limitations (known)

See [docs/internals/limitations.md](docs/internals/limitations.md) for the full inventory. Summary:

- `codegen_var_types` side table still used for some Identifier paths.
- Path completeness (`E0111`) for concrete non-unit returns on named fns.
- Unreachable code `E0118`; defer in infinite loop `E0123`.
- Unit/open-var fall-through may emit `CONST 0; RETURN` (Result-mode Ok-wraps unit only).

## Archive version bump triggers

`ArchivedProgram::version` is packed `major.minor` (`u16`/`u16` in a `u32`).

Load rule: same major and archive minor ≤ runtime minor (older archives run on newer minor runtimes; majors never cross-load).

| Change | Bump |
|--------|------|
| New append-only opcode discriminants | **minor** |
| Additive optional archive data old archives lack | **minor** |
| Incompatible bytecode encoding / `Byte` layout | **major** (reset minor) |
| Tag layout changes | **major** |
| Required envelope field changes | **major** |

Current version: check `ARCHIVE_MAJOR` / `ARCHIVE_MINOR` in `common/src/archive.rs`.

## Perf philosophy

Prefer over new opcodes / optimization passes:
- Allocation reduction in hot paths
- Bounds-check elimination
- Hot-loop tuning in VM
- `promise!` for release assertions

Soft baseline: `./scripts/poop_baseline.sh` (compile once, then `coil run` archives under `examples/perf/`). Hit-bench prove rule: [optimization-roadmap.md](../../../docs/internals/optimization-roadmap.md#hit-bench-prove-rule). PGO was removed ([#301](https://github.com/ardax-corp/coil-lang/pull/301)). See AGENTS.md user preferences.

Current archive: read `ARCHIVE_MAJOR` / `ARCHIVE_MINOR` in `common/src/archive.rs`; older minors are in [archive-history.md](../../../docs/internals/archive-history.md).

| Tree | When to update |
|------|----------------|
| [coil-website](https://github.com/ardax-corp/coil-website) `src/content/docs/manual/` | Tutorials, getting started, examples catalog (`/docs/manual/…`) |
| [coil-website](https://github.com/ardax-corp/coil-website) `src/content/docs/references/` | Syntax, per-API builtin pages, error codes (`/docs/references/…`) |
| `docs/internals/` | Pipeline, opcodes, debugger (contributor-facing) |

New stable diagnostics: add to coil-website `src/content/docs/references/error-codes.md`.
