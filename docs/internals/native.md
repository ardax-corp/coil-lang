# Native code

Coil runs programs as machine code that [Cranelift](https://cranelift.dev) makes
from their MIR: JIT by default, AOT with `coil package`. Compile-time code, macros,
`coil debug`, wasm and `--mir` (`--engine mir` under `coil test`) run the same MIR on the interpreter
([mir-interp.md](mir-interp.md)), which sits on the same ABI.

## Using it

```
coil prog.coil                       # compile, JIT, run
coil prog.coil --mir                 # the same MIR on the interpreter
COIL_NATIVE_STATS=1 coil prog.coil   # counts on stderr
COIL_NATIVE_STATS=2 coil prog.coil   # plus each body compiled or refused

coil compile prog.coil -o prog.ccoil # archive with the bodies' MIR
coil run prog.ccoil                  # JITs the section, then runs
```

`coil package` compiles ahead of time instead (see [AOT](#aot-coil-package)).
`--cow-report` runs on the interpreter.

`coil FILE`, `coil compile`, `coil run`, `coil test` and packages all run native
code. When a body gets no native code (a refusal, or the JIT itself fails), a
release `coil FILE` / `coil run` warns and runs the program on the interpreter; a
debug build stops with the reason (exit 1) and points at `--mir`, so a backend gap
fails a test instead of passing slowly. `coil test` and packages never fall back.
`COIL_NATIVE_REFUSE=a,b` makes the JIT refuse the named bodies, for testing that
path. A wasm build has none and interprets. Native code finds a panic's location
by span, so an archive or package that runs it keeps byte spans at `-g1`
(`Pipeline::set_runs_mir`), and so does a direct run.

A run takes its archive from the build cache as any other does, with the MIR
section in it. `--mir` skips the cache, since the section holds only what native
code compiles.

## How long code lives

`NativeProgram::keep` owns a program's code and stack maps, and they go with its
last handle. `coil test` gives it to `NativeCode`, so each file's code leaves with
the file: a JIT reserves 64 MiB or more, and Windows charges all of it to the commit
limit. A run of one program forgets `keep`: the code stays for the process.

## Program start

A compiled program is its bodies, a list of `common::StartupOp` and `main`. A body's
identity is `label.0 + 1`; `0` names nothing. `StartupOp::Call` calls a body,
`Init` calls one and stores its word in a static, and `Restore` rebuilds a heap
snapshot from the constant pool. `Driver::run_startup` walks the list, and
`Driver::run_program` then calls `main` (an AOT executable has no list: its `main` is
its [native start](#native-start)). `Driver::covers` says whether the installed
code has every body the startup and `main` name.

Module setup is a function, `__module_setup`
(`compiler/src/codegen/module_setup.rs`): the stores of the statics computed at run
time and each `extern` library's load and declarations. The finalizer registry
(one `gc_register_finalizer` per class with a `drop`) is `__finalizer_setup`, called
first of all, since a collection during any initializer must find the `drop` of what
it made. Codegen records the steps (`SetupStep`) and builds each function's MIR from
them, so they compile like any body.

Initializers computed while compiling (`const`s and pure `static let`s) are
`StartupOp::Init` steps of their own. `Pipeline::compute_consts` replaces them with
one `Restore`; one that cannot be computed keeps its step.

## The MIR section

`ArchivedProgram::native_mir` holds bytes in the MIR archive format
(`compiler/src/mir/archive.rs`): the magic `CNMIR`, a format byte, then each body
with its name, entry and entry label. A full body is stored whole, every field and
instruction, and comes back equal; `COIL_MIR_ARCHIVE_CHECK=1` makes
`coil_mir_interp::prepare` send every body through the format and run the copy. A
reader refuses any other format byte, a truncated section or trailing bytes. The
section is compiled again on every run: the archive carries MIR, not machine code.

## Testing

```
coil test [PATH]                   # the same as --engine native
coil test --engine mir [PATH]
```

Each file's MIR is captured and compiled, and its cases and the jobs they spawn run
it. A suite must pass on both engines. CI's `harness-*` cells run every corpus on
both, at more than one level, and `mir-parked` runs the interpreter under
`COIL_NO_FIBERS=1`.

With `COIL_NATIVE_INVENTORY=FILE` a run appends per compiled file the bodies
compiled and refused, with each reason. `scripts/native_inventory.py FILE [--exe
PACKAGED] [--max KEY=N]` sums them, groups the refusals by reason and fails over a
limit. The `harness native` cell writes the report to its job summary.

## What a body holds

A body holds `int`, `float` and `bool` words (`MirTy::I64` / `F64` / `Bool`) and heap
words (`HeapRef`, `NicheOpt`, `NicheRes`). Native-to-native calls are direct
Cranelift calls with the values in registers. A tail call (`is_tail_call_inst`) takes
the frame with `return_call`, in a body with no `defer`s or `gc::root` handles
whose callee returns the same words. Those bodies and their callees are on the `tail`
calling convention; the others stay on the platform's, which keeps callee-saved
registers. An entry has the shape `extern "C"
fn(ctx, *const u64) -> u64`: the wrapper unpacks words (a `bool` word is true when
its low byte is `1`; a `float` word is its bits), calls the body and packs the
result. A body that returns two words returns two Cranelift values, and the entry
leaves the second in `NativeCtx::ret_hi` (`NativeEntry::ret_words`).

`machine::native` holds the table of entries. `NativeCtx` is the state native code
and the runtime share, read at fixed offsets.

Heap reads and writes (`Index`, `ArrayLen`, `StoreIndex`, `HeapFieldStore`) call
helpers in `machine::native` (`coil_native_index`, `_len`, `_store_index`,
`_set_field`) when they cannot work in place: see the views below and "Inline field
access". `ProductLoad` and a boxed `MatchPayload` never call one.

An array a body indexes gets a *view* where its value is defined (a parameter, a φ,
a load): `coil_native_view` hands back the pointer and length of its element words,
and `Index` / `StoreIndex` / `ArrayLen` in bounds of the view read and write in
place. Byte arrays, tuples and an index out of the view take the helpers. Storing
into a word array never moves its storage; a push may, and another value may name
the same array, so after an `ArrayPush` every view reads through the helpers until
its value is defined again. A vector loop drops the views too.

`Cow`, `Alloc` and `ArrayPush` call `coil_native_cow`, `_cow_field`, `_alloc` and
`_array_push`. More ops follow the same pattern:

| MIR | Helper | Note |
|---|---|---|
| Boxed enum match, `MatchPayload` | `coil_native_enum_tag`, `_enum_payload` | a niche word (`Option`: `0` is `None`; `Result`: low bit is `Err`) is tested inline and is its own payload |
| `String` | `coil_native_string` | the program's string table and cache travel in `NativeCtx` |
| `StrEq` | `coil_native_eq` | `value_eq::values_eq` |
| `Stringify` | `coil_native_stringify` | |
| `Format` | `coil_native_format` | |
| `FreeEarly` | `coil_native_free_early` | a `drop` it runs may collect, so native code roots its words around it then |
| `Print` | `coil_native_print` | |
| `HostInvoke` | `coil_native_host` | below |
| `CallIndirect` | `coil_native_call_value` | below |
| `Probe` | `coil_native_probe` | coverage |

## Panics

A call never goes back and retries: it runs to its end or panics. Every exit that is
not a return goes through `coil_native_unwind(ctx, code, site)`. The first frame
records the trap (`TRAP_OVERFLOW`, `TRAP_DIV_ZERO`, `TRAP_NEGATIVE_EXPONENT`,
`TRAP_SHIFT`, `TRAP_OUT_OF_BOUNDS`, `TRAP_STACK`, `TRAP_MESSAGE`, `TRAP_EXPLICIT`,
...), and every frame on the way out appends its *site*: an index into
`NativeCode::sites`, the function's entry and the MIR op's source location
(`MirFunc::op_loc`). `Driver::native_panic` turns those into the message, its ` at
file:line:col` and the backtrace lines (`in f at …`, repeats folded), with locations
from `ProgramDebug::line_of`. A panic in a body under a host call unwinds as
`TRAP_PANICKED`: the panic is already reported, so the frames above only leave.

A host native whose first argument is a `Stream` keeps its unsent-stream check:
native code makes it and raises the message with `TRAP_MESSAGE`.

## Stack depth

Native recursion runs on the machine stack. A native call made on a thread's own
stack switches to that thread's deep stack (`stacks::on_deep_stack`,
`FIBER_STACK_BYTES`: 256 MiB reserved on 64-bit, 32 MiB otherwise). A task fiber has
the same size and a generator fiber 8 MiB. `NativeCtx::stack_limit` sits
`NATIVE_STACK_GUARD` (128 KiB) above the stack's end; past it native code panics with
`TRAP_STACK` and the stack overflow message (`stacks::STACK_OVERFLOW`). Every function
entry makes the compare, which is how a speculative auto-par job is cancelled at no
cost: the canceller stores `thread::CANCELLED_STACK_LIMIT` into the limit of each
native call the job is in, and its next call traps
([auto-par](auto-par.md#cancelling-a-failed-attempt)). In a body a speculative job
may run (`speculative_reach`), the header of a loop that may run forever with no call
in it (`mir::spin_headers`: no counter ends it) makes the compare too, with an atomic
load of the limit. The
interpreter's frames do not use the machine stack, and its limit is
`frame::MAX_WORDS` words of registers per activation.

## Calls and collections

A collection runs at a *safepoint*: the entry and loop headers of a body that may
allocate, taken when an allocation found one due (`NativeCtx::gc_due`), and inside
helpers that may collect. `gc_epoch` moves at each, so native code reloads its
rooted words.

Native frames hand the collector their heap words through Cranelift stack maps. Every
value of a heap type (and every `int` value a heap word moved into, `pointer_words`)
is declared `declare_var_needs_stack_map`: Cranelift keeps it in a stack slot across
each call and records, per call's return address, which slots hold live heap words.
Liveness is Cranelift's: **a word lives until its last use**. A `gc::root` handle is
the exception: `keep_roots` uses each one before every return, so an unused `let r =
root(h)` keeps `h` until the function returns. A weak reference or a finalizer can
therefore see an object die at its last use, not at the end of its scope. The
interpreter follows the same rule ([mir-interp.md](mir-interp.md#how-it-runs)).

Code is compiled with frame pointers. Native code calls a helper that may collect
through a local trampoline that stores its frame pointer in `NativeCtx::native_fp`.
The collector walks from there: `[fp + 8]` is the return address into the caller,
`fp + 16` the caller's stack pointer at that call, `[fp]` the caller's frame pointer.
Each return address inside a compiled body looks up its stack map
(`machine::native::register_stack_maps`; an AOT object carries them as
`coil_aot_stack_maps`); the walk stops at the first return address outside every
body. The slots are precise roots: a minor collection forwards them in place, and
native code reloads them after the call. A suspended task or generator keeps its
context's `native_fp`, so its frames are walked the same way.

Two kinds of word stay in a record linked into `NativeCtx::roots` (`[next, n,
words…]` on the stack): the heap homes, which live in memory and are written back
when `gc_epoch` moved (array views are then re-read), and the ambiguous words
(`ROOTS_AMBIGUOUS`): the low word of a pair that may carry an object, a call's result
or a parameter (`MirFunc::heap_pair_words`, and the φs that copy one: `pair_words`),
and an opaque body's `int` words. Those are pinned and never rewritten.

`compiler/src/mir/frame_words.rs` holds these analyses for both engines.

## Host calls

Host natives keep frozen ids: the `*_ID` consts in `common/src/host.rs`, with a table
in `machine/src/host_natives.rs`. Retired ids stay as panic stubs, so later ids do not
move. A `HostInvoke` carries a layout for `Option` / `Result` results (`0` boxed
`ObjEnum`, `1` `Option` pointer niche or `Result<(), E>` with heap `E`, `2` heap-heap
`Result`), and natives build that layout once through `machine::host_enum`.

A native with a plain function (`machine::native::direct_host_natives`, resolved at
compile time since ids are frozen) is called straight from `coil_native_host`. One
without (`write`, `await_writable`, `thread_*`, ...) runs through `NativeHost::invoke`
on the driver, with its words rooted, and maps the result to a value, a panic
(`TRAP_MESSAGE`) or an unwind already reported (`TRAP_PANICKED`). `gc_epoch` moves so
native code reloads its words.

- Waits (thread park, io retry, pending io) suspend the task the call runs on. With
  no scheduler the thread blocks. A clock sleep inside a scope adds a timer and
  parks.
- `task_*` ops go to `Driver::task_op`. A failed `requires` (`contract_fail`) names
  the caller.
- FFI ops (`Load`, `Declare`, `Invoke`) run through `coil_native_ffi`.

`CallIndirect` runs through `coil_native_call_value` and `Driver::call_value`, which
handles exact calls, partial and over-application and dictionaries. A function value
a host native calls (a closure passed to it, a finalizer, a callback) runs the same
way. `Alloc` of a function value (`Fn`, `PolyFn`, `PolyFnCapture`) builds the `ObjFn`
in `coil_native_alloc`; the body it names counts as a callee. `ObjFn` keeps a function
value's arity in 8 bits, so calling a function value of 256 or more parameters panics.

### Vec natives written out as MIR

`vec_insert` and `vec_remove` return an `Option` or move elements of any type. In a
captured body `compiler/src/mir/native_expand.rs` writes each `HostInvoke` of them out
as the loop it stands for, over `ArrayLen` / `Index` / `StoreIndex` / `ArrayPush`.
Element words are copied as they are (as an `i64` when the call does not say their
type), so one expansion serves every element type. `vec_from_array` stays a call: it
is the copy primitive, and keeps a packed byte or float source packed, which the MIR
array ops cannot ask for. Two more things stay calls: `remove` rotates the element to
the end and calls `vec_pop`, the shrink primitive, which also packs the `Option` in
the call's layout; and `insert` out of range calls `vec_insert` itself, so the panic
is the host native's (the message alone, no Coil frames). A call whose operand or
result types the expansion does not cover stays a `HostInvoke`.

## Drops

A direct call of a body that may be a `drop` (`Plan::drops`) claims the finalizer
first (`coil_native_claim`) and skips a `drop` that ran.

## `defer`

A body with `defer`s compiles with its cleanup pad (`MirFunc::cleanup`, a separate
function `(ctx, *const u64 homes) -> u64`). Its homes live in a zeroed stack slot;
heap homes are rooted with the body's heap words. The normal exits run the defers
inline, as in MIR. After a panic, each unwound frame's exit path hands its pad and a
copy of its homes to `coil_native_defer`, and `Driver::run_pads` runs them once the
panic is reported, innermost first: panic and backtrace, then the defers inside out.

## Tasks

Child tasks run on fibers of their own (`TaskFiber`, `corosensei` stacks, reused). The
root task, the program's own call, never leaves its stack: where it waits the driver
runs the scheduler there (`Driver::run_tasks`) until the root is ready again. A child
that waits suspends its fiber back to that loop.

- A switch fences its owner (`stacks::fiber_switch_fence`): what runs on the other
  side reaches the same object through a raw pointer, which `&mut`'s `noalias` does
  not account for.
- The collector walks a suspended fiber's frames from the `native_fp` it kept.
- A task that never resumes (its scope torn down after a root panic) drops its fiber
  with `force_reset`: native frames have no unwind tables, so what they own leaks.
- wasm has no fibers: the interpreter parks its frames on the heap there
  ([mir-interp.md](mir-interp.md)). Native code cannot park.

[tasks.md](tasks.md) has the scheduler and the semantics.

## Generators

Native code makes generators (`Alloc` of `MirAllocKind::Coro`: `ALLOC_CORO`), checks
them (`coil_native_coro_done`) and resumes them (`coil_native_resume`). A generator's
body compiles too (`NativeEntry::generator`, `AotEntry` byte `generator`); no call
enters it. Its first resume makes a fiber for it (`machine/src/gen_fiber.rs`) and runs
the body there on the generator's saved arguments. `Yield` calls `coil_native_yield`,
which suspends the fiber with the value and its call's context. The resumer then
copies that call's root words into the generator's `saved_stack`, so the collector
traces and forwards them, and the `ObjCoroutine` owns the fiber. The next resume
writes the words back, sends its word, and native code reloads after the yield
(`gc_epoch`). A resume of a finished or invalid handle panics at native code's own
site. `yield from h` in a native body is a loop of resume, done check and yield, the
sends passed through. Two-word returns are refused.

A generator body with `defer`s compiles (`NativeEntry::cleanup`, `AotEntry` byte
`cleanup`). A close resumes the fiber with `GEN_CLOSING` set; `coil_native_yield` sees
it and unwinds with `TRAP_CLOSE`, the frames hand their pads over as for a panic, and
the pads run once the coroutine is finished, inside out. A generator collected while
suspended drops its fiber unwound.

## Vector loops

A `ByteScan` or `VecLoop` ([mir.md](mir.md#loop-rewrites)) that native code cannot
reduce to lane code runs the runtime's lane kernels (`machine/src/lanes.rs`) through
`coil_native_vector`. `compiler::native_mir::vector_op` encodes the op's body over a
frame of word slots (slot 0 the loop's `dest`, which a reduce's or push's start value
fills first, slot 1 its block position, then each operand); native code stores the
words in a stack slot and the helper runs the body and copies the slots back. A trap
panics at native code's own site. `COIL_NATIVE_STATS` counts these ops.

`optimize_mir` gives native code the same bulk rewrites (`copy_until`, `bulk_push`) and
runtime kernels (`vec_extend_range` / `vec_fill`).

## The driver

`machine::driver::Driver` runs every program, over a `Runtime`. It walks the startup,
calls the entry and answers native code's host requests itself: calls of bodies and
function values, host natives, collections over the native frames' records, early
frees, finalizers, tasks, generators, threads and auto-par workers, and FFI callbacks.

`machine::run_natively` (`machine/src/start.rs`) builds a `Runtime` for the program and
runs the driver over it, and `coil_cli::execute_archived_program` calls it. When the
code does not cover the program it returns `None`, and the host stops with an error
("this archive's code does not cover the program"): no second engine takes over. The
AOT link passes `--gc-sections` (`-dead_strip` on macOS), so code nothing references
is left out of the executable. A reactor job (a thread, an auto-par body, a harness
case) runs the same way: `machine/src/worker.rs` (`run_native_job`) sets up a `Runtime`
of the worker's own (pooled per thread, reset between jobs) and calls the entry on the
driver.

## Inline allocation

An `Alloc` of a fixed-size nursery object (an enum variant or tuple, an enum object of
`PRODUCT_TAG`, of 1 to 4 words, an
instance of a class with 1 to 4 fields) does not call `coil_native_alloc` on its fast
path. Native code loads the heap's gate (`HeapCounts::inline_gate`, closed while the
nursery is off, a mark is open, pretenuring after a mostly surviving minor, a steal
epoch, and under `gc-stress`), and for an instance tests its type's bit in the
finalizer types (a type with a `drop` stays old). It then bumps its size class's cursor
(`Bump`, boxed in the slab, indexed by a class fixed for every heap), writes the slot's
image with the payload words in place, and adds the object to `alloc_bytes`,
`young_bytes` and `live_count`. A full chunk, a closed gate or a `drop` type falls back
to the helper, which refills the chunk and sets `gc_due` when a minor collection is
due, so collections still come at safepoints on time.

The images come from `machine::memory::inline_object`, probed from real objects:
`ObjEnum`, `ObjInstance` and their payloads are `repr(C)` / `repr(u8)`, so
the compiler that writes the image and the runtime that reads the object agree. An AOT
object records `layout_fingerprint` as `coil_aot_layout`; `install_aot` refuses an
object made against other layouts. `COIL_NATIVE_INLINE_ALLOC=0` at compile time turns
the fast path off, for A/B runs. Arrays, closures, generators and `drop` enums always
take the helper.

## Inline field access

`ProductLoad`, `MatchPayload` of a boxed enum (a `JumpIfMatch`'s payloads too) and
`HeapFieldStore` work in place. An instance's fields and an enum's payload (a
tuple's items) are one store at one offset (`EnumPayload`, at 8), so one read
serves every product. The offsets come from `machine::memory::product_slots`,
probed like the allocation images: where the header keeps the kind and generation
bytes, where the store sits, its `Inline` and `Spill` tags, the inline length byte
and slots, and the `Vec` pointer and length words of a spilled store. Native code
checks, in order, that the object is not null, that its kind is an instance or an
enum (one test: the two kinds differ in one bit; a store takes instances only), for
a store that the generation byte has no unlogged bit (the write barrier would have
to log the object), that the store is inline or spilled, and that the slot is under
the length. It then loads or stores the slot word. A failed load check is a
compiler bug and bails, a panic, with no helper call; the interpreter's
`coil_native_product_load` bails on the same objects. A failed store check calls
`_set_field`, which handles logged objects and panics as before.
`layout_fingerprint` covers these offsets, so an AOT object made against another
layout is refused.

## Inline share

A copy-on-write share of a word (`Cow` with `unique` false and no field) sets
the object's shared flag in place, with no call (#2506): the word with its low
tag bit cleared, nothing for null, then the header's kind byte, and for a class
instance or an array (`KIND_INSTANCE`, `KIND_ARRAY`) a store of
`COW_SHARED_SET` at `COW_SHARED_OFFSET`. Those constants sit next to
`GcHeader` (`machine/src/memory/heap.rs`), tied to it by `offset_of!` and
size asserts, and `Object::set_shared` writes the flag through the same pair;
`layout_fingerprint` covers them. Any other word is left as it is, as
`Heap::cow_share` leaves it. A share allocates nothing (`OpProps` of a share
`Cow`: no `allocates`, no `gc_edge`), so a body whose only copy-on-write op on
a type parameter is a share behind its `kinds` test stays without a GC poll
and with no helper call. The flag is a `Cell<bool>` the runtime sets from a
shared reference, never through `GcData::as_mut`, so the inline store is the
same in debug and `gc-stress` builds. The interpreter keeps `coil_native_cow`,
which also counts the share for `--cow-report`'s summary line; native code's
inline shares are not in that count (the copies are).

In-place stores are compiled only into release builds without `gc-stress`
(`ProductSlots::inline_stores`): debug and `gc-stress` builds check epoch writes in
`GcData::as_mut`, which native code would skip. Loads are in place in every build.

## Operation semantics

| Operation | Native |
|---|---|
| int `+ - *` | `sadd/ssub/smul_overflow`, panic on overflow; a plain `iadd` / `isub` for a step `in_range_steps` keeps in range |
| int `/` | panic on `b == 0` and `MIN / -1`; `sdiv` |
| int `%` | panic on `b == 0`; `x % -1` is `0`; `srem` |
| `<< >>` | panic unless `0 <= b < 64`; `ishl` / `sshr` |
| float `% **` | Rust `%` / `powf` through a helper |
| float `!=` | true when unordered (Rust `!=`), `==` / `<` ordered |
| `float as int` | `fcvt_to_sint_sat` (Rust `as`: saturating, NaN is 0) |

`mir::in_range_steps` (`compiler/src/mir/int_range.rs`) lists the int steps
that cannot overflow: `x + c` or `x - c`, with `c` a constant, where a branch
edge bounds `x` on the side the step moves toward. `x < y` bounds `x` by
`y - 1`, or by `int` max - 1 when `y` is not a constant. `x <= y` bounds it
by `y`, or by `int` max. A step by `c` is safe when the bound plus `c` is
still an `int`, so `i + 1` under `while i < n` never needs the check. The
fact holds in the blocks the edge's target dominates, where the target has
that edge as its only way in. Native code adds these steps with no
`sadd_overflow` and no bail branch (#2127). The MIR interpreter keeps its
checked add.

## AOT `coil package`

`coil package` links a native executable by default (`--no-aot` or `--runner` package
the [JIT runner](#jit-runner) instead):

1. The build captures MIR; `coil_native::aot()` lowers it with `cranelift-object` (PIC)
   into `program.o`. The object defines `coil_aot_entries` / `coil_aot_entry_count`
   (one `AotEntry` per compiled body, the function address relocated in),
   `coil_aot_sites` / `coil_aot_site_count` (call-site words) and
   `coil_aot_refused_count` (bodies left out), `coil_aot_package` /
   `coil_aot_package_len` (the program's data, below), and imports the runtime helpers
   by name (`machine::native::helper_symbols()`, all `#[unsafe(no_mangle)]`).
2. `cc` links `program.o` with `libcoil_rt.a` (the `coil-rt` staticlib): `COIL_RT_LIB`,
   else the copy embedded at the end of `coil` (`scripts/embed_runtime.py`, which the
   release workflow runs: the library, its length, `COILRT\0\1`), written once to
   `<cache>/runtime/<sha256 prefix>/` (`package_app::embedded_runtime`), else the file
   beside `coil` (a cargo build). `--strip-symbols` (`debug` by default, `none`, `all`)
   sets what the linker strips: the runtime's DWARF, then its symbol table. Coil's
   backtraces read the program's line table (`-g`), so stripping keeps them. `coil-rt`'s
   `main` calls `machine::native::install_aot` with those symbols, then hands
   `coil_aot_package` to `coil_cli::run_linked_package`, and the driver runs it
   (`machine::run_natively`). The executable does not read its own file.

`coil_aot_package` is `common::linked_package`: the archive, the native lock and a
trailer with `PACKAGE_FLAG_LINKED`, whose `archive_offset` counts back from the trailer.
The symbol is 16-byte aligned and the archive comes first, so rkyv reads it in place.
`common::find_linked_package` finds it from outside the program, for `coil natives dump`.

### Native start

An AOT executable starts in its own code. `coil package` adds a body, `__start`
(`coil_native::start_body`, `common::NATIVE_START_NAME`), that runs the program's
startup in order (each `Init` a call and a `StoreStatic`, each `Call` a call, each
`Restore` a call of host native `startup_restore`, `HostOp::Restore`, which the driver
runs) and then calls `main`. Its id is past every body's. The payload is then data only:
its `startup` is empty and its `main` is `__start`, so the driver calls one body and walks
nothing. A panic's backtrace leaves `__start`'s frame out (`Locator::native_site_line`).
The JIT runner and `coil FILE` keep the startup list.

`--emit` stops before the link. `--emit obj` writes the object: `cc app.o libcoil_rt.a`
makes the program, with your own objects beside it. `--emit lib` adds the object to a copy
of the runtime library with `ar` (`AR`), one input for the link. Either way `main` is the
runtime's, so the result is a Coil program; calling Coil from a C `main` needs exported
functions and a runtime lifecycle, which do not exist yet. `extern` blocks named with
`--link-static` stay undefined symbols for that link. `coil package` prints the link line.

A body native code refuses is an error naming it and why, at package time on both kinds
of executable. No source program reaches one today: parameters, captures, generator
arguments and live words have no limit in native code, and the corpus reports no
refusal. If the object or the link fails (no `cc`, no `libcoil_rt.a`) the package warns
and falls back to the runner. On Windows `cc` is a mingw toolchain's, so without one a
package is the runner. `.github/workflows/native-platforms.yml` runs every way a program
starts on macOS and Windows, and a fallback fails it.

## Compiling in parallel

`define_all` lowers bodies in order (it declares what they call in the module, and site
numbers come from that order) and hands each function to `compile_while`, whose threads
compile it as soon as it is lowered. Functions do not depend on each other, and results
go back in submission order, so the code is the same on any number of threads:
`COIL_NATIVE_THREADS` sets the most, and `the_code_is_the_same_on_any_number_of_threads`
holds it to that. A program of under 32 functions stays on one thread, and more than
8 threads only contend, as lowering is what compiling then waits for. `coil package`
compiles the same way.

## Code cache

Compiling is nearly all of a program's start: for `spool` (574 bodies), 335 ms in
Cranelift and 110 ms lowering, against 5 ms decoding the MIR and 3 ms planning. So `coil
FILE` and `coil run` keep the code in `~/.cache/coil/code` (`src/code_cache.rs`) and a
repeat run loads it: 482 ms becomes 9 ms there. `--no-build-cache` skips it. Neither
`coil test` nor a JIT runner uses it.

- **Key:** sha256 of the MIR section, the build id, the host's target and CPU features
  (`coil_native::image_fingerprint`) and every `COIL_*` variable. A hit skips decoding.
- **Entry:** a code image (`coil-native/src/image.rs`): each function's bytes and
  relocations, the stack maps, entries and sites. `load_image` declares the same
  functions in the same order, so `FuncId`s match, and hands the bytes to
  `define_function_bytes`, which relocates them for this process. Code holds no
  addresses of its own: the AOT object relocates the same lowering.
- **Damage:** the image carries a sha256 of itself. One that does not load warns, goes,
  and the run compiles. Machine code is not checked otherwise, so the directory is
  `0700` and a hit trusts its owner, as running their own binaries does.
- **Size:** at most 256 entries, least recently used first, as the build cache.

`COIL_CODE_IMAGE_CHECK=1` sends every JIT through an image and runs the copy, and fails
if the code does not fit one. CI's `native` cell runs the corpus so, and GC stress runs
`memory` so: any program a cache hit would break fails there.

### JIT runner

`coil-embed` is the runtime and Cranelift, with no parser or linker. `coil package
--no-aot` appends the program with its MIR section after the runner's image, and the
runner JITs it at start (`coil_native::install_section`, through
`coil_cli::try_run_embedded_with`) and runs it
on the driver. Nothing about it needs `cc`, so it is what a host that cannot link gets.
`coil package` compiles the MIR once itself, so a body that does not compile fails
there and not on the target. An AOT package has no section: its code is in the
executable.

A program pays the JIT at every start (a few ms for a small one).

### Size and speed

Measured on feature/native 544b4ba (2026-11-05, x86_64 Linux, release `coil` and
`coil-rt`). A packaged hello world was 1.58 MB with `--strip-symbols all` (1.85 MB by
default). What remained was the runtime the host-native table reached (heap, driver, host
natives, FFI, threads, reactor, archive reader): `--gc-sections` cannot drop a native
the table names. The executable now links a per-program table (below): 1.09 MB stripped
on feature/native 7e00bc9 (2026-11-08), from 1.53 MB, and its start runs 0.83M
instructions instead of 2.33M (callgrind), as it builds ~25 natives instead of 213 (twice:
the runtime's table and native code's).

### Per-program host natives

`machine::host_natives` builds the host table from groups (`STANDARD_HOST_GROUPS`, in id
order): a group is a `HostPush` that pushes consecutive ids, one per `push_*` of the
table, and one per `io` native (`push_io_kind::<K>`, a constant kind, so each keeps
its own closure). `host_groups!` also exports each group as a symbol,
`coil_host_group_K`, that holds its `HostPush`.

- `coil_native::aot` lists the groups the program's `HostInvoke`s name, with the
  groups every runtime calls (`core_host_groups`: the task scheduler, `unwind_resume`,
  `contract_fail`, `coro_close`, the collector's and `region`'s hooks), as
  `coil_aot_host_groups` (`machine::AotHostGroup`: the symbol's address, first id,
  count) and `coil_aot_host_group_count`. The link keeps those groups and drops the
  rest.
- `coil-rt`'s `main` installs them (`machine::native::install_aot_host_groups`) and
  `run_linked_package` starts on `aot_host_natives`: their natives at their ids and an
  absent native (`NativeFn::is_absent`, no name) at every other id.
- The start names no default table: `NativeStart::host_natives` is the table, and each
  host passes `machine::standard_host_natives()` (built once per process) or the AOT
  one. A default would name every group, and the link would keep them all.
- Native code calls a host native through the runtime's table (`Natives::host_call`,
  a `HostCall` per id), not a process-wide one, so an absent id bails as an unknown
  one did. `direct_host_natives` (the standard table, by name) is what `coil-native`
  plans calls with.

Across the 18 cross-language benches, an AOT executable took 0.37x the wall time of the
`--no-aot` runner as it was then, the bytecode VM's since deleted (geomean, best of 5),
and 2.7x Rust's (Go: 1.4-1.5x).

### Static `extern` libraries

`coil package --link-static [STEM=]PATH` (repeatable) links a static archive for the
`extern` block that names `STEM` (from the file name when omitted: `libsum.a` is
`sum`); `--link-arg ARG` passes linker arguments after the libraries. Both need the AOT
executable (an error with `--no-aot` / `--runner`, or when linking fails). Preparing the
archive is the build tool's job (spool); the compiler takes paths only.

- The compiler lists each `extern` function as `(library, symbol)`
  (`Pipeline::extern_symbols`); `link_aot_executable` keeps those whose library stem is
  linked and hands them to `coil_native::aot`.
- The object imports each symbol and defines `coil_static_syms` (one
  `machine::StaticSymbol` per function: stem, symbol, address) and
  `coil_static_sym_count`; `cc` links `program.o`, the archives, `libcoil_rt.a`, then
  the link arguments.
- `coil-rt`'s `main` installs the table (`machine::install_static_symbols`).
  `resolve_library` answers a linked stem with the executable itself, before the dload
  gate (no file to open, pin or grant at run time; compiling the `extern` block took
  `--allow-dload`), and an FFI load marks the library object
  (`ObjLibrary::static_symbols`), so a declare takes each function's address from the
  table. Calls go through libffi as for a shared library.
- The native lock and `--check-native` skip linked stems.

## Where the pieces are

| Piece | Place |
|---|---|
| MIR capture for a backend | `compiler/src/mir/native_capture.rs`; exported as `compiler::native_mir` |
| Lowering MIR to Cranelift, JIT | `coil-native/src/lib.rs` |
| Native table, `NativeCtx`, helpers | `machine/src/native.rs` |
| Engine choice | `--mir` in `coil-args` (`NurseryFlag`), `--engine` in `coil-test`, `src/main.rs` `install_bodies` |
| Archive section | `coil-native/src/section.rs`, `compiler/src/mir/archive.rs`, `ArchivedProgram::native_mir` |
| AOT object | `coil_native::aot` (`coil-native/src/lib.rs`), `src/package_app.rs` `link_aot_executable` |
| AOT runtime | `coil-rt/` (staticlib with `main`), `machine::native::install_aot` |
| Module setup | `compiler/src/codegen/module_setup.rs` |
| Driver, start | `machine/src/driver.rs`, `machine/src/start.rs` (`run_natively`), `machine/src/worker.rs` |
| Machine stacks | `machine/src/stacks.rs` |
| Stack maps, frame walk | `coil-native` (`stack_maps`, `define_trampolines`), `machine::native` (`register_stack_maps`, `install_aot_stack_maps`, `for_each_frame_root`) |
| Inline allocation | `machine/src/memory/inline.rs` (`inline_object`, `layout_fingerprint`), `HeapCounts` |
| Static `extern` libraries | `--link-static` in `src/cli.rs`, `src/package_app.rs` (`LinkInputs`), `coil-native` `static_symbol_table`, `machine/src/ffi/static_libs.rs` |
| Tests | `tests/native_jit_cli.rs`, `tests/package_cli.rs` (`aot_package_links_extern_libraries_statically`) |
