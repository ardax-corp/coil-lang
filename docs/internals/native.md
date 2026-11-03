# Native code (`feature/native`)

Coil can run some functions as machine code made by
[Cranelift](https://cranelift.dev) from their MIR. The VM stays: it runs
comptime and macros, `coil debug`, coverage, wasm, and every body the native
backend does not compile. The plan (AOT `coil package`, heap and task
tiers) is in the project's native-compilation assessment; this page
describes what is implemented.

## Using it

```
coil prog.coil --jit          # compile, JIT what it can, run
COIL_NATIVE_STATS=1 coil prog.coil --jit   # counts on stderr
COIL_NATIVE_STATS=2 coil prog.coil --jit   # plus why each body stays on the VM
```

`--jit` skips the build cache (it needs this compile's MIR).

```
coil compile --jit prog.coil -o prog.ccoil   # archive with a native MIR section
coil run prog.ccoil                          # JITs the section, then runs
coil run --no-jit prog.ccoil                 # VM only
```

`coil run --jit` on an archive without the section warns and runs on the
VM. `coil package` compiles ahead of time instead (see [AOT](#aot-coil-package)).

### Testing against the VM

```
coil test --engine native [PATH]
```

runs a suite with native code: each file's MIR is captured and JIT
compiled, and its cases (and the machines they spawn) run it. A suite must
pass on both engines; CI's `harness native` cell runs every corpus this
way. `--coverage` needs `--engine vm` (it counts what the VM runs).

### The native MIR section

`ArchivedProgram::native_mir` (archive 6.20; 6.0 to 6.19 archives load
with it empty) holds bytes in the `coil-native` section format
(`coil-native/src/section.rs`): the magic `CNMIR`, a format byte, then each
body the backend compiles with its name, bytecode entry PC, entry label,
signature, value types and blocks. Only those bodies and only the MIR the
backend lowers are stored, so the encoder never meets an instruction
outside the tier; the format grows with the tiers and bumps its format byte
when the layout changes. A reader refuses any other format, a truncated
section or trailing bytes, and the run falls back to the VM with a
warning. The section is compiled again on every run: the archive carries
MIR, not machine code.

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
- Instead of the allocation cap it collects at safepoints (below).
- An early free happens at once (`NativeHost::free`); the undo log stops
  growing.

What still bails after a commit (a shape native code does not model, an
early free when the program has finalizers) panics with a message saying
so; the corpus never reaches it.

A host native whose first argument is a `Stream` keeps its unsent-stream
check: native code makes it and raises the VM's panic message
(`TRAP_MESSAGE`).

### Native panics

Every bail path goes through `coil_native_unwind(ctx, code, site)`. Before
a commit it sets the bail flag. After, the first frame records the trap
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

A call to a body that does not compile is no longer a reason to refuse the
caller: native code runs it through `Machine::call_function` (a nested
execution; a two-word `RETURN` leaves its second word in
`nested_return_hi`). If that run panics, the VM has reported it; native
code unwinds with `TRAP_VM_PANIC` and the VM goes on unwinding from the
`CALL` (`rearm_unwind_after_early_drop`). The backtrace then lacks the
native frames in between.

A collection can run inside such a call, and at a *safepoint*: the entry
and loop headers of a body that may allocate, taken when an allocation
after the commit found a collection due (`NativeCtx::gc_due`). Before
either, native code links a record of every heap-typed value of the body
into `NativeCtx::roots` (`[next, n, words…]` on its stack); callers of a
body where a collection may run (`Plan::may_gc`) do the same around the
call. The VM treats those words as precise roots: a minor collection
forwards them in place. After the call, when `NativeCtx::gc_epoch` moved,
native code reloads the words and drops its array views.

### Host calls through the VM

A host native without a plain function (`write`, `await_writable`,
`thread_*`, `vec_insert`, ...: `DirectHost::via_vm`, the ordinary host
ops) runs as a VM call. A machine with native code appends *host-call
stubs* past the program (`native::host_stubs`, `Machine::ensure_host_stubs`):
for each enum layout and arity, `HostInvoke; RETURN`. `coil_native_host`
calls the stub through `NativeHost::host` with `[id, args…]` as its frame,
so parking (a fiber under a scheduler, `pending_io` without one), retries
and panics are the VM's own. A frame in a stub prints no backtrace line,
and a panic in it is located at the native call site
(`Machine::in_host_stub`). The `task_*` host ops (scope, spawn, join, ...)
go the same way: a scope native code opens through a stub lives at that
nesting level, and its switches happen in the stub executions, as in a VM
body native code called. FFI ops (`FfiLoad`, `DeclareFFI`, `FfiInvoke`) get a stub per opcode and
operand the program uses (found by scanning its code), which
`coil_native_ffi` calls the same way (`NativeHost::ffi`). A failed
`requires` (`contract_fail`) in a stub
names the native function's caller (`host_stub_caller_location`).

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
plan's `vm_reach` set) runs on a **fiber** wherever tasks can switch
(`vm_fiber.rs`; `corosensei` coroutines, 8 MiB reserved stacks, reused).
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
VM when a scheduler exists. Refused: `yield from` in the body, a body with
`defer` (a close would have to unwind it), and two-word returns. A
generator collected while suspended drops its fiber unwound
(`GenFiber::drop`).

A fiber's VM state starts with a base frame (ip 0, as the program's
bootstrap frame), so VM calls on it open their frames as anywhere else. A
failed `requires` in a VM body native code called names native code's call
site (`native_caller_location`).

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

## AOT `coil package`

`coil package` links a native executable by default (`--no-aot` or
`--runner` keep the old runner + payload):

1. The build captures MIR as for `--jit`; `coil_native::aot()` lowers it
   with `cranelift-object` (PIC) into `program.o`. The object defines
   `coil_aot_entries` / `coil_aot_entry_count` (one `AotEntry` per
   compiled body, the function address relocated in) and
   `coil_aot_sites` / `coil_aot_site_count` (call-site words), and imports
   the runtime helpers by name (`machine::native::helper_symbols()`, all
   `#[unsafe(no_mangle)]`).
2. `cc` links `program.o` with `libcoil_rt.a` (the `coil-rt` staticlib,
   found via `COIL_RT_LIB` or beside the `coil` binary). `coil-rt`'s
   `main` calls `machine::native::install_aot` with those symbols, then
   runs the embedded payload like `coil-embed`.
3. The archive payload is appended as for a runner package; the VM runs
   every body the object does not hold.

If the object or the link fails (no `cc`, no `libcoil_rt.a`, Windows) the
package warns and falls back to the runner. `tests/package_cli.rs`
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
| Tests | `tests/native_jit_cli.rs` (JIT output equals VM output), `tests/package_cli.rs` (AOT package equals runner) |
