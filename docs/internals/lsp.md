# Coil language server

`coil lsp` starts `coil-lsp`, a sibling helper that speaks the Language Server
Protocol over standard input and output. Build it with `cargo build`; the
resulting `coil-lsp` binary is placed beside `coil`.

The server is deliberately synchronous and uses `lsp-server` with
`lsp-types`. It keeps open documents in memory and does not write editor
buffers to disk.

Before handling a message the main loop reads ahead everything already
queued. `$/cancelRequest` for a queued request answers it with
`RequestCanceled` instead of running it. Edits apply at once, but the
re-analysis (typecheck + diagnostics) runs once the queue holds no more
work, or before the next request or non-edit notification. A burst of
keystrokes costs one typecheck.

## Supported

These requests are implemented and covered by `coil-lsp` scenario tests:

| Capability | Behavior |
|---|---|
| Incremental sync | `didOpen` / `didChange` (ranged UTF-16 splices, or whole text) / `didClose`; published parse and type diagnostics |
| Per-file diagnostics | Every file in the project typecheck gets its own list (an imported file's parse error lands on that file; its importers skip cascade errors until it parses). Files that become clean are cleared. Nothing is rendered to stderr |
| Protocol | Unsupported requests get `MethodNotFound`; malformed params get `InvalidParams`. A failing notification is logged, never fatal |
| Project overlays | Unsaved buffers overlay disk via `Pipeline::set_file_text`; `ProjectIndex` is refreshed on each change |
| Multi-file navigation | Goto-definition uses the use-graph `ProjectIndex` and can land in **unopened** imported `.hy` files |
| Hover | Types from the project checker (cross-file, `let x = e` shows the type of `e`, `let x: T` shows `T`), `///` docs including at the definition site in another file, parameter docs, virtual-module stubs, `macro` / `derive` / `attr` docs. Unparsable buffers fall back to the single-file / last-good path |
| Completion | Keywords, decls, inferred types, function snippets, mid-edit sanitize / last-good fallback; `Enum.Case` after `.`; virtual exports after `::` |
| Member completion | `recv.` / `recv.pre` / `self.field.` list the receiver class's fields, then methods (`Checker::class_members`). The receiver is typed from a copy of the buffer with the half-typed access removed; `self` is the enclosing `impl` owner |
| Member goto-definition | `p.method` / `p.field` jump to the `impl` method or class field of the receiver's class, in open buffers or indexed project files |
| Inlay hints | `: T` after unannotated `let` names (not `_…` or `new C(…)`), `param:` before positional arguments to free functions (skipped when the argument is that same name) |
| Code actions | Quick fixes: import an unknown name (E0100 / E0101 / E0110) from each workspace module that declares it, joining an existing `use m::{…}`; add `default => {},` to a non-exhaustive statement `match` (E0209). Refactor: add the inferred `: T` to the `let` under the cursor |
| Signature help | Declaration-based `name(T a, U b) -> R` for local and imported `fn`s, methods, and `macro` / `attr` / `derive` callables; `name!(…)` strips the bang; nesting / string aware active parameter; virtual names from the completion index |
| Format | Whole-document `coil fmt` |
| Range format | Reformats **overlapping top-level items** only (not a token-precise rustfmt range) |
| Document / workspace symbols | Top-level decls including `macro` / `derive` / `attr`; workspace symbols include indexed use-graph files |
| Highlights / references / rename | Function locals resolve through lexical scopes (`compiler::local_scopes`: `let`, params, `for`, match / `if let` patterns, lambda args and `use (…)` captures), so renaming a local touches only that binding, declaration included. Globals use `SymbolIndex` sites minus same-named locals. `prepareRename` refuses keywords / unresolved words; `rename` rejects invalid identifiers |
| Folding | One fold per top-level document symbol that spans lines |
| Selection ranges | Nested AST spans containing the cursor (full file if parse fails) |
| Semantic tokens | Lexical comments (`//`, nestable `/* */`), strings (including escaped quotes), numbers, operators, plus AST / `Checker` classification. Contextual `derive` / `macro` / `quote` / `attrs` highlight as keywords; `name!(…)` calls use the `macro` token type |

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
- Trait-item navigation; member completion on non-class receivers
  (strings, arrays, enums).
- The default-arm quick fix skips value `match`es (a value arm needs an
  expression the fix cannot choose).
- `coil.toml` `[module].roots` (compiler language path ignores it; pass
  `--root` on CLI).
- Semantic token modifiers (`declaration`, `readonly`, …).

The reporting crate exposes byte-to-LSP UTF-16 position conversion so other
tools can share the same source-location behavior as this server.

## Editor configuration

Point an editor's Coil language client at `coil-lsp` and use `coil` as the
language identifier. The server requires no command-line arguments; `--stdio`
is accepted for compatibility with editor launch configurations.
