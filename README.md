# coil

Statically typed scripting language with explicit generics and constraint-based inference. Source files use the `.hy` extension; compiled archives are `.hyc`.

## Install

Download a binary from [GitHub Releases](https://github.com/ardax-corp/coil-lang/releases). No tagged release is published yet; until one is, the rolling [`edge`](https://github.com/ardax-corp/coil-lang/releases/tag/edge) pre-release has the toolkit for Linux (x86_64 glibc and musl, aarch64 glibc), macOS (aarch64) and Windows (x86_64 MinGW). It is rebuilt from `main` after every green CI run, so it is a snapshot, not a stable release. `SHA256SUMS` lists the checksums.

In GitHub Actions, install the snapshot instead of building coil from source:

```yaml
- uses: ardax-corp/coil-lang/.github/actions/setup-coil@main
  with:
    stdlib: true   # optional: also clone coil-stdlib, exported as COIL_STDLIB_DIR
- run: coil test
```

With [spool](https://github.com/ardax-corp/spool), `./bootstrap.sh --channel edge --install` installs the snapshot into `~/.coil`.

Build from source:

```bash
git clone git@github.com:ardax-corp/coil-lang.git
cd coil-lang
# Optional: userland stdlib for `io::sync` and showcase projects
git clone git@github.com:ardax-corp/coil-stdlib.git ../coil-stdlib
cargo build
cargo run -- examples/fib.hy    # prints 2178309
```

## Documentation

User-facing docs live in [coil-website](https://github.com/ardax-corp/coil-website) (`src/content/docs/`) until a public domain is set. Site routes are `/docs/<path>` (for example `/docs/manual/getting-started`).

| Audience | Start here |
|----------|------------|
| Users | [Getting started](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/manual/getting-started.md) (`/docs/manual/getting-started`) |
| Language reference | [References](https://github.com/ardax-corp/coil-website/tree/main/src/content/docs/references) (`/docs/references`) |
| Contributors | [AGENTS.md](AGENTS.md) · [CONTRIBUTING.md](CONTRIBUTING.md) · [docs/internals/](docs/internals/README.md) |
| Userland stdlib | [coil-stdlib docs](https://github.com/ardax-corp/coil-stdlib/blob/main/docs/README.md) |
| HTTP client/server | [coil-http](https://github.com/ardax-corp/coil-http) via [spool](https://github.com/ardax-corp/spool) |

## Features

Primitives (`int`, `float`, `string`, `bool`, `byte`), scalar-backed enums and `match`, denser `Option`/`Result` ABIs (pointer niches and two-slot direct CALL), records and dicts, generics and traits, classes, coroutines, `for x in`, ranges, FFI, non-blocking IO with sync adapters, OS threads, and a userland stdlib (`collections`, `text`, `bytes`, …). HTTP is [coil-http](https://github.com/ardax-corp/coil-http) via spool.

## Repository layout

```
coil/
├── common/     # Opcodes, values, archive format
├── parser/     # Pratt parser, AST
├── compiler/   # HM typechecker, stack IL codegen, pipeline
├── machine/    # VM, heap/GC, FFI, host natives
├── coil-*/     # CLI helpers (debug, dissect, fmt, lsp) and coil-embed (packaged-app runner, not an embed library)
├── examples/   # Runnable demos
├── tests/      # Integration tests (`coil test`)
└── docs/       # Contributor internals
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Agent-oriented invariants live in [AGENTS.md](AGENTS.md).
