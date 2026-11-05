# MIR interpreter (`feature/mir-interp`)

`coil-mir-interp` runs MIR directly. It is to replace the bytecode VM as the
engine for what has no machine code: the playground, the debugger, comptime and
macros. Cranelift keeps running everything else from the same MIR.

```
coil prog.coil --mir              # raw MIR at -O 0: the reference output
coil prog.coil --mir -O 2         # raw MIR of an HIR-optimized compile
coil test --engine mir [PATH]     # a suite on it, -O 0 unless -O says otherwise
scripts/mir_diff.py [PATH ...]    # raw output against the optimized engines
```

## The reference output

The interpreter runs **raw MIR**: bodies as `from_hir` lowered them, with no MIR
pass run (`compiler::native_mir::start_raw_capture`). With `-O 0`, which also
leaves HIR unoptimized, that output is the reference. JIT and AOT start from the
same lowering and run `optimize_mir` on it, so they must give the same result.

`-O 0` alone is not raw on the other captures: `optimize_mir` skips the folds and
loop passes there, but `sroa` and `phi_sroa` run at every level. A bug in either
would show on both sides of a comparison, so the interpreter's capture skips
`optimize_mir` altogether.

What still happens to a raw body is lowering, not optimization: `native_twins`
and `native_expand` replace some host calls with Coil code in every capture.

`scripts/mir_diff.py` compares program by program (stdout and exit status, with
timings masked). `tests/mir_interp_cli.rs` does the same for stderr on a few
programs, and checks that a loop the JIT vectorizes stays scalar in the raw run.

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
- `RootMaps` (liveness and held words) is built the first time a collection finds
  a body on the stack, so a run with no collection computes none.

Side tables (`Exec`: block positions, φ moves, loop headers) are built on a
body's first call. Sites are built eagerly, because `NativeCode` holds them.

## Op by op

Most ops map one to one onto Cranelift's lowering. These needed a decision.

| Op | Handling | Status |
|---|---|---|
| `ByteScan`, `VecLoop`, `PanicStore` | Only passes make them, so raw MIR has none; `prepare` refuses a body that holds one | Settled |
| `Call` to a tail target | Takes the frame | Settled |
| `Call` of a `drop` | Claims the finalizer first, as Cranelift | Settled |
| `GcBarrier`, `KeepAlive`, `ClearWord` | No code; they matter only through the root maps | Settled |
| `HomeLoad`, `HomeStore` | Homes follow the registers; a pad reads its frame's copy, which a runtime record roots | Settled |
| `Yield`, `ResumeCoro` | `coil_native_yield` and fibers, as native code | Open (wasm) |
| `CodePtr`, `Alloc` of `Fn` / `PolyFn` / `Coro` | The word is the body's bytecode entry PC | Open (no bytecode) |
| `Deopt` | No code; only tests build one | Open (debugger) |
| `HostInvoke`, `Ffi` | The helpers, every native allowed | Open (comptime) |

### Words with no static type

A two-word enum passed by value keeps its heap payload in an `int` typed word. A
body with such a parameter holds an object nothing marks as one, and a collection
in it moved the object. `MirFunc::heap_pair_words` now lists these parameters
beside call results, and `pair_words` follows the φs and casts that copy one. Both
engines root them ambiguously (pinned, never rewritten).

## What is left

In the order they unblock the removal of the VM.

1. **Comptime and macros.** `CompiledExpansion` carries bytecode, and the host
   runs it on `Machine<256>`. It needs the program's MIR, a step budget in the
   loop, the heap cap, `native_allowed_at_compile_time` on host calls, and
   `SnapRoot` snapshots, which read only the heap.
2. **Debugger.** Stops and variables come from bytecode PCs and `debug_vars`.
   Raw MIR needs a stop per statement and a way from names to values at a stop.
   `MirInst::Deopt` with `DeoptSite::slots` was built for this and is unused.
3. **Tasks and generators on wasm.** There are no fibers there. A generator is one
   frame, so `Yield` can detach it; a task is an activation that parks. Frames
   already live outside the thread's stack, which is why this is possible.
4. **Function identity without bytecode.** Entry PCs still name bodies in
   `CodePtr`, `ObjFn`, `ObjCoroutine::resume_ip`, `finalizer_by_type`, `NativeSite`
   and the prologue the driver walks. They become body indices once nothing
   emits bytecode, and the startup steps need a form of their own.
5. **Coverage and `coil mutate`**, which count what the VM runs.
6. **Speed.** It runs `MirInst` directly, about 2x the VM's time on a call and
   loop mix before the raw capture. A flat decoded form is the next step, if
   comptime needs it.

## Where the pieces are

| Piece | Place |
|---|---|
| Crate | `coil-mir-interp/`: `lib.rs` (`prepare`, side tables), `exec.rs` (the loop), `frame.rs` (activations, roots) |
| Raw capture | `compiler/src/mir/native_capture.rs` (`start_raw_capture`) |
| Frame word analyses, shared with Cranelift | `compiler/src/mir/frame_words.rs` |
| Scan records, `NativeEntry::data` | `machine/src/native.rs` |
| Tests | `tests/mir_interp_cli.rs`, every corpus through `--engine mir` |
