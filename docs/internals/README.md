# Internals

How coil is structured for contributors and embedders. End-user language docs live in the [manual](../manual/getting-started.md) and [references](../references/README.md).

| Document | Contents |
|----------|----------|
| [Pipeline](pipeline.md) | Parse → typecheck → codegen → archive → execute |
| [Limitations](limitations.md) | Known gaps, workarounds, and tracking |
| [Array pins](array-pin.md) | Shipped `ArrayPin` / `IndexPin*` handle (COI-198) |
| [SIMD](simd.md) | `coil-simd` — stable `std::arch` kernels for packed LA |
| [Auto-par](auto-par.md) | Purity analysis + capped fork-join for recursive binops |
| [IO reactor](io-reactor.md) | Sync adapter waits + async `await_*` / CPU help-steal |
| [Stack bounds](stack-bounds.md) | Recursion depth analysis and `#[max_depth]` |
| [Collections VM split](collections-vm-split.md) | Userland collections vs VM primitives |
| [Debug line table](debug-info.md) | `source_files` / `debug_locs` in `.hyc` |
| [Opcodes](opcodes.md) | Selected bytecode ops behind builtins |
| [Dissect](dissect.md) | `coil dissect` — in-memory bytecode / IL / AST dump |
| [Debugger](debugger.md) | `coil debug` — GDB-style REPL / batch debugger |
| [Formatter](fmt.md) | `coil fmt` — AST pretty-printer for `.hy` |
| [LSP](lsp.md) | `coil lsp` — language server |
| [Test health report](test-health-report.md) | Historical flaky/broken-test notes |
| [String table migration](string-table-migration.md) | Completed migration note (retired `print` keyword) |
| [Grammar](grammar/) | tree-sitter grammar sources |

## Crate map

| Crate | Role |
|-------|------|
| `parser` | Pratt parser and AST |
| `compiler` | HM typechecker, stack IL codegen, pipeline |
| `machine` | VM, heap/GC, FFI (libffi), host natives |
| `common` | Opcodes, values, archive format |
| `coil-simd` | Stable SIMD helpers (`std::arch`) for numeric / byte kernels |
| `coil-cli` | Shared CLI argument parsing |
| `reporting` | Diagnostics rendering (ariadne) |

Contributor invariants: [AGENTS.md](../../AGENTS.md).
