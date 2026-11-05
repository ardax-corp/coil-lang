# MIR interpreter (`feature/mir-interp`)

`coil-mir-interp` runs MIR directly. It is to replace the bytecode VM as the
engine for what has no machine code: the playground, the debugger, comptime and
macros. Cranelift keeps running everything else from the same MIR.

```
coil prog.coil --mir              # raw MIR at -O 0: the reference output
coil prog.coil --mir -O 2         # the MIR native code compiles at -O 2
coil test --engine mir [PATH]     # a suite on it, -O 0 unless -O says otherwise
scripts/mir_diff.py [PATH ...]    # every level and engine against the reference
```

## The reference output

At `-O 0` the interpreter runs **raw MIR**: bodies as `from_hir` lowered them,
with no MIR pass run (`compiler::native_mir::start_raw_capture`), and HIR
unoptimized too. That output is the reference.

`-O 0` alone is not raw on the other captures: `optimize_mir` skips the folds and
loop passes there, but `sroa` and `phi_sroa` run at every level. A bug in either
would show on both sides of a comparison, so the raw capture skips `optimize_mir`
altogether.

At every other level the interpreter runs the capture Cranelift compiles, so
each engine and level says something different:

| Compared | A difference is |
|---|---|
| `--mir -O N` against the reference | a pass's (HIR or MIR) |
| `--jit -O N` against `--mir -O N` | Cranelift's lowering |
| The VM at `-O N` against the reference | the bytecode tiers' |
| Every engine against the reference, at once | a program that is not deterministic |

| Level | What the interpreter runs |
|---|---|
| `-O0` | Raw MIR |
| `-Og` | HIR folds; `sroa`, `phi_sroa` |
| `-O1` | HIR small inlining; SCCP, InstCombine, DestProp, `sroa`, `phi_sroa` |
| `-O2`, `-O3`, `-Os` | The HIR passes of the level; every MIR pass but `promote_fields` |

Generators, bodies with a cleanup pad and debugger or coverage builds keep their
MIR as lowered at every level, as for native code.

What still happens to any body is lowering, not optimization: `native_twins` and
`native_expand` replace some host calls with Coil code in every capture.

`scripts/mir_diff.py` compares program by program (stdout and exit status, with
timings masked). `tests/mir_interp_cli.rs` does the same for stderr on a few
programs, and checks which levels run a loop as a vector op.

## How it runs

It is one more backend of the native ABI (`docs/internals/native.md`).
`prepare` turns captured MIR into `NativeEntry`s, as `coil_native::jit` does, and
each op does what Cranelift's lowering does through the same `coil_native_*`
helpers. The driver, the collector, tasks, generators, threads and FFI need no
change. `NativeEntry::data` names the body, since every entry shares one function.

The difference is where frames live. An `Activation` holds them in its own
vectors, not on the thread's stack:

- A direct call pushes a frame and the loop goes on, so recursion costs no Rust
  stack. Only a call through the host (a function value, a callback) nests an
  activation.
- A tail call (`is_tail_call_inst`) takes the frame, as the VM's `TailCall`.
- Registers stay where they are, so a collection rewrites them in place. One
  `ROOTS_SCAN` record per activation hands the collector a callback: a call pays
  nothing for roots, and nothing is reloaded after one. `SITES_SCAN` does the
  same for backtraces.
- `RootMaps` is built the first time a collection finds a body on the stack, so a
  run with no collection computes none. The rule is native code's: a word lives
  until its last use, and a `gc::root` handle until the function returns.

Side tables (`Exec`: block positions, φ moves, loop headers) are built on a
body's first call. Sites are built eagerly, because `NativeCode` holds them.

## Op by op

Most ops map one to one onto Cranelift's lowering. These needed a decision.

