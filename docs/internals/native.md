# Native code (`feature/native`)

Coil can run functions as machine code made by
[Cranelift](https://cranelift.dev) from their MIR. A program whose bodies
all compile runs on the native driver with no interpreter, and an AOT
executable (`coil package`) links no bytecode VM at all. Comptime and macros
run on the MIR interpreter (`docs/internals/mir-interp.md`). The VM stays for
`coil debug`, coverage, wasm, `--no-aot` packages, and `coil --jit` runs of a program with a body native code refuses. The
plans (native compilation, then native without the VM) are in the
project's plan docs; this page describes what is implemented.

## Using it

```
coil prog.coil --jit          # compile, JIT what it can, run
COIL_NATIVE_STATS=1 coil prog.coil --jit   # counts on stderr
COIL_NATIVE_STATS=2 coil prog.coil --jit   # plus why each body stays on the VM
```

The stats end with the run's dependence on the VM:
`native: N calls ran on the VM, M VM entries from native code`. `N` counts
`CALL`s the VM executed while native code was installed (bodies that did
not compile, re-runs after a bail); `M` counts bytecode executions started
under a native frame (`VM_ENTRIES`, counted in `run_execute` while
`native_roots` is non-empty). Every program in `examples/` and
`examples/perf` reports `0` and `0`: module setup and `main` are native,
and no bytecode runs but the prologue's few ops.

The prologue reaches `main` by a `JMP` (after a `CALL` with no target, or
at the end of the setup region), never by a `CALL`. With native code
installed, `Machine::ensure_host_stubs` points that jump at an appended
`CALL main; RETURN` (`main_entry_jmp`), so `main` enters natively like any
call; the run executes the loaded image.

### Module setup

Module setup is a function, `__module_setup`
(`compiler/src/codegen/module_setup.rs`): the stores of the statics
computed at run time, each `extern` library's load and declarations
(`FfiLoad` / `DeclareFFI`, a failure panicking with the error's message)
but not the finalizer registry (`gc_register_finalizer` per class with a
`drop`): that is `__finalizer_setup`, the prologue's first call, since a
collection during any initializer must find the `drop` of what it made. Codegen records the steps (`SetupStep`) as it emits them and
builds the function's MIR from them, so the register tiers emit it and
native code compiles it like any body. The setup region the prologue
jumps to is `CALL __module_setup; POP; JMP main`, after the calls of the
initializers computed while compiling (`const`s and pure `static let`s):
those stay in the region because `Pipeline::compute_consts` replaces
their `CALL init; StoreStatic` pairs with one `RestoreSnapshot`; one that
cannot be computed keeps its call, which reaches the initializer's native
entry. Native code registers a finalizer through the machine
(`HostOp::RegisterFinalizer` in `Machine::native_invoke`).

An archive or package with native code keeps byte spans at `-g1`
(`Pipeline::set_runs_mir`), and so does a direct run: native code finds a panic's location by span.

`--jit` skips the build cache (it needs this compile's MIR).

```
coil compile --jit prog.coil -o prog.ccoil   # archive with a native MIR section
coil run prog.ccoil                          # JITs the section, then runs
coil run --no-jit prog.ccoil                 # VM only
```

`coil run --jit` on an archive without the section warns and runs on the
VM. `coil package` compiles ahead of time instead (see [AOT](#aot-coil-package)).

The MIR interpreter (`coil FILE --mir`, [mir-interp.md](mir-interp.md)) is
another engine on this same ABI, and its raw `-O 0` output is the reference
native code is compared against.

### Testing against the VM

```
coil test --engine native [PATH]
```

runs a suite with native code: each file's MIR is captured and JIT
compiled, and its cases (and the machines they spawn) run it. A suite must
pass on both engines; CI's `harness native` cell runs every corpus this
way. `--coverage` needs `--engine vm` (it counts what the VM runs).

With `COIL_NATIVE_INVENTORY=FILE` the run appends what it left on the VM:
per compiled file the bodies compiled and refused (with each reason), and
per run the VM entries, bails, calls the VM ran and host calls in a VM
stub (`machine::native::vm_use_counts`). `scripts/native_inventory.py FILE
[--exe PACKAGED] [--max KEY=N]` sums them, groups the refusals by reason,
counts the interpreter symbols (`machine::vm`) of a packaged executable,
and fails over a limit. The `harness native` cell writes the report to its
job summary and uploads the file (`native-inventory`); the steps that take
the VM out of native binaries drive every total to 0.

### The native MIR section

`ArchivedProgram::native_mir` (archive 6.20; 6.0 to 6.19 archives load
with it empty) holds bytes in the MIR archive format
(`compiler/src/mir/archive.rs`): the magic `CNMIR`, a format byte, then each
body with its name, bytecode entry PC and entry label. `coil_native::section`
chooses the bodies: those the backend compiles, and a stub of each body they
call on the VM. A full body is stored whole, every field and instruction, and
comes back equal; `COIL_MIR_ARCHIVE_CHECK` runs a corpus on the copies that
come back. A reader refuses any other format, a truncated section or trailing
bytes, and the run falls back to the VM with a warning. The section is
compiled again on every run: the archive carries MIR, not machine code.

## Numeric tier (step 2)

A body compiles when:

- every parameter, value and the (one-word) result is `int`, `float` or
  `bool` (`MirTy::I64` / `F64` / `Bool`), or a heap word (below);
- its instructions are `Const`, `Bin` (not int `**`), `Cmp`, `Unary`,
  `Cast`, `Phi`, direct `Call`, and its terminators `Jump`, `Br`,
  one-word `Return`, `Unreachable`;
- every callee compiles too (a fixpoint over the call graph);
- it has no emission refusal and no opaque words (a `defer` compiles
  with its cleanup pad since step 5).

Such a function is **pure**: it reads nothing but its arguments and does
nothing but return a word. That is what makes the tier simple and exact.

### Heap words (step 4)

A body may also hold heap words (`HeapRef`, `NicheOpt`, `NicheRes`): take
them as parameters, pass them to calls, return them, move them through
phis, read through a `HeapRef` with `Index`, `ArrayLen` and
`HeapFieldLoad`, and write with `StoreIndex` and `HeapFieldStore`. Each
calls a helper in `machine::native` (`coil_native_index`, `_len`,
`_field`, `_store_index`, `_set_field`) that does what the VM instruction
does; an index out of bounds or a store to the wrong kind of object
bails. Native code never calls into the VM and its helpers never collect
(the collector moves objects), so every heap word it holds stays a live
object at the same address for the whole call.

An array a body indexes gets a *view* where its value is defined (a
parameter, a phi, a load): `coil_native_view` hands back the pointer and
length of its element words, and `Index` / `StoreIndex` / `ArrayLen` in
bounds of the view read and write in place. Byte arrays, tuples and an
index out of the view take the helpers. Storing into a word array never
moves its storage; a push may, and another value may name the same array,
so after an `ArrayPush` every view reads through the helpers until its
value is defined again.

Stores are effects, and a bail re-runs the call. So every store logs the
word it overwrote in the context's undo log (`undo_buf` / `undo_len` /
`undo_cap`; in-place stores append inline and call
`coil_native_undo_grow` when it is full), and a bail puts them back, newest
first, before the VM runs the call again: the re-run starts from the heap
as the call found it, and makes the stores (and the panic, if any)
itself. A push logs a pop.

`Cow` (share, unique, or unique of a field), `Alloc` (arrays, tuples,
enums, and class instances whose fields stores fill) and `ArrayPush` call
`coil_native_cow`, `_cow_field`, `_alloc` and `_array_push`, which build
objects as the VM's `Cow`, `DenseMake`, `DenseMakeObject` and `ArrayPush`
do, with the same word kinds. They allocate through the machine's heap
(`NativeCtx::heap`) but never collect: a `GcBarrier` only names the
object made right before it, and the VM checks for a collection after a
native call that allocated, with the result on its stack. A call that
makes more than `NATIVE_ALLOC_CAP` objects while a collection is due
bails, so the VM's re-run collects as it goes and native code cannot
grow the heap without bound.

More instructions follow the same pattern, each a helper that does what
the VM's instruction does:

| MIR | Helper | Note |
|---|---|---|
| `JumpIfMatch` on a boxed enum, `MatchPayload` | `coil_native_enum_tag`, `_enum_payload` | a niche word (`Option`: `0` is `None`; `Result`: low bit is `Err`) is tested inline and is its own payload |
| `String` | `coil_native_string` | the machine's string table and cache travel in `NativeCtx` |
| `StrEq` | `coil_native_eq` | `value_eq::values_eq`, as `EQ` |
| `Stringify` | `coil_native_stringify` | |
| `Format` | `coil_native_format` | `vm::render_format` is shared with the VM's `FORMAT` |
| `FreeEarly` | `coil_native_free_early` | queued: see below |
| `Print` | `coil_native_print` | commits |
| `HostInvoke` | `coil_native_host` | direct natives only; commits |
| `Call` of a body on the VM | `coil_native_call_vm` | commits; see below |
| `ClearWord`, `GcBarrier` | none | nothing to zero or root while no collection can run |

An early free cannot be undone, so before a commit native code only
queues it (`NativeCtx::frees`) and the VM frees after the call returns; a
bail drops the queue. When the program has `drop` finalizers an early free may
run one (VM code), so `FreeEarly` bails instead and the VM runs it.

A body that returns two words (a two-slot `Option<int>`, a pair) returns
two Cranelift values; the entry leaves the second in `NativeCtx::ret_hi`
and `CALL` pushes both (`NativeEntry::ret_words`).

### Commit mode (step 5)

Some effects no undo log can take back: a `PRINT`
(`coil_native_print`), a direct host native (`coil_native_host`: the
plain `HostValueFn` behind a native that never parks, opens descriptors
or switches tasks, from `machine::native::direct_host_natives`; ids are
frozen, so the backend resolves them at compile time), and a call to a
body that stays on the VM (`coil_native_call_vm`). Each sets
`NativeCtx::committed` before it acts. From then on the call never bails:

- A trap panics natively (below).
- Past the stack budget it goes on down to `NativeCtx::stack_hard`, 128
  KiB above the thread's real stack end (`stacker::remaining_stack`), then
  panics with the VM's stack overflow message.
- Instead of the allocation cap it collects at safepoints (below). A call that
  reaches the cap before a commit with a collection due commits there
  (`note_alloc`) rather than bailing.
- An early free happens at once (`NativeHost::free`); the undo log stops
  growing.

What still bails after a commit (a shape native code does not model, an
early free when the program has finalizers) panics with a message saying
so; the corpus never reaches it.

A host native whose first argument is a `Stream` keeps its unsent-stream
check: native code makes it and raises the VM's panic message
(`TRAP_MESSAGE`).

### Native panics

Every bail path goes through `coil_native_unwind(ctx, code, site)`. A trap
that is a Coil panic (`TRAP_OVERFLOW`, `TRAP_DIV_ZERO`, `TRAP_NEG_EXP`,
`TRAP_SHIFT`, `TRAP_OUT_OF_BOUNDS`, `TRAP_MESSAGE`, `TRAP_EXPLICIT`:
`panics_natively`) commits on the spot even before the call's first
effect, so it panics natively instead of handing the call to a VM re-run.
Other codes (an unmodelled shape, the stack budget, the allocation cap)
set the bail flag before a commit. After, the first frame records the trap
(`TRAP_OVERFLOW`, `TRAP_DIV_ZERO`, `TRAP_SHIFT`, `TRAP_OUT_OF_BOUNDS`,
`TRAP_STACK`, …) and every frame on the way out appends its *site*: an
index into `NativeCode::sites`, the function's entry PC and the MIR op's
source location (`MirFunc::op_loc`, carried in the native MIR section).
`Machine::native_panic` finds the PC in that function whose debug location
matches, so the message, its ` at file:line:col` and the backtrace lines
(`in f at …`, repeats folded) are the VM's own, with the native frames
above the VM frames. The panic then unwinds the VM frames as any runtime
panic does.

### Calls into the VM and collections

A function value the VM calls (`call_value`, `call_function`: a closure
passed to a host native, a finalizer the collector runs, a callback) whose
body is native runs natively: `native_call_entered` pops the frame the VM
just opened and tries the native table, putting the frame back if the
body stays on the VM.

A call to a body that does not compile is no longer a reason to refuse the
caller: native code runs it through `Machine::call_function` (a nested
execution; a two-word `RETURN` leaves its second word in
`nested_return_hi`). If that run panics, the VM has reported it; native
code unwinds with `TRAP_VM_PANIC` and the VM goes on unwinding from the
`CALL` (`rearm_unwind_after_early_drop`). The backtrace then lacks the
native frames in between.

A collection can run inside such a call, and at a *safepoint*: the entry
and loop headers of a body that may allocate, taken when an allocation
found a collection due (`NativeCtx::gc_due`).

Native frames hand the collector their heap words through Cranelift stack
maps. Every value of a heap type (and every `int` value a heap word moved
into, `pointer_words`) is declared `declare_var_needs_stack_map`: Cranelift
keeps it in a stack slot across each call and records, per call's return
address, which slots hold live heap words. Liveness is Cranelift's, at last
use: a dead temporary or a local past its last use is not kept alive, so
native frames may let an object go earlier than the VM's frame does. A
`gc::root` handle is the exception: `keep_roots` uses each one before every
return, so an unused `let r = root(h)` keeps `h` until the function returns.

Code is compiled with frame pointers. Native code calls a helper that may
collect (`call_vm`, `call_value`, `resume`, `collect`, `free_early`, `ffi`,
`host`, `yield`) through a local trampoline that stores its frame pointer in
`NativeCtx::native_fp`. The collector walks from there: `[fp + 8]` is the
return address into the caller, `fp + 16` the caller's stack pointer at that
call, `[fp]` the caller's frame pointer. Each return address inside a
compiled body looks up its stack map (`machine::native::register_stack_maps`;
an AOT object carries them as `coil_aot_stack_maps`); the walk stops at the
first return address outside every body. The slots are precise roots: a
minor collection forwards them in place, and native code reloads them after
the call. A suspended task or generator keeps its context's `native_fp`, so
its frames are walked the same way.

Two kinds of word stay in a record linked into `NativeCtx::roots`
(`[next, n, words…]` on the stack): the heap homes, which live in memory
and are written back when `NativeCtx::gc_epoch` moved (array views are then
re-read), and the ambiguous words: the low word of a pair that may carry an
object, a call's result or a parameter (`MirFunc::heap_pair_words`, and the
φs that copy one: `pair_words`), and an opaque body's `int` words, which are
pinned and never rewritten.

### Host calls

A host native without a plain function (`write`, `await_writable`,
`thread_*`, `vec_insert`, ...: `DirectHost::via_vm`, the ordinary host
ops) runs in Rust through `NativeHost::invoke` (`Machine::native_invoke`):
the call commits, pushes `native_roots` (a shared-heap spawn can collect
inside it), and maps the result to a value, a panic (`TRAP_MESSAGE`, with
the VM's message) or an unwind the VM already reported (`TRAP_VM_PANIC`).
`gc_epoch` moves so native code reloads its words.

- Waits (thread park, io retry, pending io) park the fiber the call runs
  on (`native_park`, `native_io_wait`); with no scheduler the thread blocks
  in `host_io_wait`. A clock sleep inside a scope adds a timer and parks.
- Region enter/exit run `gc_region` with the native roots pushed.
- `task_*` ops go to `native_task_op`. Ops that never switch (2, 3, 5, 9,
  10, 12, 14, 15) always run there; close (45) when its target is a native
  generator (`native_close_target`); ops that suspend (0, 1, 4, 6, 7, 11,
  13) when the call runs on a fiber. A scope native code opens lives at
  its nesting level, as one opened by a VM body. A failed `requires`
  (`contract_fail`) names the native function's caller (`host_stub_frame`).
- FFI ops (`FfiLoad`, `DeclareFFI`, `FfiInvoke`) run in Rust
  (`native_ffi_vm`, sharing `ffi_load_top` / `ffi_invoke_take` /
  `declare_ffi_top` with the `exec_rest` handlers).

The *host-call stubs* (`native::host_stubs`, `Machine::ensure_host_stubs`:
`HostInvoke; RETURN` per enum layout and arity, run as a VM call through
`NativeHost::host`) remain only for a wait in code the VM itself runs one
nesting level down (a finalizer or FFI callback inside a scope), where the
stub's execution is the scheduler loop; neither the examples nor the
corpus reach them.

A scope opened under a task fiber gets its scheduler at the fiber's base
level (`fiber_base`), the level the task parks back to, even when native
code opened it from a deeper level (a VM call under native code, a
generator body).

### Coil twins of host natives

Some host natives are plain Coil over primitives the runtime keeps anyway.
The VM keeps them as natives; native code calls a Coil *twin* instead, so a
native build does not need them in `libcoil_rt`. The twins live in the
embedded module `native_twins` (`compiler/src/prelude/twins.coil`), which a
compile builds, and keeps live, only while a native capture is open.
`compiler/src/mir/native_twins.rs` holds the table (host native → twin);
`take_native_capture` turns each captured `HostInvoke` of a listed native
into a `Call` of its twin. A twin returns the word its native returns
(VM and native code share those values: a string's hash keys a map either
side), so only natives whose result is a plain word, not a host enum
layout, are listed. A `HostInvoke` with neither a twin nor a direct host
stays one, and the backend refuses it, so a gap shows in the inventory.

| Native | Twin |
|---|---|
| `hash_string` | `native_twins::hash_string` (FNV-1a, 32 bits) |
| `string_compare` | `native_twins::string_compare` |

### Vec natives written out as MIR

`vec_insert` and `vec_remove` return an `Option` or move elements of any
type, so a word-for-word twin does not fit them. In a
captured body `compiler/src/mir/native_expand.rs` writes each `HostInvoke`
of them out as the loop it stands for, over `ArrayLen` / `Index` /
`StoreIndex` / `ArrayPush`. Element words are copied as they are (as an
`i64` when the call does not say their type), so one expansion serves
every element type. `vec_from_array` stays a call: it is the copy
primitive, and keeps a packed byte or float source packed, which the MIR
array ops cannot ask for. Two more things stay calls: `remove` rotates the element to
the end and calls `vec_pop`, the shrink primitive, which also packs the
`Option` in the call's layout (a `None` is an `Alloc` of tag 0 when boxed,
the zero word when niche); and `insert` out of range calls `vec_insert`
itself, so the panic is the host native's, as the VM prints it (the
message alone, no Coil frames). A call whose operand or result types the
expansion does not cover stays a `HostInvoke`.

### Function values and indirect calls

`Alloc` of a function value (`Fn`, `PolyFn`, `PolyFnCapture`) builds the
`ObjFn` as `MakeFn` / `MakePolyFn` / `MakePolyFnCapture` do
(`coil_native_alloc`, `ALLOC_FN` / `ALLOC_POLY_FN`); the body it names
counts as a callee, so its entry PC is known (`Plan::pcs`, a stub in the
section when it stays on the VM). `CallIndirect` commits and runs the call
on the VM (`coil_native_call_value` → `Machine::call_value`): the VM's own
`call_indirect` handles exact calls, partial and over-application and
dictionaries, then runs the callee as `call_function` does.

### Drops

`ALLOC_OBJECT` / `ALLOC_DROP_ENUM` of a type with a `drop` log the new
object (`UNDO_BORN`): a bail marks it finalized, so the discarded object
owes no `drop` (the VM's re-run makes its own). A direct call of a body
that may be a `drop` (`Plan::drops`) claims the finalizer first, as the
VM's `CALL` does (`coil_native_claim`), skips a `drop` that ran, and logs
the claim (`UNDO_CLAIM`) before a commit.

### `defer`

A body with `defer`s compiles with its cleanup pad (`MirFunc::cleanup`,
a separate function `(ctx, *const u64 homes) -> u64`). Its homes live in
a zeroed stack slot; heap homes are rooted with the body's heap words.
The normal exits run the defers inline, as in MIR. After a committed
panic, each unwound frame's bail path hands its pad and a copy of its
homes to `coil_native_defer`; `Machine::run_native_pads` runs them once
the panic is reported, innermost first, before the VM unwinds its own
frames: the VM's order (panic and backtrace, then the defers inside out).
The pad's `unwind_resume` lowers to nothing. Before a commit nothing runs:
the VM's re-run runs the defers.

### Tasks

A task switch must not strand native frames on the thread's stack, so a
native body that may reach VM code (`NativeEntry::suspends`, from the
plan's `vm_reach` set) runs on a **fiber** wherever tasks can switch, and
for a body that reaches a suspending task op even without a scheduler
yet (`native_needs_fiber`: not wasm, not already inside a fiber, and no
scheduler or at task level) (`vm_fiber.rs`; `corosensei` coroutines, 8 MiB reserved stacks, reused).
Bodies that reach no VM code cannot suspend and run on the thread's
stack as before.

- The fiber's VM calls run on a VM state of its own (`ExecCtx`: operand
  stack, frames, nesting, native roots, unwind), swapped into the machine
  while the fiber runs (`Machine::swap_exec`). `fiber_switch_depth` is the
  nesting level its VM calls switch tasks at (`at_task_level`);
  `native_reenter` moves it one level down for each native → VM call, so
  a wait any number of native/VM levels deep can switch.
- A suspension point inside the fiber (`task_suspend_at`) parks it:
  `fiber_park` records the block, yields, and the `CALL` that started the
  fiber sees `TRAP_PARKED`. The task suspends there with `Retry`
  semantics (its arguments stay on the stack); when it runs again the
  `CALL` re-executes with `fiber_resume_pending` set and resumes the fiber
  instead. The wake goes to the fiber's result slot (`fiber_wake`).
- A cancel is taken when the fiber resumes (`resume_parked_fiber`, not
  `task_run_next`) and unwinds from the suspension point inside it: the
  fiber's defers, then its native frames' pads (`TRAP_VM_PANIC`), then the
  task's own frames. `rearm_unwind_after_early_drop` leaves a cancel
  without a message, so the task ends `Cancelled`, not `Panicked`.
- A switch fences its owner (`stacks::fiber_switch_fence`): what runs on the
  other side reaches the same object through a raw pointer, which `&mut`'s
  `noalias` does not account for.
- GC: a parked fiber's VM state and native roots, and the task's own
  state while a fiber runs, are scanned as ambiguous (pinned) roots
  (`for_each_fiber_word`). A parked fiber's executions stay counted in
  `IN_EXECUTE`, so nothing moves while one exists.
- Backtraces inside a fiber name its frames, its native frames, then the
  task's frames below the `CALL`.
- A task that never resumes (its scope torn down after a root panic)
  drops its fiber with `force_reset`: native frames have no unwind tables,
  so what they own leaks, as a VM coroutine dropped mid-way does.
- wasm has no fibers: there, under a scheduler, only bodies that reach no
  VM code run natively.

### Generators

Native code makes generators (`Alloc` of `MirAllocKind::Coro`, as
`MakeCoro`: `ALLOC_CORO`), checks them (`DoneCoro`, a heap read) and
resumes them (`ResumeCoro`). A body that stays on the VM runs from
`coil_native_resume` with
`Machine::resume_value`, a nested execution like `call_function`'s. A floor
frame stands for the resumer; the generator's frames go above it, and the
execution stops when a yield or the body's return comes back to that depth
(`resume_floor`, checked after `YieldCoro` / `YieldFromCoro` and in
`after_return_bookkeeping`). Delegation (`yield from`) and sends behave as
the opcode's. A resume of a finished or invalid handle panics at native
code's own site, before the VM runs anything. Inside a task fiber the
resume goes through `native_reenter`, so a generator body that waits can
switch tasks.

### Generator bodies

A generator's body compiles too (`NativeEntry::generator`, `AotEntry`
byte `generator`); no `CALL` enters it. Its first resume (VM `ResumeCoro`,
`Machine::resume_value`, or a `yield from` of a delegate already running
natively) makes a fiber for it (`vm_gen.rs`, `native_gen_step`) and runs
`native::call` on the generator's saved arguments there. `Yield` calls
`coil_native_yield`, which commits and suspends the fiber with the value
and its call's context. The resumer then copies that call's root words into
the generator's `saved_stack` (precise mask unless a record is ambiguous),
so the collector traces and forwards them like a VM generator's segment,
and the `ObjCoroutine` owns the fiber (`native`). The next resume writes
the words back, sends its word, and native code reloads after the yield
(`gc_epoch`). Its return finishes the generator; a VM generator that
delegated to it goes on (`finish_yield_from`). A bail can only come before
the first yield, and the VM then runs the body from the start.

While the fiber runs, the generator's handle is a pinned root
(`gen_running`) and VM code under it runs one nesting level deeper, so it
never switches tasks; a body that may run VM code therefore stays on the
VM when a scheduler exists. `yield from h` in a native body is a loop
of resume, done check and yield, the sends passed through (the delegation
links of the VM's `yield from` are not set). Refused: two-word returns.

A body with `defer`s compiles (`NativeEntry::cleanup`, `AotEntry`
byte `cleanup`). After each yield it is noted closeable. A close
(`coro_close`, `native_gen_close`) resumes the fiber with `GEN_CLOSING`
set; `coil_native_yield` sees it and unwinds with `TRAP_CLOSE`, the frames
hand their pads over as for a panic, and the pads run once the coroutine
is finished (`run_pads`), inside out. A panic in a pad is reported with
its location (pads are lowered with their body's entry PC) and the close
returns `TaskFlow::Reported`. After a yield `coil_native_yield` checks the
bail flag (`TRAP_HELPER`). A
generator collected while suspended drops its fiber unwound
(`GenFiber::drop`).

A fiber's VM state starts with a base frame (ip 0, as the program's
bootstrap frame), so VM calls on it open their frames as anywhere else. A
failed `requires` in a VM body native code called names native code's call
site (`native_caller_location`).

### MIR passes and vector loops

Each captured body (`compiler/src/mir/native_capture.rs`) first runs the
MIR passes the dense tier runs (`mir::specialize::optimize_mir`, at the
compile's `-O` level): SCCP, the loop passes, SROA, and the loop rewrites
`byte_scan` and `vec_loops`, and the bulk copy and fill rewrites
(`copy_until` and `bulk_push`). Native code then gets the same vector ops
and the same runtime kernels (`vec_extend_range` / `vec_fill`) as the VM:
an element-by-element copy loop in Cranelift was 3x slower on
`bytes_copy`. Two exceptions: `promote_fields` does not run (its `PanicStore`s would need
the native unwinder to write promoted fields back), and generators, bodies
with a cleanup pad, debugger and coverage builds keep their MIR as lowered.

A `ByteScan` or `VecLoop` runs the VM's own lane code (`vscan` / `vloop`)
through `coil_native_vector`. `compiler::native_mir::vector_op` encodes the
op as the VM's `VScan` / `VLoop` body over a frame of word slots (slot 0 the
loop's `dest`, which a reduce's or push's start value fills first, slot 1
its block position, then each operand); native code stores the words in a
stack slot, the helper pushes them as a frame on top of the VM stack, runs
the body and copies the slots back. A loop that writes the heap (a store or
push loop, a reduce with stores) commits the call first; a trap panics at
native code's own site with the VM's message. A push may move an array's
storage, so native code drops its element views after any vector loop.
`COIL_NATIVE_STATS` counts these ops (`native: N vector ops ran natively`).

### Bail instead of trap

Before a commit native code never raises a Coil panic. Where the VM would trap (int
overflow, division by zero, `MIN / -1`, a shift count outside `0..=63`,
`-MIN`) or where native code would leave its stack budget (256 KiB below the
VM's call), it sets `NativeCtx::bail` and returns. Every native caller checks
the flag after a call and returns too. The VM then runs the original call in
bytecode from its first instruction. The undo log takes back the call's
heap stores first (objects it made are garbage), so that re-run gives the
VM's own result, effects, panic message, location and backtrace.

After a bail, calls nested inside the VM's re-run (operand stack base above
the bailed call's) stay in the VM, so a deep recursion bails once, not once
per level (`NativeTable::quiet_above`).

### The native driver

A program whose native code covers it runs on `machine::driver::Driver`,
over the machine's `Runtime` with no interpreter: it walks the prologue's
startup ops, calls the entry natively and answers native code's host
requests itself (calls of bodies and function values, host natives,
collections over the native frames' records, early frees, finalizers,
tasks on fibers of their own, generators, threads and auto-par workers,
FFI callbacks). `Machine::with_native_driver` enters it when every body
compiled (`NativeCode::with_complete`; an AOT object exports
`coil_aot_refused_count`): `coil --jit`, an AOT executable and each
`coil test --engine native` case run on it, and `COIL_NATIVE_STATS` says
"the driver ran the program". With a refused body, or with
`COIL_NATIVE_DRIVER=0`, the VM runs the program, calling native code
per body, as before the driver.
The inventory's `vm_entries` is gated at 0.

An archive run (an AOT executable, `coil run` of a `.ccoil`) starts with
no `Machine` at all: `coil_cli::execute_archived_program` calls
`machine::run_natively` (`machine/src/start.rs`), which builds a `Runtime`
for the program and runs the driver over it, and builds a `Machine` only
when that returns `None` (incomplete code, or the driver off). The AOT
link passes `--gc-sections` (`-dead_strip` on macOS), so code nothing
references is left out of the executable. A reactor job (a thread, an
auto-par body, a harness case) runs the same way when its native code
covers the program: `machine/src/worker.rs` sets up a `Runtime` of the
worker's own (pooled per thread, reset between jobs) and calls the entry
on the driver, where `run_job_on_vm` would set up a `Machine`.

### Inline allocation

An `Alloc` of a fixed-size nursery object (an enum variant or tuple of 1 to
4 words, an instance of a class with 1 to 4 fields) does not call
`coil_native_alloc` on its fast path. Native code loads the heap's gate
(`HeapCounts::inline_gate`, closed while the nursery is off, a mark is
open, pretenuring after a mostly surviving minor, a steal epoch, and under
`gc-stress`), and for an instance tests its type's bit in the finalizer
types (a type with a `drop` stays old). It then bumps its size class's
cursor (`Bump`, boxed in the slab, indexed by a class fixed for every heap),
writes the slot's image with the payload words in place, and adds the
object to `alloc_bytes`, `young_bytes` and `live_count`. A full chunk, a
closed gate or a `drop` type falls back to the helper, which refills the
chunk and sets `gc_due` when a minor collection is due, so collections still
come at safepoints on time.

The images come from `machine::memory::inline_object`, probed from real
objects: `ObjEnum`, `ObjTuple`, `ObjInstance` and their payloads are
`repr(C)` / `repr(u8)`, so the compiler that writes the image and the
runtime that reads the object agree. An AOT object records
`layout_fingerprint` as `coil_aot_layout`; `install_aot` refuses an object
made against other layouts. `COIL_NATIVE_INLINE_ALLOC=0` at compile time
turns the fast path off, for A/B runs. Arrays, closures, generators and
`drop` enums always take the helper.

### VM ↔ native

`machine::native` holds the table: entry PC → `extern "C" fn(ctx, *const
u64) -> u64`. `CALL` checks it only when a table is installed
(`unlikely(self.native.is_some())`), and never while a debugger is
attached. The entry wrapper unpacks VM words (a `bool` word is true when
its low byte is `1`, as `Value::as_bool`; a `float` word is its bits), calls
the native body, and packs the result. Native-to-native calls are direct
Cranelift calls with the values in registers.

The table is installed on the program's main machine (`run_raw`) and
shared (`Arc<NativeCode>`) with the machines it spawns, each with its own
counters (`NativeTable`).

### Semantics kept equal to the VM

| Operation | Native |
|---|---|
| int `+ - *` | `sadd/ssub/smul_overflow`, bail on overflow |
| int `/` | bail on `b == 0` and `MIN / -1`; `sdiv` |
| int `%` | bail on `b == 0`; `x % -1` is `0`; `srem` |
| `<< >>` | bail unless `0 <= b < 64`; `ishl` / `sshr` |
| float `% **` | Rust `%` / `powf` through a helper |
| float `!=` | true when unordered (Rust `!=`), `==` / `<` ordered |
| `float as int` | `fcvt_to_sint_sat` (Rust `as`: saturating, NaN is 0) |

One rule differs on purpose: a heap value in a native frame is live until
its last use (Cranelift's liveness), where a VM frame holds a local's value
until the local is rebound or the function returns. A weak reference or a
finalizer can therefore see an object die earlier under native code. A
local holding a `gc::root` handle is the exception: `keep_roots` keeps it
alive until its function returns, as on the VM.

## AOT `coil package`

`coil package` links a native executable by default (`--no-aot` or
`--runner` keep the old runner + payload):

1. The build captures MIR as for `--jit`; `coil_native::aot()` lowers it
   with `cranelift-object` (PIC) into `program.o`. The object defines
   `coil_aot_entries` / `coil_aot_entry_count` (one `AotEntry` per
   compiled body, the function address relocated in) and
   `coil_aot_sites` / `coil_aot_site_count` (call-site words) and
   `coil_aot_refused_count` (bodies left out), and imports
   the runtime helpers by name (`machine::native::helper_symbols()`, all
   `#[unsafe(no_mangle)]`).
2. `cc` links `program.o` with `libcoil_rt.a` (the `coil-rt` staticlib):
   `COIL_RT_LIB`, else the copy embedded at the end of `coil`
   (`scripts/embed_runtime.py`, which the release workflow runs: the
   library, its length, `COILRT\0\1`), written once to
   `<cache>/runtime/<sha256 prefix>/` (`package_app::embedded_runtime`),
   else the file beside `coil` (a cargo build). `--strip-symbols`
   (`debug` by default, `none`, `all`) sets what the linker strips: the
   runtime's DWARF, then its symbol table. Coil's backtraces read the
   program's line table (`-g`), so stripping keeps them. `coil-rt`'s
   `main` calls `machine::native::install_aot` with those symbols, then
   runs the embedded payload like `coil-embed`.
3. The archive payload is appended as for a runner package. The driver
   runs it (`machine::run_natively`): `coil-rt` builds `machine` and
   `coil-cli` without their `vm` feature, so the executable holds no
   interpreter (CI gates `machine::vm` symbols at 0 on a packaged hello
   world). Build it in its own cargo invocation (`cargo build -p
   coil-rt`): a build with `coil` unifies `vm` back in.

A body native code refuses is an error naming it and why (`--no-aot`
packages the `coil-embed` runner, whose VM runs it). If the object or the
link fails (no `cc`, no `libcoil_rt.a`, Windows) the package warns and
falls back to the runner.

### Size and speed

Measured on feature/native 544b4ba (2026-11-05, x86_64 Linux, release
`coil` and `coil-rt`). A packaged hello world is 1.58 MB with
`--strip-symbols all` (1.85 MB by default) and holds no `machine::vm`
symbol; the `--no-aot` runner of the same program is 2.07 MB, as was the
AOT executable before the interpreter left it (8764f32). What remains is
the runtime the host-native table reaches (heap, driver, host natives,
FFI, threads, reactor, archive reader): `--gc-sections` cannot drop a
native the table names, so a smaller executable needs a per-program
native table.

Across the 18 cross-language benches, an AOT executable takes 0.37x the
wall time of the `--no-aot` runner (geomean, best of 5) and 2.7x Rust's
(Go: 1.4-1.5x). Stack maps and inline allocation left the AOT geomean
flat on a same-host A/B against 8764f32 (most benches within ±5%; nsieve
-14%, match_enum -12%; binary_trees -5% instructions but +7% wall time,
grid_map +4% instructions from spills in its hot loop).

### Static `extern` libraries

`coil package --link-static [STEM=]PATH` (repeatable) links a static
archive for the `extern` block that names `STEM` (from the file name
when omitted: `libsum.a` is `sum`); `--link-arg ARG` passes linker
arguments after the libraries. Both need the AOT executable (an error
with `--no-aot` / `--runner`, or when linking fails). Preparing the
archive is the build tool's job (spool); the compiler takes paths only.

- The compiler lists each `extern` function as `(library, symbol)`
  (`Pipeline::extern_symbols`); `link_aot_executable` keeps those whose
  library stem is linked and hands them to `coil_native::aot`.
- The object imports each symbol and defines `coil_static_syms` (one
  `machine::StaticSymbol` per function: stem, symbol, address) and
  `coil_static_sym_count`; `cc` links `program.o`, the archives,
  `libcoil_rt.a`, then the link arguments.
- `coil-rt`'s `main` installs the table (`machine::install_static_symbols`).
  `resolve_library` answers a linked stem with the executable itself,
  before the dload gate (no file to open, pin or grant at run time;
  compiling the `extern` block took `--allow-dload`), and `FfiLoad` marks
  the library object (`ObjLibrary::static_symbols`), so `DeclareFFI`
  takes each function's address from the table. Calls go through libffi
  as for a shared library.
- The native lock and `--check-native` skip linked stems. `tests/package_cli.rs`
(`aot_package_matches_the_vm_runner`) checks output and exit status equal
the runner package.

## Where the pieces are

| Piece | Place |
|---|---|
| MIR capture for a backend | `compiler/src/mir/native_capture.rs`, called from `IlModule::optimize_and_flatten`; exported as `compiler::native_mir` |
| Lowering MIR → Cranelift, JIT driver | `coil-native/src/lib.rs` |
| Native table, `NativeCtx`, bail | `machine/src/native.rs`, `Machine::try_native_call` |
| `--jit` / `--no-jit` | `coil-args` (`NurseryFlag`), `coil compile --jit` in `src/cli.rs`, `src/main.rs` `install_native_bodies` |
| Archive section | `coil-native/src/section.rs`, `ArchivedProgram::native_mir` |
| AOT object | `coil_native::aot` (`coil-native/src/lib.rs`), `src/package_app.rs` `link_aot_executable` |
| AOT runtime | `coil-rt/` (staticlib with `main`), `machine::native::install_aot` |
| Module setup | `compiler/src/codegen/module_setup.rs` (`__module_setup`) |
| Native driver, machine-less start | `machine/src/driver.rs`, `machine/src/start.rs` (`run_natively`), `machine/src/worker.rs` |
| Stack maps, frame walk | `coil-native` (`stack_maps`, `define_trampolines`), `machine::native` (`register_stack_maps`, `install_aot_stack_maps`, `for_each_frame_root`) |
| Inline allocation | `machine/src/memory/inline.rs` (`inline_object`, `layout_fingerprint`), `HeapCounts` |
| Static `extern` libraries | `--link-static` in `src/cli.rs`, `src/package_app.rs` (`LinkInputs`), `coil-native` `static_symbol_table`, `machine/src/ffi/static_libs.rs` |
| Tests | `tests/native_jit_cli.rs` (JIT output equals VM output), `tests/package_cli.rs` (AOT package equals runner; `aot_package_links_extern_libraries_statically`) |
