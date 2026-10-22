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
| Member goto-definition | `p.method` / `p.field` jump to the `impl` method or class field of the receiver's class, in open buffers or indexed project files; with neither, the method of the one `impl Trait for Class` that declares it |
| Traits and `Owner::member` | `trait` declarations are definitions (hover shows the header and docs). In `Trait::method(x)` the owner jumps to the trait and the member to that trait's `fn method`, never another trait's or a free `fn` of the same name, then to the ground instance's method when the last typecheck resolved one (`Checker::call_dicts_for_span`). `Type::method` reaches an inherent `impl Type` method the same way. `SymbolIndex` keys these members `Owner::member` |
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
| Compile-time code | Macros and `comptime` blocks expand as in a build, but compile-time IO comes from the result cache only (`CacheConfig::replay`). On a miss, reading the environment, running a program, connecting or writing a file is refused, and the diagnostic says to build the project. Pass the build's `--comptime-*` flags and start the server in the build's working directory so both use one cache. See [Comptime](comptime.md#tooling) |
| Coverage | From the `coil test --coverage` tracefile, for workspace sources (not `.deps`): a code lens over each function (`covered 7/9 lines (78%)`), a **Coverage:** row in its hover, and hint diagnostics for functions below the threshold and for runs of uncovered lines. See [Coverage](#coverage) |

Search roots for an opened workspace are `src`, `.` (sibling files at the
project root), and each `.deps/*/src` checkout when present, then any
`--root DIR` given on the command line, then the installed stdlib
(`$COIL_STDLIB_DIR`, `$COIL_STDLIB`, `~/.coil/stdlib/src`, when present).
A file under nested roots is namespaced by the innermost one. Other roots
come only from `--root` flags, as for `coil`.

`coil lsp` takes the same root and host-grant flags as `coil compile`, so a
project tool can start it with the flags it uses everywhere else:

```
coil lsp --root src --root .spool/deps --root ~/.coil/stdlib/src --allow-exec
```

`--root DIR` (repeatable; relative to the server's working directory),
`--allow-read` / `--allow-write` / `--allow-net` / `--allow-env`,
`--allow-attach` / `--allow-exit` / `--allow-exec` / `--allow-ffi-exec`,
`-A`, `--allow-dload STEM` and `--ffi-search-path DIR`. The grants reach the
project typecheck, so code that the project is allowed to compile is not
flagged in the editor. The read / write / net / env / exec / exit checks
run over reachable code after codegen ([pipeline.md](pipeline.md#capabilities)),
which the editor does not run, so those show at `coil compile`, not as you type. `--stdio` is accepted and ignored; `coil lsp --help`
lists the flags.

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

## Coverage

The server reads `target/coverage/lcov.info` (relative to the workspace),
the tracefile `coil test --coverage` writes ([test-runner.md](test-runner.md#coverage)).
It never runs tests. An idle server looks at the file every two seconds; when
it changes, open documents get their diagnostics re-published and clients
with `workspace.codeLens.refreshSupport` are asked to refresh lenses.

- **Functions** are top-level `fn`s and the methods of `impl` and trait
  blocks; a function's lines are those of its declaration span, so a nested
  function's lines also count for the outer one. Functions with no coverable
  line (never-instantiated generics) get nothing.
- **Hints** (`DiagnosticSeverity::Hint`, source `coil coverage`): a function
  below the threshold gets one on its name; each run of uncovered lines gets
  one, except inside a function nothing covered (its own hint says so). No
  `Unnecessary` tag: code is never faded.
- **Stale data** shows nothing: a file changed on disk after the tracefile
  was written, or an open buffer that differs from the file.
- `initializationOptions.coverage`: `false` turns it off; or an object with
  `enabled`, `threshold` (default `0.8`) and `path` (for `--coverage-out`).

## Deferred

Leave these for a later LSP pass unless they block daily editing:

- `textDocument/typeDefinition` is advertised but shares the definition
  handler (Coil has no separate type-def table).
- Cross-project references that are not on the current use-graph.
- Member completion on non-class receivers (strings, arrays, enums);
  `Trait::` completion.
- The default-arm quick fix skips value `match`es (a value arm needs an
  expression the fix cannot choose).
- Semantic token modifiers (`declaration`, `readonly`, …).

The reporting crate exposes byte-to-LSP UTF-16 position conversion so other
tools can share the same source-location behavior as this server.

## Editor configuration

Point an editor's Coil language client at `coil-lsp` and use `coil` as the
language identifier. The server requires no command-line arguments; `--stdio`
is accepted for compatibility with editor launch configurations.
