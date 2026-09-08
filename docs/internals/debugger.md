# Debugger

`coil debug` is a GDB-style debugger for coil programs. The main `coil` binary
**re-execs** the sibling `coil-debug` helper (git-style). That helper compiles the
entry `.hy` (and module graph) **in memory** — never writes `out.hyc` — and drives
the VM through a stop engine gated behind an attached `DebugController`
(`machine` feature `debugger`).

```bash
cargo build   # coil + coil-debug (+ coil-dissect / coil-embed)
coil debug examples/fib.hy
coil debug examples/fib.hy -x cmds.txt --batch
# DAP mode for IDE integration (program path from launch request):
coil debug --dap
coil-debug --dap
# or invoke the helper directly:
coil-debug examples/fib.hy -x cmds.txt --batch
```

| Flag | Effect |
|------|--------|
| (none) | Interactive `(coil) ` REPL on stdin |
| `-x <script>` | Run commands from a file (`#` comments; one command per line) |
| `--batch` | Non-interactive; exit after script (or stdin if no `-x`); non-zero on panic / script error |
| `--dap` | Debug Adapter Protocol over **stdio** (see below); no positional `.hy` |

## IDE debugging (DAP)

`coil-debug --dap` speaks the [Debug Adapter Protocol](https://microsoft.github.io/debug-adapter-protocol/)
over stdin/stdout (Content-Length framing). Point any DAP client (VS Code, Neovim
`nvim-dap`, Helix, etc.) at the `coil-debug` binary with `--dap`; editor packages
are not shipped in this repository.

**Launch args** (DAP `launch` request):

| Field | Meaning |
|-------|---------|
| `program` | Absolute or workspace-relative path to entry `.hy` |
| `cwd` | Optional working directory for resolving paths |
| `stopOnEntry` | Optional; stop before executing `main` |

**Supported (v1):** breakpoints (line + function), continue, step in/over/out,
stack trace, locals. **Not supported:** attach, evaluate, conditional breakpoints,
multi-thread.

Line breakpoints follow the same `debug_locs` coverage limits as the REPL (see
[debug-info.md](debug-info.md)); unmapped lines return `verified: false`. Function
breakpoints (`setFunctionBreakpoints`) are more reliable when line info is sparse.

## Commands

| Command | Action |
|---------|--------|
| `break` / `b` `<fn\|file:line\|line>` | Set breakpoint (function FQN or source line) |
| `delete` / `d` `[n]` | Clear one / all breakpoints |
| `info break` / `info registers` / `info locals` | List breakpoints, IP/SP/depth, or named locals |
| `run` / `r` | Start or restart from prologue |
| `continue` / `c` | Resume until next stop |
| `stepi` / `si` | One bytecode instruction |
| `step` / `s` | Until source line changes (into calls) |
| `next` / `n` | Until line changes at ≤ current frame depth |
| `finish` / `fin` | Until current frame returns |
| `print` / `p` `<name\|$N>` | Format local by name or slot index |
| `bt` | Call stack with symbol + `file:line` when known |
| `list` / `l` | Source around the current stop |
| `disassemble` / `disas` `[fn]` | Bytecode dump |
| `quit` / `q` | Exit |

## Notes

- Locals are available by **name** (`print n`, `info locals`) and by slot (`print $0`).
  Names come from compile-time slot maps (params, `let`s, `self`, match bindings).
  Shadowing keeps the innermost binding; synthetic `__pad*` / `__dict*` slots are omitted.
- Line breakpoints need known `debug_locs` (coverage is incremental; see [debug-info.md](debug-info.md)).
- Function breakpoints use live compile symbols (same FQN rules as `coil dissect --fn`).
- Hot path: stop checks run only when a debug controller is attached.
- **I7:** `coil debug` sets `Pipeline::set_debugger_attached(true)` so
  dense specialize and MIR→LIR body replace stay off. Stops remain on
  fuse-IL bytecode (this debugger). MIR `Deopt` metadata is for a later
  native tier; see [mir-deopt.md](mir-deopt.md).
