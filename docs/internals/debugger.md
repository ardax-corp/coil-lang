# Debugger

`coil debug` is a GDB-style debugger for coil programs. The main `coil` binary
**re-execs** the sibling `coil-debug` helper (git-style). That helper compiles the
entry `.coil` (and module graph) **in memory** — never writes `out.ccoil` — and runs
it on the MIR interpreter, through a stop engine gated behind an attached
`DebugController`.

```bash
cargo build   # coil + coil-debug (+ coil-dissect / coil-embed)
coil debug examples/fib.coil
coil debug examples/fib.coil -x cmds.txt --batch
coil debug gated.coil --allow-attach -x cmds.txt --batch
# DAP mode for IDE integration (program path from launch request):
coil debug --dap
coil debug --dap --allow-exit
coil-debug --dap
# or invoke the helper directly:
coil-debug examples/fib.coil -x cmds.txt --batch
```

| Flag | Effect |
|------|--------|
| (none) | Interactive `(coil) ` REPL on stdin |
| `-x <script>` | Run commands from a file (`#` comments; one command per line) |
| `--batch` | Non-interactive; run the whole script (or stdin if no `-x`). A failing command is reported and the script goes on, like `gdb -batch`; the exit status is non-zero if any command failed or the program panicked |
| `--dap` | Debug Adapter Protocol over **stdio** (see below); no positional `.coil` |
| `--allow-attach` / `--allow-exit` / `--allow-ffi-exec` / `--allow-dload STEM` | Host grants at typecheck (same as `coil compile` / `coil dissect`) |
| `--ffi-search-path DIR` | Extra FFI lookup directory (not a grant) |
| `--root DIR` | Extra `use`/`mod` search directory |

Host grants are CLI / DAP-launch only. Ungranted
gated calls fail typecheck (`E0406`–`E0411`). The compiled in-memory program is
the grant at run time, same as other compile-and-run paths.

`coil debug` does **not** take `-Og` / `--opt-level`. The session always sets
`Pipeline::set_debugger_attached(true)`, compiles at `-O 0` and runs raw MIR.

## IDE debugging (DAP)

`coil-debug --dap` speaks the [Debug Adapter Protocol](https://microsoft.github.io/debug-adapter-protocol/)
over stdin/stdout (Content-Length framing). Point any DAP client (VS Code, Neovim
`nvim-dap`, Helix, etc.) at the `coil-debug` binary with `--dap`; editor packages
are not shipped in this repository.

**Launch args** (DAP `launch` request):

| Field | Meaning |
|-------|---------|
| `program` | Absolute or workspace-relative path to entry `.coil` |
| `cwd` | Optional working directory for resolving paths |
| `stopOnEntry` | Optional; stop **before** the program's first op |
| `allowAttach` / `allowExit` / `allowFfiExec` / `allowRead` / `allowWrite` / `allowNet` / `allowEnv` / `allowAll` | Optional; OR with CLI `--allow-*` |
| `allowDload` | Optional string array of dload stems |
| `ffiSearchPath` | Optional string array of FFI lookup dirs |

**Supported (v1):** breakpoints (line + function), continue, step in/over/out,
stack trace, locals, `stopOnEntry`, host grants. A panic is a `stopped` event
with reason `exception` and the stack intact; the next resume sends `exited`
(code 1) and `terminated`. **Not supported:** attach, evaluate, conditional
breakpoints over DAP (REPL only), OS threads (`thread::spawn` workers).

**Tasks as threads.** While a `task::scope` has child tasks, `threads`
lists every unfinished task with its state (`task 2 [running]`,
`main [blocked (end of scope)]`, `task 1 [blocked (sleep)]`). The running
task is thread `1` (the one that stops and steps); the others are
`1000 + task id`. `stackTrace` on a task shows its own call chain: the
running task's frames end at its task body, the root task's frames sit
below it, and a suspended child's frames come from its
saved coroutine. Locals are available for the frames on the stack; a
suspended child's frames have none (`scopes` is empty).

`stopOnEntry` starts the program and pauses before its first op. `stackTrace` /
`stepIn` work from that stop.

Every statement carries a debug location (see [debug-info.md](debug-info.md)),
so any line with code verifies; lines without code (signatures, braces, blank
lines, code the optimizer removed) return `verified: false`. Source paths are
matched canonically, so the absolute paths DAP clients send resolve against the
paths stored at compile time. Locs from expanded `macro` / `derive` / `attr`
output are rewritten onto the use site so breakpoints and `list` land in the
source you wrote.

## Commands

| Command | Action |
|---------|--------|
| `break` / `b` `<fn\|file:line\|line> [if <name\|$N> <op> <value>]` | Set breakpoint (function FQN or source line). With `if`, stop only when the condition holds: `op` is `==` `!=` `<` `<=` `>` `>=`, `value` an integer or `true` / `false`, compared with the local's slot as an integer |
| `delete` / `d` `[n]` | Clear one / all breakpoints |
| `info break` / `info registers` / `info locals` | List breakpoints, op number and depth, or named locals |
| `run` / `r` | Start or restart from prologue |
| `continue` / `c` | Resume until next stop |
| `stepi` / `si` | One MIR op |
| `step` / `s` | Until source line changes (into calls); falls back to `stepi` (prints `Step`) if the current op has no line |
| `next` / `n` | Until line changes at ≤ current frame depth; same `stepi` fallback when the op has no line |
| `finish` / `fin` | Until current frame returns |
| `print` / `p` `<name\|$N>` | Format local by name or slot index |
| `bt` | Call stack, gdb order (`#0` is the innermost frame), with symbol + `file:line`. |
| `list` / `l` | Source around the current stop (nearest loc in this function, else `fn` decl) |
| `disassemble` / `disas` `[fn]` | A function's MIR |
| `quit` / `q` | Exit |

## Notes

- **Panics stop the program for inspection.** The frames stay: `bt`, `print`
  and `info locals` show the panicking frame. It cannot resume; `run`
  restarts.

- **It runs raw MIR on the interpreter.** `coil debug` compiles at `-O 0` with
  a raw capture and runs on `coil-mir-interp` (`docs/internals/mir-interp.md`),
  on a thread of its own. A release build can still differ from a debug run, as
  in any compiled language.

- **Locals.** `info locals` / `print` read a local from the registers: the word of
  the value last bound to it (`MirFunc::local_binds`). Raw MIR keeps every value,
  so a value is exact, and a local nothing has bound yet shows `<not set>`.
  - Codegen records each binding (`let`, parameter, `for` variable, match
    binding) with its **source scope** and type (`compiler::debug_vars`). The
    locals visible at a stop are the ones whose scope contains the stop's
    statement; the innermost wins for a shadowed name.
  - Heap values render with their static type: strings, class instances
    (fields by name), arrays / `Vec`, tuples, enums (including the `Option` /
    `Result` niche layouts), a few levels deep.
  - `print` takes paths: `print p.x`, `print xs[2]`, `print a.b[1].c`.
- **Line breakpoints** work on any line with code: every op a statement emits
  carries that statement's span unless a nested statement gave it a narrower
  one. `break 12` on a line without code (or code the optimizer merged away)
  fails with `no code locations`. `bt` / DAP stack frames show `file:line`.
- Function breakpoints use live compile symbols (same FQN rules as `coil dissect --fn`).
- Hot path: the interpreter's loop is generic over a `const DEBUG: bool`, and
  only the debugged program's thread takes the instantiation with the stop
  check, so nothing else pays for it.
- A panic stops before it unwinds, and that run goes no further. `run` starts a
  new one; the old thread stays parked until the session ends, because its
  frames cannot be unwound as they are.
