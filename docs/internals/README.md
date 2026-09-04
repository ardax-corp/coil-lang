# Internals

How coil is structured for contributors. End-user language docs live in [coil-website](https://github.com/ardax-corp/coil-website) (`src/content/docs/`; site routes `/docs/<path>` until a public domain is set).

| Document | Contents |
|----------|----------|
| [Pipeline](pipeline.md) | Parse → typecheck → codegen → archive → execute |
| [IL opt contracts](../../compiler/src/il/opt/README.md) | Per-pass input / output / refusals / solo tests (D1) |
| [Limitations](limitations.md) | Known gaps, workarounds, and tracking |
| [Optimization roadmap](optimization-roadmap.md) | AOT/JIT plan; float-fuse tombstones; residual = payload layout. Extra benches: `examples/perf/gc_churn.hy` ([#286](https://github.com/ardax-corp/coil-lang/pull/286)), Option/Result ObjEnum churn ([#289](https://github.com/ardax-corp/coil-lang/pull/289)) |
| [Array pins](array-pin.md) | Shipped `ArrayPin` / `IndexPin*` handle (COI-198) |
| [Heap identity](heap-identity.md) | Mapped slab + header poison for `find_object_by_addr` (COI-200) |
| [SIMD](simd.md) | `coil-simd` — stable `std::arch` kernels for packed LA |
| [Auto-par](auto-par.md) | Purity analysis + capped fork-join for recursive binops |
| [IO reactor](io-reactor.md) | Sync adapter waits + async `await_*` / CPU help-steal; HostInvoke **119**/`stream_attach`, **120**/`stream_park`; clocks **121–123** (`clock_wall_nanos` / `clock_mono_nanos` / `clock_sleep_ms`) |
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
| `coil-cli` | Shared CLI argument parsing (`try_run_embedded` for packaged apps) |
| `reporting` | Diagnostics rendering (ariadne) |
| `coil-embed` | Packaged-app runner: a small bin that calls `coil_cli::try_run_embedded`. **Not** an embed-the-VM library. |

`coil package` concatenates a `.hyc` onto a runner (`coil-embed` when present, otherwise the full `coil` binary). There is no supported embed-the-VM library API yet (that needs host-catalog + archive C-layout work, later).

Contributor invariants: [AGENTS.md](../../AGENTS.md).
