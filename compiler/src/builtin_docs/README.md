# Builtin doc stubs

One `.hy` file per compiler virtual module (`io/fs.hy` is `io::fs`), plus
`types.hy` (primitive types) and `keywords.md` (one `## word` section per
keyword). Each item is a declaration with an empty body and `///` docs; the
files are parsed for hover docs and never compiled. `compiler/src/builtin_docs.rs`
lists them, and a coil-lsp test checks that every builtin has docs here.

Docs follow Rust std style: a one-line summary, details, then `# Examples`.

Examples are future doc-tests:

- A fence opened with ```` ``` ```` or ```` ```coil ```` is an example; ```` ```text ```` is not run.
- Lines starting with `# ` are hidden on hover but run (setup such as `# use tests::assert;`).
- `use` lines are hoisted; without a `fn main` the rest is wrapped in `fn main() { ... }`.
- An example passes when it compiles and exits 0.
