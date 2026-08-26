# coil — AGENTS

coil: statically typed `.hy` → stack IL → `.hyc` archive → custom VM.

| Need | Read |
|------|------|
| Write / edit `.hy` | `.cursor/skills/coil-language` · `docs/manual/` · `docs/references/` |
| Compiler, VM, pipeline | `.cursor/skills/coil-contributor` · `docs/internals/` |
| Hangs, panics, breakpoints | `.cursor/skills/coil-debug` · `docs/internals/debugger.md` |
| Known gaps / workarounds | `docs/internals/limitations.md` |

## User preferences

- Tests: `cargo test --workspace --lib --tests --bins` (required gate; covers integration tests, skips Criterion benches). Bare optional stack: `cargo test --workspace --lib --tests --bins --no-default-features` (feature-dependent tests are `cfg`-skipped). CI still compile-gates each of `crypto`/`time` with `cargo check --workspace --lib --tests --bins --no-default-features --features <name>` (`crypto` is an empty leftover Cargo feature; virtual crypto is gone). Tooling: `--features <dissect|debugger>` with full test. Leak smoke: `cargo build --bin coil && (ulimit -v 65536; ./target/debug/coil test)`. Soft CPU: `./scripts/poop_baseline.sh`.
- Large tasks: scoped sub-agents on disjoint modules.
- VM perf: alloc reduction, hot-loop tuning, bounds-check elimination, `promise!` — not benchmark-shaped opcodes unless universal.
- Language features: draft plans; full HM; update `docs/`; minimal runnable example.
- **Method-based APIs** — prefer inherent/`impl` methods over free functions for type-tied operations (stdlib, new language surface, codegen fixes). Free generic fns returning enums are fragile today; see `docs/internals/limitations.md`.
- Granular conventional commits; stage only related files.
- Prefer compiler virtual modules over userland for core interpreter machinery; extracted features (regex, TLS, HTTP, collections) live in separate repos (`ardax-corp/coil-regex`, `coil-tls`, `coil-http`, `coil-stdlib`).
- **Userland package tests** — demos, native builds, and integration tests for extracted packages stay in their repos, not coil-lang `compiler/tests` or CI.
- **VM vs `.hy` tests** — prefer `.hy` language tests (`tests/positive/`, `coil test`) over Rust VM bytecode tests when coverage overlaps; remove duplicates.
- `cargo build` builds `coil` + `coil-debug` / `coil-dissect` / `coil-fmt` / `coil-lsp` / `coil-embed` (`coil package` defaults to embed).
- IL inspection: `coil dissect` — no verbose debug-build dumps.
- `coil fmt`: preserve `//` and `///`; wrap long lines; trailing commas on multi-line lists.

## Invariants (do not break)

- **Append-only opcodes** (`common/src/opcode.rs`). New variants at end → bump archive **minor**, `promise!` in `machine/src/vm.rs`, `instruction_from_u8_covers_last_appended_variant`. ABI break → **major** (reset minor).
- **Virtual-module natives** via `HostInvoke` — host wiring in `machine/`. Removing natives drops their slots entirely and bumps archive minor (no reserved panic stubs).
- **Feature gates**: debugger `feature = "debugger"`; dissect `feature = "dissect"` on helper binaries, not default `coil`.
- **Lint gate**: `cargo check --workspace` (not clippy — `Gc::payload_mut` deny).

Codegen / match / `STORE`: `.cursor/skills/coil-contributor/reference.md`. Pipeline: `docs/internals/pipeline.md`.

## Cloud agents

Pre-installed: `poop`, `valgrind`, `heaptrack`, `hyperfine`, `lua` (`.cursor/Dockerfile`). Use `--release` for benchmarks.
