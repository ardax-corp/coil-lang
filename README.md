# coil

Statically typed scripting language with explicit generics and constraint-based inference. Source files use the `.coil` extension; compiled archives are `.ccoil`.

Documentation, the tutorial and the language reference live at [coil-lang.dev](https://coil-lang.dev).

## Install

Download a binary from [GitHub Releases](https://github.com/ardax-corp/coil-lang/releases). No tagged release is published yet; until one is, the rolling [`edge`](https://github.com/ardax-corp/coil-lang/releases/tag/edge) pre-release has the toolkit for Linux (x86_64 glibc and musl, aarch64 glibc), macOS (aarch64) and Windows (x86_64 MinGW). It is rebuilt from `main` after every green CI run, so it is a snapshot, not a stable release. `SHA256SUMS` lists the checksums.

In GitHub Actions, install the snapshot instead of building coil from source:

```yaml
- uses: ardax-corp/coil-lang/.github/actions/setup-coil@main
  id: coil
  with:
    stdlib: true   # optional: also clone coil-stdlib
- run: coil test --root "${{ steps.coil.outputs.stdlib }}"
```

`coil` reads no environment variable for library roots: pass each one with `--root`.

With [spool](https://github.com/ardax-corp/spool), `./bootstrap.sh --channel edge --install` installs the snapshot into `~/.coil`.

Build from source:

```bash
git clone git@github.com:ardax-corp/coil-lang.git
cd coil-lang
# Optional: userland stdlib for `io::sync` and showcase projects
git clone git@github.com:ardax-corp/coil-stdlib.git ../coil-stdlib
cargo build
cargo run -- examples/fib.coil    # prints 2178309
```

## Documentation

Live docs site: https://coil-website-jet.vercel.app

User-facing docs live in [coil-website](https://github.com/ardax-corp/coil-website) (`src/content/docs/`) until a public domain is set. Site routes are `/docs/<path>` (for example `/docs/manual/getting-started`).

| Audience | Start here |
|----------|------------|
| Users | [Getting started](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/manual/getting-started.md) (`/docs/manual/getting-started`) |
| Language reference | [Language guide](https://github.com/ardax-corp/coil-website/tree/main/src/content/docs/language) (`/docs/language/…`) · [API reference](https://github.com/ardax-corp/coil-website/tree/main/src/content/reference) (`/docs/reference`) |
| Contributors | [AGENTS.md](AGENTS.md) · [CONTRIBUTING.md](CONTRIBUTING.md) · [docs/internals/](docs/internals/README.md) |
| Userland stdlib | [coil-stdlib docs](https://github.com/ardax-corp/coil-stdlib/blob/main/docs/README.md) |
| HTTP client/server | [coil-http](https://github.com/ardax-corp/coil-http) via [spool](https://github.com/ardax-corp/spool) |

## Features

Primitives (`int`, `float`, `string`, `bool`, `byte`), scalar-backed enums and `match`, denser `Option`/`Result` ABIs (pointer niches and two-word direct calls), records and dicts, generics and traits, classes, coroutines, `for x in`, ranges, FFI, non-blocking IO with sync adapters, OS threads, and a userland stdlib (`collections`, `text`, `bytes`, …). HTTP is [coil-http](https://github.com/ardax-corp/coil-http) via spool.

## Repository layout

```
coil/
├── common/          # Values, archive format, host native table
├── parser/          # Pratt parser, AST
├── compiler/        # HM typechecker, HIR optimizations, HIR → MIR, MIR passes, pipeline
├── machine/         # Runtime: driver, heap/GC, tasks, reactor, FFI, host natives
├── coil-native/     # MIR → Cranelift: JIT and AOT executables
├── coil-mir-interp/ # MIR interpreter (comptime, macros, debugger, wasm)
├── coil-rt/         # Runtime library AOT executables link
├── coil-*/          # CLI helpers (debug, dissect, fmt, lsp, test) and coil-embed (JIT runner for packaged apps)
├── examples/   # Runnable demos
├── tests/      # Integration tests (`coil test`)
└── docs/       # Contributor internals
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Agent-oriented invariants live in [AGENTS.md](AGENTS.md).
