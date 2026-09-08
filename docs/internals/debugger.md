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
coil debug gated.hy --allow-attach -x cmds.txt --batch
# DAP mode for IDE integration (program path from launch request):
coil debug --dap
coil debug --dap --allow-exit
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
| `--allow-attach` / `--allow-exit` / `--allow-exec` / `--allow-ffi-exec` / `--allow-dload STEM` | Host grants at typecheck (same as `coil compile` / `coil dissect`) |
| `--ffi-search-path DIR` | Extra FFI lookup directory (not a grant) |
| `--root DIR` | Extra `use`/`mod` search directory |

Host grants are CLI / DAP-launch only — `coil.toml` does not grant them. Ungranted
gated calls fail typecheck (`E0406`–`E0411`). The compiled in-memory bytecode is
the grant at run time, same as other compile-and-run paths.

`coil debug` does **not** take `-Og` / `--opt-level`. The session always sets
`Pipeline::set_debugger_attached(true)` (I7). Use `-Og` on `coil compile` /
`coil dissect` when you want the same refuse without a debug controller.

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
| `stopOnEntry` | Optional; stop **before** the first bytecode insn (real VM frame) |
| `allowAttach` / `allowExit` / `allowExec` / `allowFfiExec` | Optional; OR with CLI `--allow-*` |
| `allowDload` | Optional string array of dload stems |
| `ffiSearchPath` | Optional string array of FFI lookup dirs |

**Supported (v1):** breakpoints (line + function), continue, step in/over/out,
stack trace, locals, `stopOnEntry`, host grants. **Not supported:** attach,
evaluate, conditional breakpoints, multi-thread.

`stopOnEntry` starts the VM and pauses at PC 0 (prologue). `stackTrace` /
`stepIn` work from that stop. A previous fake pause (no `start`) left those
empty — that is fixed.

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
| `step` / `s` | Until source line changes (into calls); falls back to `stepi` if the current PC has no line |
| `next` / `n` | Until line changes at ≤ current frame depth |
| `finish` / `fin` | Until current frame returns |
| `print` / `p` `<name\|$N>` | Format local by name or slot index |
| `bt` | Call stack with symbol + `file:line` when **exactly** known |
| `list` / `l` | Source around the current stop (nearest loc in this function, else `fn` decl) |
| `disassemble` / `disas` `[fn]` | Bytecode dump |
| `quit` / `q` | Exit |

## Notes

- Locals are available by **name** (`print n`, `info locals`) and by slot (`print $0`).
  Names come from compile-time slot maps (params, `let`s, `self`, match bindings).
  Shadowing keeps the innermost binding; synthetic `__pad*` / `__dict*` slots are omitted.
- **Line breakpoints are sparse.** Many codegen sites still emit unknown
  `debug_locs`. `break 12` / DAP `setBreakpoints` on an unmapped line fails
  (`verified: false` / REPL `no code locations`). Prefer function breakpoints.
  `list` may snap to a nearby known loc **in the same function**; `bt` / DAP
  stack lines stay exact (unknown → no path / line 0).
- Function breakpoints use live compile symbols (same FQN rules as `coil dissect --fn`).
- Hot path: stop checks run only when a debug controller is attached.
- **I7:** `coil debug` sets `Pipeline::set_debugger_attached(true)` so
  dense specialize and MIR→LIR body replace stay off. Stops remain on
  fuse-IL bytecode (this debugger). **MIR `Deopt` metadata is unused** by
  `coil-debug` / DAP — it names edges for a later native tier; see
  [mir-deopt.md](mir-deopt.md).
