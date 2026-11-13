# MIR interpreter

`coil-mir-interp` runs MIR directly. It is the engine for what has no machine code:
the playground, the debugger, comptime and macros. Cranelift runs everything else
from the same MIR.

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
| Native code at `-O N` against `--mir -O N` | Cranelift's lowering |
| Every engine against the reference, at once | a program that is not deterministic |

| Level | What the interpreter runs |
|---|---|
| `-O0` | Raw MIR |
| `-Og` | HIR folds; `sroa`, `phi_sroa` |
| `-O1` | HIR small inlining; SCCP, InstCombine, DestProp, `sroa`, `phi_sroa` |
| `-O2`, `-O3`, `-Os` | The HIR passes of the level; every MIR pass |

Generators, bodies with a cleanup pad and debugger or coverage builds keep their
MIR as lowered at every level, as for native code.

What still happens to any body is lowering, not optimization: `native_expand`
writes some host calls out as MIR in every capture.

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
- A tail call (`is_tail_call_inst`) takes the frame.
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
| `Call` to a tail target | Takes the frame | Settled |
| `Call` of a `drop` | Claims the finalizer first, as Cranelift | Settled |
| `GcBarrier`, `KeepAlive`, `ClearWord` | No code; they matter only through the root maps | Settled |
| `HomeLoad`, `HomeStore` | Homes follow the registers; a pad reads its frame's copy, which a runtime record roots | Settled |
| `Yield`, `ResumeCoro` | `coil_native_yield`: on a fiber as native code does, or parked on the heap where there are no fibers (below) | Settled |
| `CodePtr`, `Alloc` of `Fn` / `PolyFn` / `Coro` | The word is the body's identity (`label.0 + 1`) | Settled |
| `Probe` | Counts through `coil_native_probe`, as Cranelift's code does | Settled |
| `HostInvoke`, `Ffi` | The helpers. A native the runtime's table replaced goes through that table, so the comptime sandbox holds | Settled |

### Words with no static type

A two-word enum passed by value keeps its heap payload in an `int` typed word. A
body with such a parameter holds an object nothing marks as one, and a collection
in it moved the object. `MirFunc::heap_pair_words` now lists these parameters
beside call results, and `pair_words` follows the φs and casts that copy one. Both
engines root them ambiguously (pinned, never rewritten).

## Engine plan

The plan was one MIR, two executors: native code runs programs, this interpreter
serves everything with no machine code. The bytecode VM is deleted.

