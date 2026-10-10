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
| `--batch` | Non-interactive; run the whole script (or stdin if no `-x`). A failing command is reported and the script goes on, like `gdb -batch`; the exit status is non-zero if any command failed or the program panicked |
| `--dap` | Debug Adapter Protocol over **stdio** (see below); no positional `.hy` |
| `--allow-attach` / `--allow-exit` / `--allow-exec` / `--allow-ffi-exec` / `--allow-dload STEM` | Host grants at typecheck (same as `coil compile` / `coil dissect`) |
| `--ffi-search-path DIR` | Extra FFI lookup directory (not a grant) |
| `--root DIR` | Extra `use`/`mod` search directory |

Host grants are CLI / DAP-launch only. Ungranted
gated calls fail typecheck (`E0406`–`E0411`). The compiled in-memory bytecode is
the grant at run time, same as other compile-and-run paths.

`coil debug` does **not** take `-Og` / `--opt-level`. The session always sets
`Pipeline::set_debugger_attached(true)` (I7). **B8:** that flag no longer
disables MIR specialize. Use `-Og` on `coil compile` / `coil dissect` for
Basic IL cleanup (still may dense / LIR).

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
| `allowAttach` / `allowExit` / `allowExec` / `allowFfiExec` / `allowRead` / `allowWrite` / `allowNet` / `allowEnv` / `allowAll` | Optional; OR with CLI `--allow-*` |
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
below it on the VM stack, and a suspended child's frames come from its
saved coroutine. Locals are available for the frames on the stack; a
suspended child's frames have none (`scopes` is empty).

`stopOnEntry` starts the VM and pauses at PC 0 (prologue). `stackTrace` /
`stepIn` work from that stop. A previous fake pause (no `start`) left those
empty — that is fixed.

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
| `info break` / `info registers` / `info locals` | List breakpoints, IP/SP/depth, or named locals |
| `run` / `r` | Start or restart from prologue |
| `continue` / `c` | Resume until next stop |
| `stepi` / `si` | One bytecode instruction |
| `step` / `s` | Until source line changes (into calls); falls back to `stepi` (prints `Step`) if the current PC has no line |
| `next` / `n` | Until line changes at ≤ current frame depth; same `stepi` fallback when the PC has no line |
| `finish` / `fin` | Until current frame returns |
| `print` / `p` `<name\|$N>` | Format local by name or slot index |
| `bt` | Call stack, gdb order (`#0` is the innermost frame), with symbol + `file:line`. The VM bootstrap frame is hidden |
| `list` / `l` | Source around the current stop (nearest loc in this function, else `fn` decl) |
| `disassemble` / `disas` `[fn]` | Bytecode dump |
| `quit` / `q` | Exit |

## Notes

- **Panics stop the program for inspection.** The frames stay: `bt`, `print`
  and `info locals` show the panicking frame. It cannot resume; `run`
  restarts.

- **Locals under optimization.** `info locals` / `print` show a value only
  where it is known to be live, and `<optimized out>` elsewhere; never a stale
  or foreign word. How it works (`compiler::debug_vars`):
  - Codegen records each binding (`let`, parameter, `for` variable, match
    binding) with its **source scope** (declaration to the end of its block)
    and type. The locals visible at a stop are the ones whose scope contains
    the stop's statement; the innermost wins for a shadowed name.
  - A binding's defining stores get a debug location equal to its **name
    token** (its def site). Passes keep op locations when they rewrite or move
    an op, and MIR moves a store's location onto the value it stores, so the
    tag follows the variable into whatever slot or dense register now holds
    it. After finalize, a dataflow over each body's final bytecode builds
    **location lists** (`pc` range → slot): a tagged write defines the
    variable (and makes older copies stale), a copy (`DenseMove`,
    `LOAD s; STORE d`) carries it, any other write kills it, and joins keep
    only agreeing claims. A body with an unmodeled opcode keeps the codegen
    slots.
  - Layouts codegen split are rendered whole: Q2 class locals as
    `Point { x: 3, y: <optimized out> }`, Q1 fixed arrays as `[1, 2, 3]`,
    two-slot enums as `Option::Some(5)`; each component is tracked separately.
  - Heap values render with their static type: strings, class instances
    (fields by name), arrays / `Vec`, tuples, enums (including the `Option` /
    `Result` niche layouts), a few levels deep.
  - `print` takes paths: `print p.x`, `print xs[2]`, `print a.b[1].c`.
  - Known gaps: a value an optimization turned into an alias of another slot
    (copy propagation removed its store) shows as `<optimized out>`; match
    bindings (written through the cursor) use their codegen slot.
- **Line breakpoints** work on any line with code: every op a statement emits
  carries that statement's span unless a nested statement gave it a narrower
  one. `break 12` on a line without code (or code the optimizer merged away)
  fails with `no code locations`. `bt` / DAP stack frames show `file:line`.
- Function breakpoints use live compile symbols (same FQN rules as `coil dissect --fn`).
- Hot path: stop checks run only when a debug controller is attached. The
  dispatch loop is generic over a `const HOOKS: bool` (`execute::<HOOKS>`):
  `run_execute` picks the hooked instantiation only while a controller (or a
  coverage collector) is attached, so a `coil` built with the `debugger`
  feature through workspace unification still has no per-instruction check.
- **I7 / B8:** `coil debug` sets `Pipeline::set_debugger_attached(true)`.
  Stops remain on reconstructed bytecode (fuse-IL, LIR, or dense).
  Function breakpoints and `stepi` work on specialized bodies; with a
  debugger attached the VM checks every instruction (no dense streaks run
  past a breakpoint). **MIR `Deopt` metadata is unused** by `coil-debug` /
  DAP — emit skips those insts.
  Dense / LIR reconstruct keeps most statement locations; lines whose ops are
  merged (constant `let`s folded into one dense init, eliminated copies) have
  none. See [mir-deopt.md](mir-deopt.md).
