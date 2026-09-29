# Formatter (`coil fmt`)

`coil fmt` re-execs the sibling `coil-fmt` helper (git-style). That helper parses
`.hy` sources and pretty-prints from the AST (hardcoded 4-space indent).

```bash
cargo build   # coil + coil-fmt (+ other helpers)
coil fmt path/to/file.hy
coil fmt src/
coil fmt --check .
# or invoke the helper directly:
coil-fmt --check examples/fib.hy
```

CI runs `coil-fmt --check` over `examples`, `tests/positive`,
`tests/negative_runtime` and `tests/*.hy` (`tests/compile_fail` holds
deliberately broken programs and is skipped).

## Behavior

| Mode | Effect |
|------|--------|
| default | Rewrite files in place |
| `--check` | Report paths that would change; exit `1` if any; no writes |

Directories are walked recursively for `*.hy`. Non-`.hy` files given explicitly are rejected.

## Line wrapping

Soft wraps kick in when a construct would exceed **100** columns:

| Construct | Break style |
|-----------|-------------|
| `&&` / `\|\|` / `??` chains | Operator stays at end of the previous line; continuation hangs under the first operand |
| `.` / `?.` member and method chains | Each `.name` / `.name(args)` on its own line at +1 indent |
| Call args, tuples, arrays/lists, dicts, type args, params | One item per line at +1 indent; **trailing comma** after every item (including the last) |

Short forms stay on one line (except 1-tuples, which always keep `(x,)`).

A broken list made only of short literals (numbers, `true`/`false`, strings of
up to 8 characters) fills each line up to the width instead of one item per
line, still with a trailing comma.

Class and enum bodies are always multiline when non-empty and use trailing commas after each field/variant. Match arms are one per line with a trailing comma after every arm, including the last.

## Normalization

- A statement-position `match` loses its trailing `;` (it ends at `}` like `if`).
- Redundant parentheses are removed: around a whole initializer, return value,
  argument, element or `if` / `while` / `match` head (`let w = ((1 + 2));` →
  `let w = 1 + 2;`, `if (a > 1)` → `if a > 1`), and around atoms anywhere
  (`(y).z` → `y.z`). Parens that change precedence (`(1 + 2) * 3`), tuples, and
  `(1).f()` (which would read as a float) are kept. A `{` record head keeps its
  parens so it does not read as the body.
- An empty block is `{}`.

Adjacent `use` statements are kept together; a blank line or a comment the
author put between two `use`s starts a new group and is kept.
Imports with the same namespace are grouped, such as
`use io::{stdout, open};`. Brace-group forms (`use io::{stdout, open};`) round-trip
through the same grouping. Different nested namespaces are grouped only when each
import has more than three path segments.

## Comments and docs

- `//` and `/* */` comments are parser **trivia** (`trivia()` in
  `parser/src/lib.rs`): they may sit between any two tokens and never reach
  the AST. The formatter reads them from `parser::comments::collect` and
  reattaches them by byte position: a comment on its own line leads the next
  item (at that item's indent), a comment after code on the same line trails
  it (`let x = 1; // why`, `fn f() { // why`, `}, // arm`). A comment inside an
  otherwise flat list / record forces the multi-line layout. A comment in the
  middle of an expression moves to the next line boundary; none is ever dropped.
- Blank lines: a single blank line between statements, fields, arms or list
  items is kept (runs collapse to one); none is added at the start or end of
  a body. Top level: one blank line between items, except that a comment
  directly above an item stays attached and a header comment block stays one
  block.
- Safety net: `format_source` re-parses its output and recounts comments. If
  either check fails it returns a `coil fmt bug` error and the file is left
  unchanged.
- `///` doc comments attach to the following declaration (`fn`, `class`, `field`, `trait`, `enum`, …) as `docs: Vec<&str>`. Read them later via [`parser::item_docs`](../../parser/src/ast.rs).
- `///` lines immediately inside a function parameter list attach to that
  parameter; documented parameter lists are formatted one item per line with
  trailing commas.
- Orphan `///` (not immediately before a documentable item) is a **parse error**.
- Attributes may follow docs: `/// …` then `#[…]` then the keyword.

## Limitations

- Style is fixed (4 spaces, 100 columns); there is no config file by design.
- Parse errors abort that file (pretty diagnostics via the reporting crate); other files in a multi-path run still process.
