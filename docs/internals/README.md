# Internals

How coil is structured for contributors. End-user language docs live in [coil-website](https://github.com/ardax-corp/coil-website) (`src/content/docs/`; site routes `/docs/<path>` until a public domain is set).

| Document | Contents |
|----------|----------|
| [Pipeline](pipeline.md) | Parse → typecheck → codegen → archive → execute |
| [Numeric MIR](mir.md) | COI-267/268: SSA sidecar + dense numeric bytecode (Value ABI at edges) |
| [MIR language islands](mir-islands.md) | COI-292 I0: islands vs full-MIR rewrite; I1–I8 ladder; I8 IL→MIR entry; I4 FORMAT/string **reopened** under Q9 (delivery ladder); A/B rules |
| [Language quirks](language-quirks.md) | Locked Q1–Q9 (2026-09-10): `[T; N]` box-once, class field-SROA, grow type error, defined `i % N`, panic vs raise, roadmap Q6–Q9 |
| [Q6 iterator protocol](q6-iterator-protocol.md) | Counted desugar for `for` (array / literal range / B5 first-class range locals); later rungs for user Iterator / coro |
| [Q9 format / string](q9-format-string.md) | I4 reopen: SSA + LIR reconstruct of `STRING` / `PRINT` / `FORMAT` / `STRINGIFY`; later rungs for bytes / maps / unicode |
| [Opt generalization](opt-generalization.md) | COI-333 A0: MIR default + one object story + dense-native + cost gate; A4 measurement; **B0** audit + **B1** Q6–Q8 entry hygiene |
| [MIR deopt / debugger](mir-deopt.md) | COI-299 I7: stop/deopt edges; VM debugger stays source of truth |
| [Specialize refuse](specialize-refuse.md) | Hard walls vs Q6–Q9 ladders / cost gate; B1 entry hygiene; B2 CALL convoy; B3 two-slot CALL; B7 sibling / mutual / self two-slot |
| [IL opt contracts](../../compiler/src/il/opt/README.md) | Per-pass input / output / refusals / solo tests (D1) |
| [Limitations](limitations.md) | Known gaps, workarounds, and tracking |
| [Optimization roadmap](optimization-roadmap.md) | AOT/JIT plan; landed opts (#304–#318); hit-bench prove rule; PGO removed (#301). Extra benches: `gc_churn`, Option/Result churn, `iv_mul_sr` / `licm_nested_chains` / `tail_sibling` / `cse_*` / `dest_prop_field_alias` |
| [Array pins](array-pin.md) | Shipped `ArrayPin` / `IndexPin*` handle (COI-198) |
| [Heap identity](heap-identity.md) | Mapped slab + header poison for `find_object_by_addr` (COI-200) |
| [Incremental GC](gc-incremental.md) | COI-309 S4: safepoint mark + SATB + lazy sweep; moving GC deferred |
| [SIMD](simd.md) | `coil-simd` — packed LA + V0/V1 `V*` opcode backend |
| [Auto-par](auto-par.md) | Purity analysis + capped fork-join for recursive binops |
| [IO reactor](io-reactor.md) | Sync adapter waits + async `await_*` / CPU help-steal; HostInvoke **119**/`stream_attach`, **120**/`stream_park`; clocks **121–123**; M1 math **125–135** (archive minor 5: `atan`…`tanh`). `PI`/`E`/`TAU` → coil-stdlib `num` |
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
