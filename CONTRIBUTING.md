# Contributing to coil

## Build and test

```bash
cargo clippy --workspace --lib --tests --bins -- -D warnings   # lint gate
cargo build -p coil-fmt && ./target/debug/coil-fmt --check examples tests compiler/src/prelude comptime/tests
cargo test -p <crate> <filter>                 # the tests nearest your change
./target/debug/coil test path/to/touched.coil   # .coil tests you added or changed
```

CI runs the full suites: `cargo test --workspace --lib --tests --bins`, the
`.coil` corpora on native code and on the MIR interpreter (`--engine native|mir`),
the leak smoke and GC stress (`harness-*` jobs in `.github/workflows/ci.yml`).
Push and fix what it reports rather than running them locally as a gate.

Release build and soft CPU baseline:

```bash
cargo build --release --workspace
./scripts/poop_baseline.sh
```

### Examples

Every `examples/*.coil` starts with a comment header describing the feature it
shows and ends that header with its exact stdout:

```coil
// Recursive Fibonacci.
//
// @output 2178309
```

Escape newlines as `\n` (also `\t`, `\e`, `\\`). An example that cannot run
in the test harness says why instead:
``// @output (not checked: needs `--allow-dload sum`)``. `examples_print_their_documented_output`
(`cargo test -p compiler --test pipeline`) runs every example
and compares, so a broken or drifting example fails CI.

## Where to change things

| Area | Location |
|------|----------|
| Syntax | `parser/` |
| Types / diagnostics | `compiler/src/typechecking/` |
| HIR / optimizations | `compiler/src/hir/` |
| Codegen plan / MIR | `compiler/src/codegen/`, `compiler/src/mir/` |
| Native code (Cranelift JIT / AOT) | `coil-native/` |
| MIR interpreter | `coil-mir-interp/` |
| Runtime / host natives | `machine/` |
| Archive format | `common/src/archive.rs`, `compiler/src/mir/archive.rs` |

Routing detail: [.claude/skills/coil-contributor/SKILL.md](.claude/skills/coil-contributor/SKILL.md).

## Invariants (do not break)

See [AGENTS.md](AGENTS.md). Highlights:

- Archive version (`ARCHIVE_MAJOR` / `ARCHIVE_MINOR` in `common/src/archive.rs`): an additive change bumps the minor, any other the major.
- MIR is what both engines run: a new MIR op needs native code and the MIR interpreter, and both must agree (`coil test`, `coil test --engine mir`).
- Virtual-module natives via `HostInvoke`, not new MIR ops for `io` / `thread` / etc. Frozen ids in `common/src/host.rs` are never renumbered.
- Language features need full HM integration, user-doc updates in [coil-website](https://github.com/ardax-corp/coil-website), internals updates here when needed, and a minimal runnable example.
- Method-based APIs: prefer `impl` methods on classes over free functions for type-tied operations (stdlib, new surface). See [limitations.md](docs/internals/limitations.md) for codegen gaps on free generic enum returns.

Known gaps and workarounds: [docs/internals/limitations.md](docs/internals/limitations.md).
Opt prove rule: [optimization-roadmap.md](docs/internals/optimization-roadmap.md#hit-bench-prove-rule)
(hit benches, not flagship-identical as a skip). PGO was removed; do not revive it.

## Documentation

| Tree | When to update |
|------|----------------|
| [coil-website](https://github.com/ardax-corp/coil-website) `src/content/docs/manual/` | Tutorials, getting started, examples catalog (`/docs/manual/…`) |
| [coil-website](https://github.com/ardax-corp/coil-website) `src/content/docs/language/` | Syntax, types, error codes (`/docs/language/…`). Per-API **builtin** pages are generated from the `///` stubs into `src/content/reference/` (`/docs/reference/…`) |
| `docs/internals/` | Pipeline, MIR, engines, runtime, tooling (this repo) |
| [coil-stdlib](https://github.com/ardax-corp/coil-stdlib) | Userland API docs (`docs/` in that repo); `///` on public `.coil` |

User-facing docs live in coil-website until a public domain is set.

## Commits

Granular conventional commits; stage only related files.
