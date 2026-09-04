# coil — AGENTS

coil: statically typed `.hy` → stack IL → `.hyc` archive → custom VM.

| Need | Read |
|------|------|
| Write / edit `.hy` | `.cursor/skills/coil-language` · [coil-website](https://github.com/ardax-corp/coil-website) `src/content/docs/` (`/docs/…`) |
| Compiler, VM, pipeline | `.cursor/skills/coil-contributor` · `docs/internals/` |
| Hangs, panics, breakpoints | `.cursor/skills/coil-debug` · `docs/internals/debugger.md` |
| Known gaps / workarounds | `docs/internals/limitations.md` |

## User preferences

- Tests: `cargo test --workspace --lib --tests --bins` (required gate; covers integration tests, skips Criterion benches). Bare optional stack: `cargo test --workspace --lib --tests --bins --no-default-features`. Tooling: `--features <dissect|debugger>` with full test. Leak smoke: `cargo build --bin coil && (ulimit -v 65536; ./target/debug/coil test)`. Soft CPU: `./scripts/poop_baseline.sh`.
- Large tasks: scoped sub-agents on disjoint modules.
- VM perf: alloc reduction, hot-loop tuning, bounds-check elimination, `promise!` — not benchmark-shaped opcodes unless universal.
- Language features: draft plans; full HM; update coil-website user docs (`src/content/docs/`) and `docs/internals/` here when needed; minimal runnable example.
- **Method-based APIs** — prefer inherent/`impl` methods over free functions for type-tied operations (stdlib, new language surface, codegen fixes). Free generic fns returning enums are fragile today; see `docs/internals/limitations.md`.
- Granular conventional commits; stage only related files.
- Prefer compiler virtual modules over userland for core interpreter machinery; extracted features (regex, TLS, HTTP, collections) live in separate repos (`ardax-corp/coil-regex`, `coil-tls`, `coil-http`, `coil-stdlib`).
- **Userland package tests** — demos, native builds, and integration tests for extracted packages stay in their repos, not coil-lang `compiler/tests` or CI.
- **VM vs `.hy` tests** — prefer `.hy` language tests (`tests/positive/`, `coil test`) over Rust VM bytecode tests when coverage overlaps; remove duplicates.
- `cargo build` builds `coil` + `coil-debug` / `coil-dissect` / `coil-fmt` / `coil-lsp` / `coil-embed`. `coil-embed` is the packaged-app runner (`coil package` prefers it); not an embed-the-VM library.
- IL inspection: `coil dissect` — no verbose debug-build dumps.
- `coil fmt`: preserve `//` and `///`; wrap long lines; trailing commas on multi-line lists.

## Invariants (do not break)

- **Append-only opcodes** (`common/src/opcode.rs`). New variants at end → bump archive **minor**, `promise!` in `machine/src/vm.rs`, `instruction_from_u8_covers_last_appended_variant`. ABI break → **major** (reset minor).
- **Virtual-module natives** via `HostInvoke` — host wiring in `machine/`. Leftover TLS/crypto/regex slots were dropped (holes collapse, archive **minor** bump); they are not reserved panic stubs. Virtual-time names stay as panic stubs so later ids do not move. `stream_attach` / `stream_park` own **119** / **120**. Process clocks append after that: `clock_wall_nanos` / `clock_mono_nanos` / `clock_sleep_ms` are **121** / **122** / **123** (`use clock::{…}`).
- **Feature gates**: debugger `feature = "debugger"`; dissect `feature = "dissect"` on helper binaries, not default `coil`.
- **Lint gate**: `cargo check --workspace` (not clippy — `Gc::payload_mut` deny).
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

- Regularity target: one ground-call convention, panic on OOB (not `-1` / no-op); `a[i]` stays type `T`. `Option`/`Result` use niche / two-slot / boxed by shape (COI-92), not one heap ABI. Fuse opcodes are debt — rewrite existing ops rather than growing bench-shaped fuses.
