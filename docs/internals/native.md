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

`NativeProgram::keep` owns a program's code, and it goes with its
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

A view has a state word (`machine::VIEW_PLAIN` = 1): a view stores and loads in
place when it is set (`ok != 0`). The state is part of `layout_fingerprint`, so
AOT code built for another protocol is refused.

`Cow`, `CowIndex`, `Alloc` and `ArrayPush` call `coil_native_cow`, `_cow_field`,
`_cow_index`, `_alloc` and `_array_push`. A unique check (`Cow` unique, with a
field or not, and `CowIndex`) tests the word inline first: the element is read
as `Index` reads it, and only an object whose header has the shared flag calls
the helper, on a cold path. `CowIndex` leaves the kind to the helper, which
copies only a class instance or an array and hands back the element. More ops follow the same pattern:

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

A safepoint is the entry and loop headers of a body that may allocate, taken when
an allocation found work due (`NativeCtx::gc_due`: a `drop` counts left due, or the
cycle collector on allocation pressure), and inside helpers that may run code. Nothing
moves, so native code keeps its words in registers across them; only array views are
re-read when `gc_epoch` moved (`epoch_mark` / `epoch_check` around such calls).

Frames are no roots. The count operations the compiler inserts (`RcInc` / `RcDec`,
inline in native code) make every word a frame uses owned by a count, the frame's own
or a holder's that outlives the use, so the cycle collector needs nothing from frames
([memory.md](memory.md#why-frames-are-no-roots)). There are no stack maps, root
records or helper trampolines. Code is still compiled with frame pointers, for
profilers and backtraces. A word dies at its last use (its owned count is dropped
there); a `gc::root` handle keeps its value until `unroot`. A weak reference or a
finalizer can therefore see an object die at its last use, not at the end of its
scope. The interpreter follows the same rule ([mir-interp.md](mir-interp.md#how-it-runs)).

`compiler/src/mir/frame_words.rs` holds the word-kind analyses the count insertion uses.

## Host calls

Host natives keep frozen ids: the `*_ID` consts in `common/src/host.rs`, with a table
in `machine/src/host_natives.rs`. Retired ids stay as panic stubs, so later ids do not
move. A `HostInvoke` carries a layout for `Option` / `Result` results (`0` boxed
`ObjEnum`, `1` `Option` pointer niche or `Result<(), E>` with heap `E`, `2` heap-heap
`Result`, `3` two words), and natives build that layout once through
`machine::host_enum`. A boxed layout's higher bits give the word kinds of the
`Some` / `Ok` and `Err` payloads, from the call's type in a ground body
(`common::host_layout_with_kinds`, read back with `host_enum::host_payload_kind`);
`Vec::pop` / `remove`, one thunk for every `T`, take theirs from the array's
element kind.

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
- FFI ops (`Load`, `Declare`, `Invoke`, `Addr`) run through `coil_native_ffi`.
- An `extern` block's call is an `ExternCall` with the declared signature. When its
  parameters are `int`, `float`, `bool`, `string`, `constbytes`, `bytes` or `ptr` and
  its result is `int`, `float`, `bool`, `ptr`, `string` or `void`, native code calls
  the C function directly at the address module setup stored (`FFI_OP_ADDR`), with
  the platform's C calling convention: no runtime call and no libffi. A `bool` goes
  as its byte and comes back as `true` for any nonzero byte. Each pointer argument
  comes from `coil_native_ffi_arg`: a `string` as a NUL-terminated copy for the call
  (dropped by `coil_native_ffi_done` once the result is read), a byte view or a
  `Vec<byte>` buffer lent in place (C's writes land in it), a `ptr` word as it is. When
  the runtime's call must make one instead (an interior NUL, an empty or word
  buffer it copies, a buffer that is also another argument, an array a `ptr`
  copies), the call goes there (`FFI_OP_CALL`), which also reports the error. A
  returned C string becomes a coil string (`coil_native_ffi_str`). Nothing collects
  during native code, so a lent buffer stays put. An `extern struct` argument or
  result (a record) goes direct too when its fields are numbers and bools: the
  signature carries each struct's C layout (`MirExternSig::structs`, MIR format 20),
  `coil_native_ffi_struct_in` writes a record's C bytes into a stack buffer (a field
  the runtime must make, such as a string, sends the call there) and
  `coil_native_ffi_struct_out` makes a record of a result's bytes. Where the bytes
  travel follows the platform: x86-64 System V passes a struct of up to 16 bytes as
  its eightbytes in general-purpose or SSE registers (all of them, or the whole
  struct on the stack, a Cranelift `StructArgument`) and a larger one on the stack;
  AArch64 passes a homogeneous float aggregate in `d` registers, others up to 16
  bytes in `x` registers and larger ones as a pointer to a copy; Windows x64 passes
  a struct of 1, 2, 4 or 8 bytes in a register and any other as a pointer to a copy.
  A result too large for registers comes back through a hidden pointer
  (`StructReturn`: `rdi` on x86-64 System V, `rcx` on Windows x64, `x8` on
  AArch64; an AArch64 struct that would be split between registers and the stack
  goes through the runtime). A variadic function's arguments after the declared ones (`...`) go
  direct as C's default promotions make them (a `bool` as an `int`; no struct): as
  declared ones on AArch64 Linux; on Windows x64 a `double` in the integer register
  of its position, which is where the callee's `va_arg` reads it; on Apple arm64
  each in an 8-byte stack slot, after zero words that use up the registers the
  declared arguments leave; on x86-64 System V through `coil_native_va_call`, which
  sets `al` (an upper bound of the vector registers used, which Cranelift does not
  set) and jumps to the C function, whose address is the call's last integer
  argument after zero words, in `r9`, or, when the arguments take all six integer
  registers, through `coil_native_va_call_f` with the address as the last `double`
  after zero ones, in `xmm7` (a call that takes every integer and vector register
  goes through the runtime). A call of more than 64 words (library, function id,
  arguments and a variadic call's tags) keeps its argument tuple and goes through
  the runtime. A sized integer (`int32`, `uint8`, …) is an `int` in coil: an
  argument is narrowed to its C type and sign or zero extended to a whole register
  (what C callers do), a result sign or zero extended back, as the runtime's call
  does. A C callback (an extern parameter of function type, `fn(int x) ->
  int cb`, whose argument names a top-level function) goes direct as the address of
  a thunk the backend emits per callback function and signature,
  `coil_cb_thunk_<body>_<aux>`: a C-convention entry that stores its arguments as
  words and calls `coil_native_callback`, which runs the body on the program through
  `Driver::callback` (a C string becomes a coil string) and returns the result word.
  The signature (up to five `int`, `float`, `bool`, `ptr` or `string` parameters;
  an `int`, `float`, `bool`, `ptr` or no result) is in the `CALLBACK` tag's aux
  (`common::encode_callback_sig`), so the archive is unchanged. In a program that may
  make a C callback into coil (an extern function that takes one: a library may keep
  a callback and run it during a later call, `may_call_back`), every direct extern call goes through a trampoline
  per call shape, `coil_cb_call_<k>`: it calls `coil_native_cb_enter` (which makes
  this thread's context the one a callback runs on), calls C, and then
  `coil_native_cb_leave`, which reports a panic in a callback as the call's. A call
  with a pointer argument other than a C string (`bytes`, `Vec`, a struct) still goes
  through the runtime there. A call into `sum` costs about 3.5 ns direct against 270 ns through
  `invoke` on a tuple, and about 10 ns against 125 ns through the trampoline in a
  program that may call back; one passing a 12-byte string about 40 ns against
  315 ns; one passing a 16-byte struct about 18 ns against 400 ns through the
  runtime's call, and one that also returns one (a new record) about 60 ns against
  760 ns. A call to `apply_cb(doubler, i)` (C calls the coil function once) costs
  about 60 ns; through `invoke` with a `Callback` about 400 ns, where the runtime
  used to make a new libffi closure on each call (about 1.5 us, and never freed);
  it now keeps one per function and signature.

`CallIndirect` runs through `coil_native_call_value` and `Driver::call_value`, which
handles exact calls, partial and over-application and dictionaries. A function value
a host native calls (a closure passed to it, a finalizer, a callback) runs the same
way. `Alloc` of a function value (`Fn`, `PolyFn`, `PolyFnCapture`) builds the `ObjFn`
in `coil_native_alloc`; the body it names counts as a callee. `ObjFn` keeps a function
value's arity in 8 bits, so calling a function value of 256 or more parameters panics.

A `gen fn`'s body runs only on its coroutine, so a value of one never names the body
(#2747): the call through it must make the coroutine as a direct call's `MakeCoro`
does. A ground generator's value (its name, a partial application, a static method's
value) targets its `name$value` adapter (`Compiler::fn_value_target`), a function
calling it, whose call is the `MakeCoro`. A generic one read at ground types targets
`name$value<T>` (`generic_value_target`, the adapter any generic function's value at a
ground type whose shared body holds other words gets, #2755), which calls it at those
types and so takes its per-type clone; the mono plan clones a generator for each such read
(`candidate_for_value`, past the clone budget, as a call through an alias). Its
`PolyFn` (a `forall` argument) targets `name$coro` (`coro_trampoline_target`), MIR that
takes the shared body's words as they are (parameters, dictionaries, `kinds` word) and
returns `Alloc Coro` of them; that shared body has the limits of "A value nobody
produced" below, so a generator whose receive type is open is refused there (E0127,
#2748).

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
a heap home owns a count of its word. The normal exits run the defers
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
which suspends the fiber with the value and its call's context, and the
`ObjCoroutine` owns the fiber. The frame's words stay on the fiber's stack, owned by
the frame's counts; the generator's saved words are cleared once it starts, and only an
unstarted generator owns its arguments. The next resume sends its word. A resume of a finished or invalid handle panics at native code's own
site. `yield from h` in a native body is a loop of resume, done check and yield, the
sends passed through. Two-word returns are refused.

### A value nobody produced (#2717)

Two places read a value no one gave: the `resume` that completes a body ending
without `return e` (falling off its end, a bare `return;`, in a `Result` yield type
too), and a binding `yield` resumed without `with` (a `for` sends nothing). Both
read the word `0`, and the MIR lowering (`Lower::missing_value`) decides what that
is by the type (`layout::zero_of`), the same on every engine:

- a type with a **zero** (numbers, `byte`, `bool`, `()`, `string`, `Option`, `Vec`,
  and tuples, records and fixed arrays of such) reads its zero: the `0` word itself
  where that is the zero (`0`, `0.0`, `false`, a niche `None`), else the zero
  object, built from literal nodes the `Resume` / `Yield` holds (`zero`, made by
  `hir::synth::zero_value`, at a generic generator's per-type clone in
  `hir_instance`);
- a type with **no zero** (a class, `Result`, an enum other than `Option`, a
  function, an existential, a range, a product holding one) panics, `the generator
  finished without a value of T` / `resumed without a value of T`, at the read;
- a **statement** (`resume h;`, an unbound `yield e;`, a block's tail lowered for
  its effect) reads nothing: no zero, no panic, no pair unpacked; a `for` binds its
  variable only once the generator is known to have yielded.

Where the word `0` is itself a value of a type with no zero (`Result<(), E>`'s
`Ok(())`, a scalar enum backed by `0`), another word stands for no value
(`HirSite::missing`, `hir_missing_word`: `1`, or a backing no variant has): a gen
fn's no-value completion returns it, a `resume` without `with` and a `for` send it
where the receive type is such a type, and the read compares with it. A
`bool`-backed enum with both variants leaves no word free and reads `0` as `false`'s
variant. A shared generic body cannot tell a value nobody produced from a word of
its bare `T` (the word `0` is an `int`'s value), nor make `T`'s zero, nor knows the
word that stands for no value at `Result<(), E>` (#2738). So a free generic
generator whose yield or receive type is open in its type parameters
(`layout::open_coroutine`), and a generic function calling one by name, are
clone-only in the mono plan (`mark_open_coroutines`, as a keyed function is): every
ground call takes its per-type clone, past the clone budget too. Methods do the same
through their inherent clones (#2748): a generic `gen fn` method (of a generic
class, with type parameters of its own, a static one) and a generator called from a
method's per-type clone are clone sources (`note_method_clone_src`,
`note_fn_clone_src`), `fn_sig_split` makes them, and a generic method or function
calling one by name or on `self` (`note_open_coroutine_callers`), clone-only, and
`hir_method_clone` takes a generator's clone only where it compiles from its body (a
thunk calling the shared entry would make the shared body's coroutine). The clone
is a generator (`is_coro_key`: its call is `MakeCoro`, never a tail call, its
arguments one word each), and neither it nor a method calling such a generator is
spliced into its caller (`hir_method_clone_body`). A generic generator whose receive
type is open, passed as a `PolyFn` (a `forall` argument), is E0127: no clone serves
that read and its shared body would hand an unsent binding `yield`'s word on. A
call at open types (polymorphic recursion, which the checker does not type for a
generator today) would still take the shared body, which moves the word as it is.

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

A reduce (with or without stores into word arrays), a store map and a push compile to
native loops (`native_reduce`, `native_store`, `native_push` in `coil-native/src/lib.rs`) over two
layouts: `int` / `float` word arrays, and packed bytes, where a map or push stores
`int` lanes into a byte array from strings and byte arrays (an ASCII lower case). A
guard in front checks every lane's element is in its array's view (word arrays) or
bytes (`bytes_guard`, `bytes_store_guard`); then blocks of 8 lanes run
on 128-bit vectors (`i64x2` / `f64x2`, four per block, `vlane`), and the last lanes, and
any block with a lane that would trap, one lane at a time (`lane`), which traps where the
scalar loop does or hands the rest to the runtime's lane code. In a block:

- Every value is read and computed before any is stored, the order `vec_loop` already
  requires of the runtime's 8-lane blocks. A reduce that stores also checks its fold
  for a trap first, so a block run again one lane at a time reads what the scalar loop
  does; one lane at a time, a trap leaves its lane untouched and the runtime's lane
  code goes on from there with the fold so far (`vector_lanes_from`).
- `int` `+` / `-` check overflow by the sign bits of the result and operands; `*`, `/`
  and float-to-int casts have no vector form and run per lane inside the vector (a zero
  divisor divides by `1` and flags its lane); a product with a `0` / `1` compare is a
  mask.
- Folds whose `int` result is the same in any order keep an accumulator per lane:
  and / or / xor, a min or max (ties are one value), and an add while every term is in
  `-2^52 .. 2^52` and the sum before the lane sums below `2^61` in size, for 64 blocks
  at a time (then no partial of the scalar loop's left fold can overflow, so it is the
  sum that loop gets). Past those, a block folds one lane at a time. Float folds and
  `int` products fold the block's terms in lane order.

Over bytes, a block reads 8 bytes of a source in one load and widens them to four
`i64x2`; its values go back as one 8-byte store (`vec_pack_bytes`, a `pshufb` per
vector: a two-operand shuffle may lower to AVX-512, which valgrind cannot run). A
value that is no byte (`> 255`) leaves the block to one lane at a time, where it goes
to the runtime's lane code from that lane.

A push appends a block in place while `out` has room, its length in a register and
stored when the run ends. With less room than a block, `out` grows first
(`coil_native_array_reserve`, by the lanes left but at most its length, or 64, so a loop
that traps early does not take room for every lane). A block that would trap, and the
last lanes, push one at a time through `coil_native_array_push`, which grows `out` (and
turns bytes into words for a value that is no byte; the next run checks the layout
again). A push of the same value in every lane stays with the runtime: MIR fills that
in one `vec_fill` and keeps the loop only for where the fill did not apply. Every way
out of a native loop to the runtime's lane code (a trap from its lane on, or an access
the guard could not show in place) shares one call (`vector_lanes_at`), which copies
the op's body into the frame.

A rough per-lane cost (`lane_cost`, `vectors_pay`) picks the vector loop only when it
beats one lane at a time: a body mostly of per-lane ops (`spectral_norm`'s `/` and `*`)
stays scalar.

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

An `Alloc` of a fixed-size bump-allocated object (an enum variant or tuple, an enum object of
`PRODUCT_TAG`, of 1 to 4 words, an
instance of a class with 1 to 4 fields) does not call `coil_native_alloc` on its fast
path. Native code loads the heap's gate (`HeapCounts::inline_gate`, closed in a steal
epoch and under `rc-check`), and for an instance tests its type's bit in the
finalizer types (a type with a `drop` stays old). It then bumps its size class's cursor
(`Bump`, boxed in the slab, indexed by a class fixed for every heap), writes the slot's
image with the payload words in place, and adds the object to `alloc_bytes` and
`live_count`. A full chunk, a closed gate or a `drop` type falls back to the helper,
which refills the chunk and sets `gc_due` when the cycle collector is due, so it still
runs at a safepoint on time. A free pushes the slot on its class's free list inline.

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
that the store is inline or spilled, and that the slot is under
the length. It then loads or stores the slot word. A failed load check is a
compiler bug and bails, a panic, with no helper call; the interpreter's
`coil_native_product_load` bails on the same objects. A failed store check calls
`_set_field`, which panics as before.
`layout_fingerprint` covers these offsets, so an AOT object made against another
layout is refused.

## Map table ops

`HostInvoke`s of `map_find`, `map_upsert`, `map_live` and `map_len`
(`key_index::map_*`, [key-index.md](key-index.md), "Map tables") and of
`hash_string` run in place (`lower_map_op`), with the host call as the cold
path. A find or upsert runs in place only when its key mode is a constant
(after `eq_mode` folds) that the probe compares itself: `1`, words, or `2`,
strings. An upsert runs in place only where pushes may be written in place
(`ArraySlots::inline_push`: not in debug or `rc-check` builds).

- The table and the keys are read at the `array_slots` offsets (kind byte,
  words tag, pointer, length, capacity), as the inline `ArrayPush` reads
  them. A table must be well formed (`mask + 1` a power of two, the words
  long enough for the cells) and a candidate slot under the keys' length,
  or the op takes the host call. So a hand-made `Vec<int>` can never make
  native code read outside its storage.
- The probe compares the cell's hash, then the key word. A string key
  whose word differs calls `coil_native_map_str_eq`, a leaf helper with no
  context, on a hash match.
- An upsert of an absent key writes the cell, the two counts and the slot's
  word, and bumps the table's length, as `map_table::add` does, when the
  cells stay at most half full and the storage has room. Otherwise
  (growth, a full `Vec`) it leaves everything to the native. The table's
  views are dropped after an upsert.
- `hash_string` reads the hash cached in the string's header
  (`string_slots`: the kind byte, `ObjString`'s hash word and its "set"
  bit) and calls the native when the hash is not cached yet.

`layout_fingerprint` covers `string_slots`.

## Inline share

A copy-on-write share of a word (`Cow` with `unique` false and no field) sets
the object's shared flag in place, with no call (#2506): the word with its low
tag bit cleared, nothing for null, then the header's kind byte, and for a class
instance or an array (`KIND_INSTANCE`, `KIND_ARRAY`) an or of `FLAG_SHARED`
into the flags byte at `HEADER_FLAGS_OFFSET`. Those constants sit next to
`GcHeader` (`machine/src/memory/heap.rs`), tied to it by `offset_of!` and
size asserts, and `Object::set_shared` sets the same bit;
`layout_fingerprint` covers them. Any other word is left as it is, as
`Heap::cow_share` leaves it. A share allocates nothing (`OpProps` of a share
`Cow`: no `allocates`, no `gc_edge`), so a body whose only copy-on-write op on
a type parameter is a share behind its `kinds` test stays without a GC poll
and with no helper call. The flags byte is a `Cell<u8>` the runtime sets from a
shared reference, never through `GcData::as_mut`, so the inline store is the
same in debug and `rc-check` builds. The interpreter keeps `coil_native_cow`,
which also counts the share for `--cow-report`'s summary line; native code's
inline shares are not in that count (the copies are).

The reference count ops `RcInc` / `RcDec` (archive 8.5, reference counts plan
step 3) call `coil_native_rc` (`ctx`, the word, `1` for a decrement), behind a
branch on the op's guard when it has one. The helper does nothing unless the heap
keeps counts (`COIL_RC_CHECK` today) and the word names an object
(`Heap::rc_retain` / `Heap::rc_drop`); a decrement of the last owner releases the
object once `Heap::set_rc_frees` is on: what it names loses an owner in turn, and
it is freed (strings, arrays, enums and instances; anything else, an object with
a pending `drop`, and everything during a collection or a steal epoch stays for
the collector at count zero). The interpreter calls the same helper. An inline
increment and decrement come with the insertion pass (steps 4 and 6).

A pinned object (archive 8.2, [cow-calls.md](cow-calls.md), "Pinned objects") is
never marked: after the kind byte the share tests `FLAG_PINNED` in the flags byte
and, when it is set, branches to a cold block that calls `coil_native_cow`, whose
copy is the share's result. That helper allocates and never collects, so the share
stays no GC edge and allocates nothing as far as the body's `OpProps` go: a copy
it makes is counted, and the next safepoint collects. `CowPin` and `CowUnpin`
are inline or / and operations on the same flags byte, for a class instance or an
array only (null and other kinds: `CowPin` gives `0`, `CowUnpin` does nothing).
The pin test and its cold call are emitted only in the bodies that may run
while an object is pinned (`Plan::pins`, `pin_reach`): the bodies with a
`CowPin` and what they run. That is their calls and the function values and code
pointers they make. Once one of them calls a value, the host, a generator or
foreign code, it is also every body whose code is taken, and the `drop`s
when one allocates or frees. A pin is set and cleared by one body, on an object
that body writes in place and its caller made unique, so no other body can name
the object while the pin is set. A pin left set at an exit is stale: no in-place
writer of it is live, so a share that ignores it is right. Every body tests the
pin when the reach yields or calls a native that needs the runtime (the task
may park, and any task may run), and in a program that starts a thread. Elsewhere
the share is the one above. The call alone made each body with a share a non-leaf:
a stand-in `bbox` +14.5%, and `bint` at `--clone-budget 0` +2.2%. With one
`split` (which pins) in a `bbox` program, testing every body cost +14.2%. With
the reach it costs +0.15%.

In-place stores are compiled only into release builds without `rc-check`
(`ProductSlots::inline_stores`): debug and `rc-check` builds check epoch writes in
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
refusal. If the object or the link fails (no `cc`, no `libcoil_rt.a`) the package fails
and says so: the runner is several times larger, so it is packaged only with `--no-aot`.
On Windows `cc` is a mingw toolchain's. A runtime library built in debug carries
`coil-rt profile: debug;` (`coil_rt::PROFILE_MARK`), and `coil package` warns when it
links one. On Linux the link uses lld when `cc -fuse-ld=lld` works (about 40 KB
smaller), else the default linker; a `--link-arg` naming `-fuse-ld` picks it instead.
`.github/workflows/native-platforms.yml` runs every way a program starts on macOS and
Windows.

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
  relocations, entries and sites. `load_image` declares the same
  functions in the same order, so `FuncId`s match, and hands the bytes to
  `define_function_bytes`, which relocates them for this process. Code holds no
  addresses of its own: the AOT object relocates the same lowering.
- **Damage:** the image carries a sha256 of itself. One that does not load warns, goes,
  and the run compiles. Machine code is not checked otherwise, so the directory is
  `0700` and a hit trusts its owner, as running their own binaries does.
- **Size:** at most 256 entries, least recently used first, as the build cache.

`COIL_CODE_IMAGE_CHECK=1` sends every JIT through an image and runs the copy, and fails
if the code does not fit one. CI's `native` cell runs the corpus so, and the rc-check cell runs
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

On feature/native 6e9943a (2026-11-20) a hello world was 1,107 KB with `--strip-symbols
all`. Two changes take it to 675 KB (-39%) with the same instruction counts:

- **FFI link gate** (-131 KB): `Driver::HOST.ffi` reaches the FFI hook only through
  `NativeStart::ffi` (`machine::FFI_HOOK`), and `run_package` reads the native lock and
  builds the `dload` gate only through `coil_cli::FfiLink`. `coil-rt` exports the full
  link as `coil_ffi_link`, and the AOT object's `coil_aot_ffi` holds its address when a
  body has a `MirInst::Ffi`, else null (`coil_native::ffi_link_table`). Without that
  reference the link drops libffi, `libloading`, the FFI registry, the `dload` gate and
  `sha2`. Every other start (`coil FILE`, `coil run`, the JIT runner, `coil debug`)
  passes the hook. A program with no FFI op whose package still says it uses FFI exits
  with "linked without FFI"; `coil package` makes neither. When every library the
  program's `extern` blocks name is linked into the executable (`--link-static`, or a
  shared library on ELF) and every extern call is direct (no `runtime_extern_call`,
  `Ids::runtime_ffi`) with no `MirFfiOp::Invoke`, `coil_aot_ffi` holds
  `coil_ffi_link_linked` instead (`coil_cli::FFI_LINK_LINKED`,
  `machine::FFI_LINKED_HOOK`): module setup's load, declare and address ops read the
  executable's `coil_static_syms` table, and there is no native lock, gate, `dload` or
  libffi (`examples/ffi_sum.coil`: 1,045 KB -> 905 KB with `--strip-symbols all`).
  A direct call makes its own argument copies (`machine::ffi::DirectCopies`, through
  `coil_native_ffi_arg` / `coil_native_ffi_done`) and panics on an argument it cannot
  pass, so it never retries on the runtime's libffi call.
- **Size-trimmed std** (-300 KB): `scripts/build_runtime.sh` builds the release
  `libcoil_rt.a` on Linux with std rebuilt (`-Zbuild-std`, `RUSTC_BOOTSTRAP=1`
  on stable, needs `rust-src`): without std's backtrace symbolizer (`gimli`,
  `addr2line`, demangling; Coil backtraces come from the program's line table), std
  optimized for size, and `-C force-unwind-tables=no` (the runtime is
  `panic = "abort"`). A Rust-level panic in the runtime prints its message without a
  Rust backtrace, and `perf` / `gdb` unwind runtime frames by frame pointer only.
  macOS and Windows keep a plain build until it is measured there. `COIL_RT_PLAIN=1`
  skips it.

### Per-program host natives

`machine::host_natives` builds the host table from groups (`STANDARD_HOST_GROUPS`, in id
order): a group is a `HostPush` that pushes consecutive ids, one per `push_*` of the
table, and one per `io` native (`push_io_kind::<K>`, a constant kind, so each keeps
its own closure). `host_groups!` also exports each group as a symbol,
`coil_host_group_K`, that holds its `HostPush`.

- `coil_native::aot` lists the groups the program's `HostInvoke`s name, with the
  groups every runtime calls (`core_host_groups`: `unwind_resume`, the collector's and
  `region`'s hooks), as
  `coil_aot_host_groups` (`machine::AotHostGroup`: the symbol's address, first id,
  count) and `coil_aot_host_group_count`. The link keeps those groups and drops the
  rest.
- `coil-rt`'s `main` installs them (`machine::native::install_aot_host_groups`) and
  `run_linked_package` starts on `aot_host_natives`: their natives at their ids and an
  absent native (`NativeFn::is_absent`, no name) at every other id.
- The start names no default table: `NativeStart::host_natives` is the table, and each
  host passes `machine::standard_host_natives()` (built once per process) or the AOT
  one. A default would name every group, and the link would keep them all.
- Pushing a group links the runtime part its natives run on, so a table without them
  leaves it out: the task natives, `contract_fail` and `coro_close` link the scheduler
  (`driver::link_tasks`: `HostOp::Task` and `HostEnv::park` go through `TASKS`), the
  thread spawns link the job runner (`worker::link_jobs`: the reactor runs a job
  through `job_runner`, and refuses it with none), and `startup_restore` links the
  snapshot restore (`driver::link_restore`). The standard table pushes every group,
  so every other start links them all. On a hello world this leaves out about 110 KB.
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
  table. Calls go as for a shared library: direct for scalar signatures, libffi
  otherwise.
- The native lock and `--check-native` skip linked stems.
- A declare of a symbol the table does not hold looks it up in the process.

### Shared `extern` libraries

On ELF hosts (`package_app::LINKS_SHARED_LIBRARIES`) an AOT executable links each shared
library an `extern` block names that has an `--ffi-native` row (not a system library,
not `--link-static`) at link time:
`package_app::shared_links` picks them and checks each file's sha256
(the native lock's) against its `--dload-pin`, failing the package on a mismatch, and
`cc` gets `-L DIR -l:FILE` (so `DT_NEEDED` is the soname, or the file name when there is
none) and the run path `$ORIGIN:$ORIGIN/lib`. Their functions go into `coil_static_syms`
like a static archive's, so at start the loader binds them, the runtime opens no file and
checks no pin for them (trusted by soname), and the natives cache is not searched. The
lock keeps their rows, for `coil natives dump` and the tools that ship the files. macOS
(the install name is the library's choice) and Windows keep opening them at start
through the `dload` gate.

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
| Count ops, safepoints | `compiler/src/mir/rc_insert.rs`, `coil-native` (`emit_rc`, `epoch_mark` / `epoch_check`), `machine/src/memory/rc.rs`, `machine/src/collect.rs` |
| Inline allocation | `machine/src/memory/inline.rs` (`inline_object`, `layout_fingerprint`), `HeapCounts` |
| Static and shared `extern` libraries | `--link-static` in `src/cli.rs`, `src/package_app.rs` (`LinkInputs`, `shared_links`), `coil-native` `static_symbol_table`, `machine/src/ffi/static_libs.rs` |
| Tests | `tests/native_jit_cli.rs`, `tests/package_cli.rs` (`aot_package_links_extern_libraries_statically`) |