| Op | Handling | Status |
|---|---|---|
| `ByteScan`, `VecLoop` | Only passes make them, so raw MIR has none. Above `-O 0` they run the runtime's lane code (`coil_native_vector`), as native code does; the raw run of the same source is their independent check | Settled |
| `PanicStore` | Only `promote_fields` makes it, which no capture runs; `prepare` refuses a body that holds one | Open (VM only) |
| `Call` to a tail target | Takes the frame | Settled |
| `Call` of a `drop` | Claims the finalizer first, as Cranelift | Settled |
| `GcBarrier`, `KeepAlive`, `ClearWord` | No code; they matter only through the root maps | Settled |
| `HomeLoad`, `HomeStore` | Homes follow the registers; a pad reads its frame's copy, which a runtime record roots | Settled |
| `Yield`, `ResumeCoro` | `coil_native_yield` and fibers, as native code | Open (wasm) |
| `CodePtr`, `Alloc` of `Fn` / `PolyFn` / `Coro` | The word is the body's bytecode entry PC | Open (no bytecode) |
| `Deopt` | No code; only tests build one | Open (debugger) |
| `HostInvoke`, `Ffi` | The helpers. A native the runtime's table replaced goes through that table, so the comptime sandbox holds | Settled |

### Words with no static type

A two-word enum passed by value keeps its heap payload in an `int` typed word. A
body with such a parameter holds an object nothing marks as one, and a collection
in it moved the object. `MirFunc::heap_pair_words` now lists these parameters
beside call results, and `pair_words` follows the φs and casts that copy one. Both
engines root them ambiguously (pinned, never rewritten).

## Retiring the VM

This branch carries option C of the "Retiring the VM: options" doc (one MIR, two
executors): native code runs programs, this interpreter serves everything with
no machine code, and the VM goes. It lands here rather than on a later
`feature/one-engine`. Each step leaves the VM runnable until the last.

| Step | State |
|---|---|
| 1. MIR interpreter | Done: every corpus, every level, CI cells |
| 2. Glue as MIR | Mostly done already (below) |
| 3. Comptime and macros on the interpreter | Done, on a `Machine` that holds the runtime (below) |
| 4. JIT by default | Open |
| 5. Packaging without the VM | Open |
| 6. Debugger and coverage | Open: `Deopt` stops, names at a stop, MIR probes |
| 7. wasm | Open: `Yield` detaches its frame, a task parks an activation; then comptime leaves `Machine<256>` |
| 8. Delete the VM | Last: bodies by index, not bytecode PC |

**Step 2.** A clean compile panics on a body with no MIR (`require_mir`), and
thunks are built as HIR (`hir::synth`), so every function body already has MIR.
What is left is about 30 direct `bytecode.push_*` sites in `codegen`, not yet
sorted into prologue steps and dead paths, plus the startup region the driver
walks.

**The MIR archive format** is in place for steps 3, 4 and 5, which all need MIR
on disk: the compile-time program cache stores `CompiledExpansion`, and `.ccoil`
becomes MIR and data only. `compiler/src/mir/archive.rs` builds for wasm and
carries a body whole. `COIL_MIR_ARCHIVE_CHECK=1` makes `prepare` send every body
through it, assert it comes back equal and run that copy; CI's `mir` cell runs
the corpus so. The section is the larger part of a `--jit` archive (125 KB
against 18 KB of bytecode on `examples/perf/vec_lanes.coil`), because locations
and tables are stored as they are; step 5 will want to look at that.

**Step 3.** Macros, consts and call folding run on the interpreter.

- **Capture.** A `CompiledExpansion` carries raw MIR. An expansion program takes
  it from a `ComptimeCapture` of its own (a compile inside a compile saves the
  outer capture), and the disk cache stores it in the archive format. Const
  evaluation runs in the middle of a compile, so it reads the open capture without
  ending it: `Pipeline::comptime_capture` opens one before the bodies are lowered,
  unless a native build already has one. Evaluation then runs that build's MIR, at
  its level. A `ComptimeCapture` adds no twins: the interpreter runs a twinned
  native as the host call it is, so a plain compile's bytecode does not change.
