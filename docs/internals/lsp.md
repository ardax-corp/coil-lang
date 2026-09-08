# Coil language server

`coil lsp` starts `coil-lsp`, a sibling helper that speaks the Language Server
Protocol over standard input and output. Build it with `cargo build`; the
resulting `coil-lsp` binary is placed beside `coil`.

The server is deliberately synchronous and uses `lsp-server` with
`lsp-types`. It keeps open documents in memory and does not write editor
buffers to disk.

## Supported

These requests are implemented and covered by `coil-lsp` scenario tests:

| Capability | Behavior |
|---|---|
| Full-document sync | `didOpen` / `didChange` / `didClose`; published parse and type diagnostics |
| Project overlays | Unsaved buffers overlay disk via `Pipeline::set_file_text`; `ProjectIndex` is refreshed on each change |
| Multi-file navigation | Goto-definition uses the use-graph `ProjectIndex` and can land in **unopened** imported `.hy` files |
| Hover | Inferred types, `///` docs, parameter docs, virtual-module stubs |
| Completion | Keywords, decls, inferred types, function snippets, mid-edit sanitize / last-good fallback; `Enum.Case` after `.`; virtual exports after `::` |
| Signature help | Local `fn` parameter lists; imported / virtual names from the completion index |
| Format | Whole-document `coil fmt` |
| Range format | Reformats **overlapping top-level items** only (not a token-precise rustfmt range) |
| Document / workspace symbols | Top-level decls; workspace symbols include indexed use-graph files |
| Highlights / references / rename | `SymbolIndex` identifier sites — not substring hits in comments or strings |
| Folding | One fold per top-level document symbol that spans lines |
| Selection ranges | Nested AST spans containing the cursor (full file if parse fails) |
| Semantic tokens | Lexical comments/strings/numbers/operators plus AST / `Checker` classification |

Search roots for an opened workspace are `src`, `.` (sibling files at the
project root), and each `.deps/*/src` checkout when present. That matches
typical package + spool layouts. Language `use`/`mod` still does **not**
read `[module].roots` from `coil.toml` (same rule as `coil` without
`--root`).

Virtual-module imports use the compiler's `VirtualModules` registry.
Imported functions, types, and implicit prelude exports get completion and
hover text with links into [coil-website](https://github.com/ardax-corp/coil-website)
(`src/content/docs/`; site route `/docs/…`). Process clocks are virtual
`clock` (`wall_nanos` / `mono_nanos` / `sleep_ms` → HostInvoke `clock_*`).
There is no virtual `time` / `regex` / `tls` / `crypto` module; those live
in userland packages. `ffi::dload` / `declare` / `invoke` are documented as
HostInvoke FFI stubs.

Function completion items use snippet insertion: selecting `fib` inserts
parameter placeholders such as `fib(${1:n})$0` and advertises `(` / `,` as
signature-help triggers.

## Deferred

Leave these for a later LSP pass unless they block daily editing:

- `textDocument/typeDefinition` is advertised but shares the definition
  handler (Coil has no separate type-def table).
- Cross-project references that are not on the current use-graph.
- Implementation / trait-item navigation and field completion from inferred
  receiver types.
- `coil.toml` `[module].roots` (compiler language path ignores it; pass
  `--root` on CLI).
- Incremental (`TextDocumentSyncKind::INCREMENTAL`) edits.
- Rename prepare / invalid-identifier rejection.
- Semantic token modifiers (`declaration`, `readonly`, …).

The reporting crate exposes byte-to-LSP UTF-16 position conversion so other
tools can share the same source-location behavior as this server.

## Editor configuration

Point an editor's Coil language client at `coil-lsp` and use `coil` as the
language identifier. The server requires no command-line arguments; `--stdio`
is accepted for compatibility with editor launch configurations.
