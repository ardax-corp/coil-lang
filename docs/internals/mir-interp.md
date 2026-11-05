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
- `RootMaps` (liveness and held words) is built the first time a collection finds
  a body on the stack, so a run with no collection computes none.

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
| `HostInvoke`, `Ffi` | The helpers, every native allowed | Open (comptime) |

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
| 3. Comptime and macros on the interpreter | Next |
| 4. JIT by default | Needs the MIR archive format |
| 5. Packaging without the VM | Needs the MIR archive format |
| 6. Debugger and coverage | Open: `Deopt` stops, names at a stop, MIR probes |
| 7. wasm | Open: `Yield` detaches its frame, a task parks an activation |
| 8. Delete the VM | Last: bodies by index, not bytecode PC |

**Step 2.** A clean compile panics on a body with no MIR (`require_mir`), and
thunks are built as HIR (`hir::synth`), so every function body already has MIR.
What is left is about 30 direct `bytecode.push_*` sites in `codegen`, not yet
sorted into prologue steps and dead paths, plus the startup region the driver
walks.

**The MIR archive format comes first.** Steps 3, 4 and 5 all need MIR on disk:
the compile-time program cache stores `CompiledExpansion`, and `.ccoil` becomes
MIR and data only. `coil_native::section` is the start of it, but it lives in a
crate that is empty on wasm, stores only what `coil_native::plan` admits, and
leaves out `opaque_words` and `local_binds`. It should move beside MIR and carry
every body whole.

**Step 3.** The three places that build a `CompiledExpansion` (a fresh
expansion, one from the disk cache, const evaluation) need its MIR. A compile
inside a compile already saves the outer capture (`without_native_capture`). The
order that keeps the corpus green at each step:

1. Carry raw MIR in `CompiledExpansion` and install it on the comptime `Machine`
   with `install_native`: bodies run on the interpreter, the rest of the host
   stays.
2. Fuel counted per block and call (the step budget), and the heap cap.
3. Replace `Machine<256>` with a driver host: the sandboxed native table
   (`native_allowed_at_compile_time`, `io_gate`), statics, `restore_snapshot`
   and `SnapRoot`, output capture, and reuse between calls.

## Open problems

- **Harness SIGSEGV.** 2 of 18 corpus runs of one grid died on a reactor thread
  (`-O0` native, `-O2` interpreter); an earlier grid of 18 and about 50 reruns of
  the same seeds had none. The fault is a fiber switch to a stack that is no longer mapped,
  inlined into `collect::collect::<Driver>`, with the current stack pointer on a
  different stack from the one being left. It is not in the interpreter, and the
  branch does not touch the fiber or collector code, but whether `feature/native`
  has it too is unknown.
- **Cranelift relocation panic.** One `--engine native` run aborted in
  `cranelift-jit` (`compiled_blob.rs`, a relocation out of range). `jit` leaks
  each module, so a harness that compiles thousands of files spreads them past
  a 32-bit reach. Step 4's code cache has to settle this.
- **GC stress.** `tests/comptime/drop_not_folded.coil` fails on native and the
  interpreter at every level and passes on the VM.
- **Speed.** It runs `MirInst` directly. On a call and loop mix (`fib(38)` and a
  200M iteration loop) raw `-O 0` took 7.4 s, the VM at the default level 3.2 s
  and the JIT 0.7 s. The doc says speed is not a goal; comptime's step limits
  count steps, not time.

## Where the pieces are

| Piece | Place |
|---|---|
| Crate | `coil-mir-interp/`: `lib.rs` (`prepare`, side tables), `exec.rs` (the loop), `frame.rs` (activations, roots) |
| Raw capture | `compiler/src/mir/native_capture.rs` (`start_raw_capture`) |
| Frame word analyses, shared with Cranelift | `compiler/src/mir/frame_words.rs` |
| Scan records, `NativeEntry::data` | `machine/src/native.rs` |
| Tests | `tests/mir_interp_cli.rs`, every corpus through `--engine mir` |
