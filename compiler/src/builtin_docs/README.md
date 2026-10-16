# Builtin doc stubs

One `.hy` file per compiler virtual module (`io/fs.hy` is `io::fs`), plus
`types.hy` (primitive types) and `keywords.md` (one `## word` section per
keyword). Each item is a declaration with an empty body and `///` docs; the
files are parsed for hover docs and never compiled. `compiler/src/builtin_docs.rs`
lists them, and a coil-lsp test checks that every builtin has docs here.

Docs follow Rust std style: a one-line summary, details, then `# Examples`.

Examples are doc-tests (the rules live in `parser::doc_examples`, shared with
the language server's hover):

- A fence opened with ```` ``` ```` or ```` ```coil ```` is an example; ```` ```text ```` is not run.
  ```` ```ignore ```` is not compiled, ```` ```no_run ```` is compiled only (use it for
  examples that end the process, like `env::exit`), ```` ```should_panic ```` must panic.
- Lines starting with `# ` are hidden on hover but run (setup such as `# use tests::assert;`).
- `use` lines are hoisted; without a `fn main` the rest is wrapped in `fn main() { ... }`.
- An example passes when it compiles and runs without a panic. An `Err` that
  reaches the end of `main` (a failed `assert!`, an uncaught `raise`) is a panic.
- The wrapper's `main` takes its error type from the body, so one example
  cannot mix `?` on, say, `IoError` with `assert!` (which raises a `string`):
  `match` the `Result` instead, or give the example its own `fn main`.

CI runs them on every push:

```sh
coil test --doc-tests-only -A --root .deps/coil-stdlib/src compiler/src/builtin_docs
```