| Step | State |
|---|---|
| 1. MIR interpreter | Done: every corpus, every level, CI cells |
| 2. Glue as MIR | Done: thunks are built as HIR (`hir::synth`), a clean compile panics on a body with no MIR (`finalize_bodies`) |
| 3. Comptime and macros on the interpreter | Done (below) |
| 4. JIT by default | Done: `coil FILE`, `coil compile`, `coil run` and `coil test` |
| 5. Packaging | Done in coil: `coil-embed` is the JIT runner. AOT links on Windows and CI smoke-tests JIT and AOT packages on macOS and Windows (#2298) |
| 6. Debugger and coverage | Done (below) |
| 7. wasm | Done: the interpreter runs programs, tests and comptime in `coil.wasm`, generators and tasks park on the heap, and comptime runs on an `Isolate` (below) |
| 8. Delete the VM | Done: bodies by identity, not bytecode PC; archive major 7, MIR and data only ([archive-history.md](archive-history.md)) |
| 9. Start-up | Done: a [code cache](native.md#code-cache) for repeat runs, and [parallel compiling](native.md#compiling-in-parallel) for first runs, in place of lazy compile, which would tax every hot call |

**The MIR archive format** (`compiler/src/mir/archive.rs`) builds for wasm and carries
a body whole. `COIL_MIR_ARCHIVE_CHECK=1` makes `prepare` send every body through it,
assert it comes back equal and run that copy; CI's `mir` cell runs the corpus so. The
compile-time program cache stores `CompiledExpansion` in the same format.

**Comptime.** Macros, consts and call folding run on the interpreter.

- **Capture.** A `CompiledExpansion` carries raw MIR. An expansion program takes it
  from a `ComptimeCapture` of its own (a compile inside a compile saves the outer
  capture). Const evaluation runs in the middle of a compile, so it reads the open
  capture without ending it: `Pipeline::comptime_capture` opens one before the bodies
  are lowered, unless a native build already has one. Evaluation then runs that build's
  MIR, at its level.
- **Host.** `call_limited` calls `Isolate::call`: a driver over the isolate's
  `Runtime`. Natives, statics, snapshots, output capture and reuse between calls live
  in the `Runtime`.
- **Limits.** The budget and the cap live in the `Runtime`. The interpreter charges a
  step per call and per edge back in block order, and a zero stops the call with
  `TRAP_BUDGET`, unless `coil_native_refuel` gives it more: a speculative auto-par
  job runs on slices of `thread::FUEL_SLICE` steps and gets another until it is
  cancelled ([auto-par](auto-par.md#cancelling-a-failed-attempt)). Cranelift's code
  charges nothing, so what has a budget runs here: comptime, and every `coil mutate`
  run. `collect::over_cap` serves both hosts.
- **Sandbox.** `coil_native_host` asks the runtime's table, and a native with no direct
  function there goes through the table, which holds the sandbox's stubs. This fails
  closed: a host that replaces a native needs no other step. FFI goes through
  `Runtime::ffi_load` and its gate on both engines.
- **Nothing else runs it.** Comptime has no second engine: a body the interpreter
  refuses (none in the corpus) fails the call.

**Folds at the limit.** Steps are counted on raw MIR: `N` iterations of a `while` take
`N` back-edges. A call right at `SILENT_CALL_STEPS` stays a call, with E0138.

**`machine::Isolate`** is what comptime holds: a `Runtime`, the program's tables and a
call that opens a driver.

**Debugger and coverage.**

- **Stops.** The program runs raw MIR on a thread of its own, on the `DEBUG`
  instantiation of the loop (`coil_mir_interp::debug`). A stop waits on that thread
  with the frames as they are, so nested activations, generators and task fibers need
  nothing special, and `DebugSession` keeps its pull API.
- **Op numbers.** `prepare_debug` numbers every op and builds a `ProgramDebug` over
  them, for breakpoints, `DebugController`, `LineIndex`, the REPL and DAP.
- **Locals.** At each op the loop notes what the op before bound, by `local_binds`.
  Codegen leaves a debugged program's variables on their own slots, which are the
  `LocalId`s.
- **Coverage.** A coverage build's captured bodies get a `Probe` per source location
  (`mir::coverage`), on either engine.

**Standing decisions.**

- `.ccoil` is a **MIR archive that JITs on load**, so archives are portable across
  hosts and `coil run x.ccoil` keeps working.
- **The first run compiles everything.** A repeat run loads its code from the cache;
  a new program pays for all its bodies once.

**Parking.** wasm has no stacks to switch, so nothing can leave a frame on one. The
interpreter's frames are already on the heap: a yield or a task's wait *parks* the
activation instead.

- A park rides the error path. The helper sets `ctx.park` and bails, `check!()` gives
  `Flow::Trap`, and `unwind` sees the park and keeps the frames: `finish` boxes the
  `Activation` and the call ends as `NativeOutcome::Parked`, a `ParkedCall` whose
  `resume` goes on with a word (`RESUME_VALUE`), the op again (`RESUME_RETRY`), a close
  or a cancel.
- A generator keeps its `ParkedCall` in its `GenFiber`, and its root words in
  `saved_stack`, so the collector sees it as it sees a fiber's. A task's waits on the
  driver's `parked_tasks`, and `host_roots` visits them.
- A wait inside a generator parks twice. The generator parks with `PARK_WAIT`, and
  `GenStep::Waiting` makes the op that resumed it park with `PARK_BELOW`, up to the
  task. The wake re-runs each resume op, with the wake in `pending_wake`, until the wait
  takes it. Another resume of that generator meanwhile finds no wake and is refused.
- Closures run as interpreter frames (`rt::apply_value`), not through a nested call, so
  a yield or a wait inside one parks like any other.
- What cannot park: a host frame between the task and the wait, a native callback,
  panics with "a task cannot wait inside a native callback here". A wait in a `defer`
  pad runs the scheduler nested, and so does the root's wait.

`machine::native::parks()` is true on wasm, and natively under `COIL_NO_FIBERS=1` with
`--engine mir`, which is how a parking bug gets a native debugger. CI runs that as the
`mir-parked` cell and in GC stress. Cranelift's code cannot park.

`coil compile` on wasm stores every body in the MIR section
(`coil_mir_interp::encode_section`), so a wasm `.ccoil` runs on the interpreter and on
native code.

## Open problems

- **Speed.** It runs `MirInst` directly. On a call and loop mix (`fib(38)` and a
  200M iteration loop) raw `-O 0` took 7.4 s and the JIT 0.7 s. Speed is not a goal;
  comptime's step limits count steps, not time.
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
