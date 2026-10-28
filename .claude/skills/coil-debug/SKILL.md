---
name: coil-debug
description: >-
  Troubleshoot coil programs with `coil debug` (GDB-style REPL / batch scripts):
  infinite loops, hangs, wrong values, panics, unexpected call stacks. Use when
  a `.hy` program loops forever, hangs, panics, prints wrong output, or when the
  user asks to debug, step, breakpoint, backtrace, or inspect VM state.
---

# coil debug troubleshooting

Prefer **batch scripts** (`-x` + `--batch`) over interactive REPL when automating.
`coil debug` compiles in memory — it never writes `out.hyc`.

Full command reference: [docs/internals/debugger.md](docs/internals/debugger.md).
For bytecode/IL dumps without running: `coil dissect <file.hy> --fn <pat>`.

## Quick start

```bash
# Build helpers next to `coil` (required for dispatch)
cargo build -q

# Interactive REPL
cargo run --quiet -- debug path/to/prog.hy

# DAP over stdio (any DAP client: nvim-dap, etc.)
coil debug --dap

# Scripted (preferred for agents)
cargo run --quiet -- debug path/to/prog.hy -x /tmp/coil_dbg.txt --batch

# Grant-gated programs (same flags as compile / dissect)
coil debug gated.hy --allow-attach -x /tmp/coil_dbg.txt --batch
```

Use `coil dissect` for bytecode/IL/AST dumps; use `--release` for faster runs.

## Command cheat sheet

| Need | Commands |
|------|----------|
| IDE debug session | `coil debug --dap` (DAP client launches adapter) |
| Stop in a function | `break fib` / `b Foo::method` then `run` |
| Stop at a line | `break file.hy:12` or `break 12` (entry file) |
| See how you got here | `bt` |
| See source | `list` |
| See bytecode | `disas` / `disas fib` |
| Inspect locals | `print n` / `print $0` / `info locals` |
| Step | `stepi` (insn), `step` (line into), `next` (over), `finish` |
| Resume | `continue` — clear recursive BPs first with `delete` if needed |
| Restart | `run` |
| Exit | `quit` |

Recursive functions re-hit function breakpoints on every call — `delete` before `continue` to run to completion.

## Workflow: infinite loop / hang

Suspect a tight loop or runaway recursion when the process never exits.

```
Task:
- [ ] 1. Identify candidate fn / loop site (source or `dissect --fn`)
- [ ] 2. Break on entry; `run`; confirm stop
- [ ] 3. `bt` + `list` + `print $N` — note depth / slots
- [ ] 4. `next` or `stepi` a few times — does PC/line/slots change?
- [ ] 5. If recursion: watch `bt` depth grow unboundedly
- [ ] 6. Fix source; re-run the same `-x` script to confirm exit
```

**Batch template** (`/tmp/coil_dbg.txt`):

```text
# Break on the looping function or a line inside the loop body
break looping_fn
run
bt
list
info registers
# Step a handful of times; if depth/slots never progress toward exit → infinite
next
next
next
bt
print $0
quit
```

**Signals**
- `bt` depth climbs on every `continue`/`next` → unbounded recursion
- Same source line + same `$N` forever after `next`/`stepi` → stuck loop / bad condition
- Never reaches `Program exited normally.` after `delete` + `continue` → still looping

Timeout the process if needed (`timeout 5s cargo run -- …`); a hang after `continue` is itself evidence.

## Workflow: wrong value / bad print

```text
break compute
run
bt
list
print $0
info locals
print n
bt
list
quit
```

Compare named locals / slots to expected intermediates. Use `disas <fn>` if fusion/`BinSlot*` obscures the mapping.

## Workflow: panic

```text
# Break just before the panic site (fn or line), or run until panic
break suspect_fn
run
bt
print $0
info locals
print n
quit
```

Panic stops the session (`Program panicked.` / non-zero in `--batch`). `bt` + `list` at the last stop show the path; `resolve` uses `debug_locs` when known.

## Workflow: unexpected control flow

```text
break caller
run
next
bt
finish
bt
disas callee
quit
```

`finish` returns to the caller; `bt` shows whether the intended callee ran.

## Agent rules

1. Write a temp `-x` script; run with `--batch`; capture stdout/stderr.
2. Start with **function** breakpoints (`break name`) — more reliable than lines when `debug_locs` are sparse (`verified: false` / `no code locations`).
3. After a recursive hit, `delete` before `continue` unless you intend to stop on every call.
4. Prefer `next` for source-level progress; use `stepi` + `disas` when line info is missing.
5. Prefer **named** locals (`print n`, `info locals`); fall back to `$N` when a name is missing.
6. Do not rely on `out.hyc` for debug sessions; delete it only if mixing with normal `coil` runs.
7. If debug cannot set a line BP, fall back to `coil dissect <file> --fn <pat>` and break on the function entry PC symbol instead.

## Related docs

- [docs/internals/debugger.md](docs/internals/debugger.md)
- [docs/internals/dissect.md](docs/internals/dissect.md)
- [docs/internals/debug-info.md](docs/internals/debug-info.md)