- **Host.** `call_limited` calls `Machine::call_on_driver`: a driver over the
  machine's `Runtime`. Natives, statics, snapshots, output capture and reuse
  between calls stay as they were, because they live in the `Runtime`. It is a
  driver and not `install_native` under the VM because native code under a machine
  never collects, so a macro that allocates would never free anything.
- **Limits.** The budget and the cap live in the `Runtime`. The interpreter
  charges a step per call and per edge back in block order, as the VM does, and a
  zero stops the call with `TRAP_BUDGET`. Cranelift's code charges nothing:
  nothing with a budget runs on it. `collect::over_cap` serves both hosts.
- **Sandbox.** `coil_native_host` used to call a direct function from the global
  table, past the runtime's own, so a macro read the environment with no grant.
  It now asks the runtime's table, and a native with no direct function there goes
  through the table, which holds the sandbox's stubs. This fails closed: a host
  that replaces a native needs no other step. FFI goes through `Runtime::ffi_load`
  and its gate on both engines.
- **What the VM still runs.** A body the interpreter refuses (none in the corpus),
  every wasm build until step 7, and everything under `COIL_NATIVE_DRIVER=0`.
  `comptime::engine_calls` counts both, and `comptime/tests/macro_engine.rs` fails
  on any fallback, from a fresh compile or from the disk cache.

`COIL_NATIVE_DRIVER=0` is the A/B switch: compiling with and without it must give
the same archive, apart from the folds below.

**Folds at the limit.** Steps are counted on raw MIR, which has more of them than
the VM's optimized bytecode: `N` iterations of a `while` take `N` back-edges there
and `N - 1` once the loop is rotated. A call right at `SILENT_CALL_STEPS` used to fold and now stays a call,
with E0138. The value is the same; five of the 365 examples changed this way.

**`Machine<256>` stays until step 7.** The host's other work is already on the
`Runtime`; what is left is only the type that holds it. wasm needs the VM until
then, so swapping it now would mean two hosts. Step 7 ends with comptime on a
holder that has no VM in it, and with the fallback to `call_function` gone, so
step 8 deletes the VM with nothing in comptime depending on it.

**Decisions for steps 4 and 5** (the options doc's 3 and 4):

- Packages without AOT, and spool `--vm`, become a **JIT runner**: `coil-rt`,
  Cranelift and a MIR payload, with no linker. This replaces the earlier plan for
  spool to keep VM packaging long-term.
- `.ccoil` stays, as a **MIR archive that JITs on load**, so archives are portable
  across hosts and `coil run x.ccoil` keeps working.

Decision 5 (keep the VM-observable rules, or define them anew on MIR) is open.
Until it is made, today's rules stand and tests do not change.

## Open problems

- **Speed.** It runs `MirInst` directly. On a call and loop mix (`fib(38)` and a
  200M iteration loop) raw `-O 0` took 7.4 s, the VM at the default level 3.2 s
  and the JIT 0.7 s. The doc says speed is not a goal; comptime's step limits
  count steps, not time.
- **Words with no static type.** Three bugs came from an `int` typed word holding
  an object: a pair parameter, a `Vec` expansion's element, and a φ of an object
  and a number. `pair_words` now covers each, but the rule is a list of known
  cases. A type for "word that may be an object" in MIR would end the family.

## Where the pieces are

| Piece | Place |
|---|---|
| Crate | `coil-mir-interp/`: `lib.rs` (`prepare`, side tables), `exec.rs` (the loop), `frame.rs` (activations, roots) |
| Raw capture | `compiler/src/mir/native_capture.rs` (`start_raw_capture`) |
| Frame word analyses, shared with Cranelift | `compiler/src/mir/frame_words.rs` |
| Scan records, `NativeEntry::data` | `machine/src/native.rs` |
| MIR on disk | `compiler/src/mir/archive.rs` |
| Tests | `tests/mir_interp_cli.rs`, every corpus through `--engine mir` |
