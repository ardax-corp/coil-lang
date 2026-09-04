# coil contributor reference

## Key files

| File | Role |
|------|------|
| `common/src/opcode.rs` | `Instruction` enum — append-only |
| `common/src/archive.rs` | Packed `ARCHIVE_MAJOR`/`ARCHIVE_MINOR`, archive envelope |
| `machine/src/vm.rs` | Main dispatch loop, `promise!` opcode ceiling |
| `compiler/src/codegen/` | Codegen driver (`Compiler`, `finalize_bytecode`, `do_compile`) |
| `compiler/src/lib.rs` | Crate root: re-exports, `#[macro_use] mod codegen` |
| `compiler/src/il/` | IL ops, lower, fuse-select, opts |
| `compiler/src/typechecking/infer/` | HM `Checker` (`mod.rs` types + `checker.rs` impl) |
| `parser/src/` | Pratt parser |
| `machine/src/packed_la.rs` | LA ops via HostInvoke (no LA opcodes); uses `coil-simd` |
| `coil-simd/` | Stable `std::arch` SIMD (dot/matmul/zip, `bytes::eq`) |

## Codegen / VM invariants

- **`STORE`**: pops TOS into slot(s); `cursor = max(cursor, slot + 1)`. Match bindings skip store (`UNPACK` / `JUMP_IF_MATCH`). `StorePop` is deprecated alias — compiler never emits. Packed multi-slot `LOAD`/`STORE`: `[31:24]=n` (1..=3), three slot bytes; `n==0` → wide single slot in low 24 bits.
- **Stack IL**: symbolic labels until `finalize_bytecode` → per-body opts + **single** `il::lower` after concat. Nested fused returns must `capture_nested_return`.
- **Type aliases**: scoped; same-frame duplicates are errors; inner may shadow outer.
- Packed LA (`dot`/`matmul`/`Matrix`) → `HostInvoke` in `machine/src/packed_la.rs` (no LA opcodes); kernels go through `coil-simd`.

## Codegen notes

- `BlockBuilder`: thin wrapper over `IlBuilder` labels; no absolute PC patching in emitters.
- **Method-based APIs** — prefer inherent/`impl` methods over free functions for type-tied ops (stdlib, new language surface). Free generic fns returning enums are codegen-fragile; see `docs/internals/limitations.md`.
- `ConstEnv`: scalar const folding; constant branch/loop elimination; loop unroll ≤8.
- Tiny direct-call inlining; one-level self-`CALL` peel; `TailCall` for eligible recursion.
- Match: threaded layout, `JumpIfMatch`, nested records use `UnpackAt` (slot-based).
- Enum fields: `LoadField` (index); typed class fields: `LoadField` / `SetField` with slot operand; dict fields: `GetField`/`SetField` (interned names).
- **Return layout:** `typechecking::return_layout::two_word_return_enum` classifies two-slot `[payload, tag]` on *direct* `CALL`/`RETURN` (`Option<int>` / immediate-Ok `Result` / arity-≤1 user payload enums). `CALL` bit 31 / `RETURN` operand `2`. Niches stay one word. See [limitations.md](docs/internals/limitations.md) COI-92.
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

Prefer over new opcodes / IL opts:
- Allocation reduction in hot paths
- Bounds-check elimination
- Hot-loop tuning in VM
- `promise!` for release assertions

Soft baseline: `./scripts/poop_baseline.sh` (compile once, then `coil run` archives under `examples/perf/`). See AGENTS.md user preferences.

| Tree | When to update |
|------|----------------|
| [coil-website](https://github.com/ardax-corp/coil-website) `src/content/docs/manual/` | Tutorials, getting started, examples catalog (`/docs/manual/…`) |
| [coil-website](https://github.com/ardax-corp/coil-website) `src/content/docs/references/` | Syntax, per-API builtin pages, error codes (`/docs/references/…`) |
| `docs/internals/` | Pipeline, opcodes, debugger (contributor-facing) |

New stable diagnostics: add to coil-website `src/content/docs/references/error-codes.md`.
