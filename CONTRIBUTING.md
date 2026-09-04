# Contributing to coil

## Build and test

```bash
cargo check --workspace          # lint gate
cargo test --workspace --lib --tests --bins   # required gate (skip Criterion benches)
cargo build --bin coil && (ulimit -v 65536; ./target/debug/coil test)  # leak smoke (64MB)
```

Release build and soft CPU baseline:

```bash
cargo build --release --workspace
./scripts/poop_baseline.sh
```

## Where to change things

| Area | Location |
|------|----------|
| Syntax | `parser/` |
| Types / diagnostics | `compiler/src/typechecking/` |
| Codegen / IL | `compiler/src/codegen/`, `compiler/src/il/` |
| VM / natives | `machine/` |
| Opcodes / archive | `common/` |

Routing detail: [.cursor/skills/coil-contributor/SKILL.md](.cursor/skills/coil-contributor/SKILL.md).

## Invariants (do not break)

See [AGENTS.md](AGENTS.md). Highlights:

- Append-only opcodes — new `Instruction` variants at the end only; bump archive **minor**, VM `promise!` ceiling, and `instruction_from_u8_covers_last_appended_variant`.
- Virtual-module natives via `HostInvoke` — not new opcodes for `io` / `thread` / etc.
- Language features need full HM integration, user-doc updates in [coil-website](https://github.com/ardax-corp/coil-website), internals updates here when needed, and a minimal runnable example.
- **Method-based APIs** — prefer `impl` methods on classes over free functions for type-tied operations (stdlib, new surface). See [limitations.md](docs/internals/limitations.md) for codegen gaps on free generic enum returns.

Known gaps and workarounds: [docs/internals/limitations.md](docs/internals/limitations.md).

## Documentation

| Tree | When to update |
|------|----------------|
| [coil-website](https://github.com/ardax-corp/coil-website) `src/content/docs/manual/` | Tutorials, getting started, examples catalog (`/docs/manual/…`) |
| [coil-website](https://github.com/ardax-corp/coil-website) `src/content/docs/references/` | Syntax, per-API **builtin** pages, error codes (`/docs/references/…`) |
| `docs/internals/` | Pipeline, VM, tooling (this repo) |
| [coil-stdlib](https://github.com/ardax-corp/coil-stdlib) | Userland API docs (`docs/` in that repo); `///` on public `.hy` |

User-facing docs live in coil-website until a public domain is set.

## Commits

Granular conventional commits; stage only related files.
