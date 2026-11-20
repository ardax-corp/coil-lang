# coil contributor reference

## Key files

| File | Role |
|------|------|
| `common/src/archive.rs` | Packed `ARCHIVE_MAJOR`/`ARCHIVE_MINOR`, `ArchivedProgram` |
| `common/src/startup.rs` | `StartupOp`: what runs before `main` |
| `common/src/host.rs` | Host native table, frozen `*_ID` consts |
| `machine/src/driver.rs` | `Driver`: runs a program over a `Runtime`, answers native code's host requests |
| `machine/src/native.rs` | `NativeCtx`, the entry table, `coil_native_*` helpers |
| `coil-native/src/lib.rs` | MIR to Cranelift, JIT and AOT |
| `coil-mir-interp/src/` | The interpreter: `lib.rs` (`prepare`), `exec.rs` (the loop), `frame.rs` (activations, roots) |
| `compiler/src/codegen/` | Codegen driver (`Compiler`, `do_compile`, `finalize_bodies`: tree-shake, the no-MIR check, body identities) |
| `compiler/src/lib.rs` | Crate root: re-exports, `#[macro_use] mod codegen` |
| `compiler/src/hir/` | HIR build, typed inliner, optimization passes (fold, bounds, unroll, sroa, sink_return; CSE and LICM live in `compiler/src/mir/`) |
| `compiler/src/codegen/emit_hir.rs` | Per-body layout / dictionary / call plan handed to MIR lowering |
| `compiler/src/mir/` | `from_hir` (HIR to SSA for every body), MIR passes, `archive.rs` (MIR on disk), `native_capture.rs` (bodies to engines, call folding), `frame_words.rs` (what both engines need about frame words) |
| `compiler/src/bodies.rs` | The bodies of a compile, `Label` |
| `compiler/src/opt/` | `OptimizeOptions`, `OptLevel`, `--opt-stats` counters |
| `compiler/src/typechecking/infer/` | HM `Checker` (`mod.rs` types + `checker.rs` impl) |
| `parser/src/` | Pratt parser |
| `machine/src/packed_la.rs` | LA ops via HostInvoke; uses `coil-simd` |
| `coil-simd/` | Stable `std::arch` SIMD (dot/matmul/zip, `bytes::eq`) |

## Codegen invariants

- **Type aliases**: scoped; same-frame duplicates are errors; inner may shadow outer.
- Packed LA (`dot`/`matmul`/`Matrix`) → `HostInvoke` in `machine/src/packed_la.rs`; kernels go through `coil-simd`.

## Codegen notes

- **Method-based APIs** — prefer inherent/`impl` methods over free functions for type-tied ops (stdlib, new language surface). Free generic fns returning enums are codegen-fragile; see `docs/internals/limitations.md`.
- `hir::fold`: scalar const folding and algebraic identities; `if` on a literal keeps one branch; `hir::unroll`: full unroll of counted loops ≤8 trips.
- Typed inlining in HIR (`hir::inline`); tail calls for eligible self- and sibling-cycle recursion ([#316](https://github.com/ardax-corp/coil-lang/pull/316); `MirFunc::tail_targets`, `is_tail_call_inst`; both engines take the frame: Cranelift `return_call`, the interpreter reuses its activation). Two-slot `?` flatten is codegen ([#307](https://github.com/ardax-corp/coil-lang/pull/307)), not InstCombine.
- Match: `hir::match_tree` groups rows by outermost constructor, so a nested match tests the scrutinee's tag once and then only the sub-patterns of that tag's rows. MIR reads a payload with `MatchPayload`. Nested niche `Option`/`Result` words are tested by layout (`0` / bit 0).
- Records and typed class fields: `ProductLoad` / `HeapFieldStore` with a slot operand (`ProductLoad` also reads a tuple's item at a constant index and a boxed enum's payload word) (a record's slots are its fields sorted by name; see `docs/internals/mir.md` Records).
- **Return layout:** `layout::abi::two_word_kind` classifies two-word returns on *direct* calls (`Option<int>` / immediate-Ok `Result` / arity-≤1 user payload enums as `[payload, tag]`; arity-2 immediate products as `[a, b]`). In MIR the second word is `Terminator::Return { hi }` and a `Call`'s `dest_hi`. Niches stay one word. See [layout.md](../../../docs/internals/layout.md).
- **HostInvoke enum layout:** `0` boxed `ObjEnum`, `1` Option pointer-niche / `Result<(), E>` (heap `E`), `2` heap-heap Result. Pack once via `machine::host_enum`.

## Runtime / values

- Statics: `LoadStatic` / `StoreStatic` MIR ops; `static_slot_count` in the archive.
- Program start: `common::StartupOp` list plus `main`, walked by `Driver::run_startup` / `run_program`. A body's identity is `label.0 + 1`.
- Generators: `Alloc` of `MirAllocKind::Coro`, `ResumeCoro`, `Yield`, `DoneCoro`.
- `panic` aborts the run; the test harness treats it as failure.
- Panic locations: `ProgramDebug::line_of` over `debug_locs` / `debug_lines`.
- Roots: Cranelift stack maps and `NativeCtx::roots` records on native code, one `ROOTS_SCAN` record per activation on the interpreter. A word lives until its last use, a `gc::root` handle until the function returns.

## Typechecker limitations (known)

See [docs/internals/limitations.md](docs/internals/limitations.md) for the full inventory. Summary:

- `codegen_var_types` side table still used for some Identifier paths.
- Path completeness (`E0111`) for concrete non-unit returns on named fns.
- Unreachable code `E0118`; defer in infinite loop `E0123`.
- Unit/open-var fall-through may return a zero word (Result-mode Ok-wraps unit only).

## Archive version bump triggers

`ArchivedProgram::version` is packed `major.minor` (`u16`/`u16` in a `u32`).

Load rule: same major and archive minor ≤ runtime minor (older archives run on newer minor runtimes; majors never cross-load).

| Change | Bump |
|--------|------|
| Additive optional archive data old archives lack | **minor** |
| Any other change to `ArchivedProgram` | **major** (reset minor) |
| A change to the MIR section's layout | `FORMAT` in `compiler/src/mir/archive.rs` |

Current version: check `ARCHIVE_MAJOR` / `ARCHIVE_MINOR` in `common/src/archive.rs`.

## Perf philosophy

Prefer:
- Allocation reduction in hot paths
- Bounds-check elimination
- Hot-loop tuning in the runtime and in what MIR passes leave

Soft baseline: `./scripts/poop_baseline.sh` (compile once, then `coil run` archives under `examples/perf/`, JIT included). Build flagships with `--fold-calls off`. Hit-bench prove rule: [optimization-roadmap.md](../../../docs/internals/optimization-roadmap.md#hit-bench-prove-rule). PGO was removed ([#301](https://github.com/ardax-corp/coil-lang/pull/301)). See AGENTS.md user preferences.

Older archive versions are in [archive-history.md](../../../docs/internals/archive-history.md).

| Tree | When to update |
|------|----------------|
| [coil-website](https://github.com/ardax-corp/coil-website) `src/content/docs/manual/` | Tutorials, getting started, examples catalog (`/docs/manual/…`) |
| [coil-website](https://github.com/ardax-corp/coil-website) `src/content/docs/language/` | Syntax, types, error codes (`/docs/language/…`). Per-API builtin pages are generated from the `///` stubs into `src/content/reference/` (`/docs/reference/…`) |
| `docs/internals/` | Pipeline, MIR, engines, debugger (contributor-facing) |

New stable diagnostics: add to coil-website `src/content/docs/language/error-codes.md`.
