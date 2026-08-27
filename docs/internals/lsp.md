# Coil language server

`coil lsp` starts `coil-lsp`, a sibling helper that speaks the Language Server
Protocol over standard input and output. Build it with `cargo build`; the
resulting `coil-lsp` binary is placed beside `coil`.

The initial server supports:

- full-document synchronization and published parse/type diagnostics;
- whole-document formatting;
- whole-document range formatting requests;
- document and workspace symbols;
- hover for inferred types;
- keyword and source-name completion;
- signature help;
- folding and selection ranges;
- document highlights and references;
- same-workspace definition lookup and rename;
- syntactic semantic tokens (lexical comments/strings/numbers/operators plus
  AST-based declaration/reference classification via `SymbolIndex`, with
  reference sites resolved through `Checker::lookup_for_codegen_span`).

The server is deliberately synchronous and uses `lsp-server` with
`lsp-types`. It keeps open documents in memory and does not write editor
buffers to disk.

Completion items include declaration kinds, inferred type details, and
`///` docs when available. Function documentation includes a Parameters
section for documented parameters, and hovering a parameter shows its own
type and documentation. Mid-edit buffers that fail to parse fall back to
the last successful analysis (or a sanitized parse) so incomplete identifiers
still surface functions and locals.

Virtual-module imports use the compiler's `VirtualModules` registry as a
documentation-stub source. Imported functions, types, and implicit prelude
exports receive completion and hover documentation with links to the matching
reference page in [coil-website](https://github.com/ardax-corp/coil-website) (`src/content/docs/`; site route `/docs/…`).

Function completion items use snippet insertion: selecting `fib` inserts
parameter placeholders such as `fib(${1:n})$0` and advertises `(` / `,` as
signature-help triggers.

## Deferred semantic work

Project diagnostics use `Pipeline` overlays and its typecheck-only project
path. Navigation indexes are intentionally conservative: unresolved imports,
type definitions, implementations, and cross-project references require richer
compiler symbol resolution.

The reporting crate exposes byte-to-LSP UTF-16 position conversion so future
clients can share the same source-location behavior as the existing LSP
diagnostic sink.

## Editor configuration

Point an editor's Coil language client at `coil-lsp` and use `coil` as the
language identifier. The server requires no command-line arguments; `--stdio`
is accepted for compatibility with editor launch configurations.
