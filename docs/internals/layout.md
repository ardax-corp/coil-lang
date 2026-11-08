# Value representation

How a value of a static type is held in a word, crosses a call, and reads
to the GC. The code is `compiler/src/layout/` (`rep.rs` decides,
`abi.rs` answers call edges); the tests that hold it to this page are the
generated matrix under `tests/representation/`.

## The rule

**`rep(T)` is a function of the static type only.** A value of type `T`
has the same word wherever it lives: ground code, a shared generic body, a
per-type clone, a dictionary or closure adapter, a host call, a native frame.
A bare type variable `V` in a shared body holds `rep(ground T)`: the raw
niche word for `Option<heap>` / `Result<heap, heap>`, the pointer for an
object, the immediate for a scalar. A shared body never interprets a bare
`V`; it moves it, stores it, roots it and passes it to dictionaries, which
are ground instances that already expect `rep(ground T)`.

`layout::rep::of` returns one `Layout` per type, and every other answer is
a view of it: `value_layout` (the one-word encoding codegen, `HostInvoke`
packing and FFI repacking use), `word_kind` / `array_elem_stamp` (what
the GC and array stamps read a word as), and `abi::two_word_kind` (the
two-slot call width).

## Representations

| Type | Word | Notes |
|------|------|-------|
| `int`, `float`, `bool`, `byte`, scalar `#[repr]` enum | immediate | |
| class instance, `string`, `Vec`, record, tuple, payload enum | pointer | |
| `Option<T>`, `T` a ground heap type | niche: `0` = `None`, else the pointer | `Layout::NicheOption` |
| `Result<(), E>`, `E` a ground heap type | niche: `0` = `Ok(())`, else the error pointer | `Layout::NicheUnitResult` |
| `Result<T, E>`, `T` and `E` ground heap types | niche: pointer = `Ok`, `pointer \| 1` = `Err` | `Layout::NicheResult`; never steals int bits |
| `Option<int>` (any immediate inner), `Result<immediate, _>`, `Result<(), immediate>`, closed payload enums with every arity `<= 1`, arity-2 immediate tuples, two-field immediate `readonly class` (#1562) | boxed object | `Layout::Pair`: two slots only under the call convention below |
| `Option<Option<_>>`, `Result<Result<_>>`, mixed heap/immediate, enums with `fn drop` | boxed `ObjEnum` / `ObjTuple` | `Layout::Word` |
| `Option<T>` / `Result<T, E>` with `T` open | boxed `ObjEnum` | see [Open enums](#open-enums) |
| bare `V` in a shared body | `rep(ground T)` | |

The niche and two-slot families are disjoint: a niche needs heap payloads,
a pair an immediate one. Values are not tagged; the static type picks the
decode (a compare with `0` for Option, bit operations for Result). No
pair- or niche-specific operation exists.

**GC.** A niche word is a pointer root: root marking strips bit 0 before
`find_object_by_addr`, and `values_eq` compares the stripped words. A
two-slot pair is two scalars, which are no roots.

**No boxes at generic edges.** A word crosses a shared generic body, a
dictionary entry, a closure or `PolyFn` adapter and a clone thunk as it
is: `rep(ground T)`. Where the static type changes at the edge, HIR
marks it with `Builtin::Retype` (dictionary entries and thunks) or a
word flag (`HirCallSite::words` / `word_ret`, `HirPolyCall`,
`HirAdaptConv::Word`, `HirDictOp::Word`), and MIR lowers it to the same
bits retyped (`MirBuilder::ins_word` / `ins_word_as`: only a `float` or
`bool` changes MIR type), so nothing boxes a word at an edge. `Show` of a scalar passes its type
on `STRINGIFY`'s operand instead of boxing for the tag.

## Shared generic bodies

A ground call of a generic function or method either clones the body per
type or calls the shared body through dictionaries. Both see the same
words:

- `hir_bare_niche_abi` (`codegen/emit_hir.rs`) gives a declared bare `V`
  parameter or return the niche layout when its ground argument is a niche
  type, so the call site passes the word unchanged.
- A bare `V` field, `Vec<V>` element or `HashMap` value read from ground
  code is the niche word (`hir_field_rep`'s bare-niche case).
- Dictionary, closure and `PolyFn` adapters pass a niche word as itself.
- A ground function passed where the shared body takes a function whose
  result is an open `Option` / `Result` (`T -> Result<U, E>` in
  `Result::and_then`) returns its niche word, but the body reads that
  result as the enum object. The call wraps the argument in a fn adapter
  that boxes the word (`HirFnAdapt::ret`, `hir_open_ret_adapts`), a lambda
  literal included.
- `hir_method_clone` clones niche types too: the per-type clone is a cost
  decision (speed), never a correctness one.

### Open enums

`Option<T>` / `Result<T, E>` whose payload is still a type variable *is*
interpreted by the shared body (it matches on it), so there the value is
the boxed `ObjEnum`. The edge from ground code converts once: `niche_args`
/ `hir_niche_arg` (`codegen/emit_hir_mir.rs`) box a niche word into an
`ObjEnum` on the way in, and `generic_enum_layout` (`codegen/compiler.rs`)
unboxes on the way out. A generic-head instance's `self` receiver (#1609)
is the main user. This is the only place a value changes representation at
a boundary, and it is keyed on the static type of the parameter (open
enum), not on the caller's context.

### Residue boxes (E0808)

A two-word value has no two-word form where the code that reads it takes
one word for every type. Those places box it, and the project's own code
warns there (`ErrorCode::BoxedResidue`, `Compiler::note_box_residue`):

- a class field typed by a class parameter (`v: V` at `Option<int>`),
  which is one word in every instance because the class's shared bodies
  read it so; it warns at the `new`, once per field and type;
- a value packed as a bare-trait existential (`let s: Named = pick`);
- an argument to a `forall` function value (`PolyFn`), whose shared body
  takes one word.

A call the clone budget sends to a shared body boxes its two-word
arguments too; E0804 already warns at that call. Two other boxes stay
without a warning, because nothing in the source chooses them: a
generator's yielded value, which shared consumers resume as one word, and
a two-word payload nested in another two-word value. A dependency's code
(a path outside the project or under `.deps`), a clone compiled for
another module and compiler-made code never warn. `coil dissect --boxes`
lists every box, these included.

### Lane arrays

A `Vec` or array of a closed two-word type (`Option<int>`, `Result<int,
string>`, a user enum without `drop` whose payloads are each one word, an
arity-2 tuple of immediates; not ranges or readonly pair classes) keeps
both words of each element in place: `ArrayData::Lanes { words, k: 2 }`,
the element's second word its tag, and the first a heap reference only
under the tags of the array's `tag_mask` (`layout::pair_lane_shape`). The
collector and compaction read each first word by its tag.

Ground code reads, stores and pushes the two words (`LaneIndex`,
`LaneStore`, `LanePush`); a literal is an `Alloc` of kind `Lanes`, and a
`for` loop binds the element's words. Code that takes one word per element
(a shared generic body, a collection's internals, a host native, `==`,
`Show`) never sees a lane array: where a lane array's value goes to such a
use (`hir_lane_escapes`), `LanesToWords` turns it into boxed elements in
place, as the op's `MirLaneShape` says to box them. The array stays the
same object, so every alias sees the change, and it never turns back. The
lane ops in turn accept an array of boxed elements (one a generic body
made, or one converted earlier): `LaneIndex` unboxes the element, and
`LaneStore` / `LanePush` box the new one. A one-word element op on a lane
array is a compiler bug and panics in the runtime.

Lane arrays are never stack arrays. The `Vec` methods keep the lanes
(`hir_lane_calls`): `pop`, `remove` and `insert` lower at the call (or at
the native a thunk inlined into the caller calls) to `ArrayLen`, the lane
ops and `vec_drain` / `vec_rotate_in`, which move the words in place; their
`Option` result boxes the element as any `Option` of a two-word value
does. `clear`, `reserve`, `capacity` and `Vec::from` call their thunks,
whose natives take either form, and `Vec::new()` / `Vec::with_capacity(n)`
of a lane type allocate an empty lane array. An early free or a
copy-on-write copy of the array keeps it in lanes as well. `coil dissect
--boxes` lists each place an array still converts (`lane array`).

## The two-slot calling convention

The two-slot form of a `Layout::Pair` type is a calling convention, not a
representation. `abi::convention_ret` / `abi::convention_param` decide it
from `CalleeFacts` (`escaped`, `coroutine`, `host_native`,
`instance_method`, `inherent_method`, `overloaded_or_generic`):

- A *direct* call of a ground callee whose address never escapes moves
  `[payload, tag]` (or `[a, b]` for products and pair classes). In MIR the
  second word is the `Call`'s `dest_hi` and `Terminator::Return { hi }`;
  both engines pass it beside the first (`NativeCtx::ret_hi`).
- "Escapes" is the fail-closed whole-compile `typechecking::fn_value_escape`
  walk: any function value, partial application, `thread::spawn` or FFI
  callback of that name, in any file of the package use-graph, keeps every
  caller on the one-word ABI for its parameters. Its result too, unless
  the value can target an adapter (`Compiler::fn_value_redirects`): then
  the function returns two words to direct calls, and its value enters
  `f$value`, a one-word adapter that boxes the result, whose `pair_twin`
  is the function itself. That covers free functions of any module (the
  adapter is reserved once per compile, in whichever module first takes
  the value, so every module's direct calls and values agree) and static
  methods; a partial application's closure (`f(a)`) targets the adapter
  as well, so a call through it that reads two words enters the function.
  Not redirected: overloads (the adapter is typed by name, not by
  candidate), generic functions (their value is the shared body's
  `PolyFn`), functions with a rest parameter, coroutines and host
  natives (neither returns two words to a call), and trait instance
  methods (a value never targets one; dictionaries hold its `$dict`
  adapter).
- Coroutines never return two slots; host natives return theirs through
  the context (see "Host calls"). A raw trait
  instance method (a ground instance with no context, not a default body)
  returns its pair to direct calls; its `$dict` adapter, which
  dictionaries hold, boxes it. Other instance methods return one word
  unless their definition pins the pair (`Iterator::next`,
  `IntoIterator::into_iter`). Overloaded and generic functions and trait
  instance methods keep one-word parameters (a numeric range's two words
  excepted). A free function's or inherent method's two-word parameter,
  an arity-2 immediate tuple or two-field readonly class included, is two
  words like its result; a method's receiver stays one word. (The stack
  VM kept these boxed: splitting a box into `[a, b]` needed a temp slot
  above live operands, which MIR parameters do not.)
- A local of a closed two-word type keeps two slots
  (`plan.pair_locals`), so `match r`, `r?`, `return r`, `let (a, b) = r`
  and `r[0]` do not allocate. A one-word init (an element, a field, a
  boxed result) unpacks once at the `let`; `x = e` rebinds both slots,
  loops included. `?` compares the tag in place and shares one fail
  epilogue per function (#297, #307). Only a local captured by a
  `defer`, or assigned as a value (`ADJUST`), stays one word.
- A lambda captures a two-slot local by value as its two words
  (`HirLambda::pair_captures`): the function value's `MakeFn` carries
  both, and the lambda's frame takes them as a two-slot parameter
  (`hir_lambda_capture_slots`, a `{name}$hi` slot after the capture's).
  Its operand counts capture words, not captures.
- A call through a function value of a two-word result is a
  `CallIndirect` with `pair` set: a lambda whose result is a pair is
  emitted twice, its one-word body (the value's entry) and a two-word
  twin (`MirFunc::pair_twin`), and the call enters the twin
  (`coil_native_fn_entry_pair`). A callee without one (a named function,
  an adapter, a runtime call) returns its box and the call reads the two
  words through a per-type `unpair` body (`__coil_unpair_N`).
- A class field of a closed two-word type sits flat: its first word in
  the field's own slot, its second in a tail slot after the declared
  fields (`Checker::class_flat_tails`), so no other field's slot moves.
  `ClassWordKinds` marks the second word scalar, and the first scalar
  for a `layout::scalar_pair` (`Option<int>`, `(int, float)`, a range,
  `Result<int, int>`, an enum of immediate payloads) or tagged for a
  payload that is a heap word only under some tags (`Result<int,
  string>`, `enum { Num(int), Word(string) }`): `WORD_TAGGED`, with the
  row's trailer naming the tail slot and the pointer tags
  (`layout::pair_pointer_tags`), so the collector reads the word as a
  precise pointer or a scalar by its current tag and an object it
  reaches can move. A payload whose kind is unknown leaves it unknown. `new` stores both words, and a read or write
  is two loads or stores (`pair_fields`). In a generic class only a
  field whose type names no class parameter is flat (`G<T> { v: T, o:
  Option<int> }` flattens `o`): its layout is the same in every
  instance, so a shared body and a clone read it alike. A `shared` (a
  two-word store is not atomic) or `view` class keeps one word per field,
  and a class with a flat field is never scalar-replaced into frame slots.
- A `static let` of a closed two-word type keeps its second word in a
  tail slot after every declared static (`Checker::finalize_static_tails`,
  `static_tail`): its initializer returns both words and `__module_setup`
  stores them, a store writes both and a read loads both. Its kinds are
  the pair's (`static_word_kinds`; a payload that is a heap word only under
  some tags stays unknown, an ambiguous root). Such a static is never
  folded to a snapshot at compile time, which would fill one slot. A
  `const` keeps one slot: it is only read.
- Everywhere else a value must be one word (another field, a `defer`
  capture, a container element, a shared body, a dictionary call, `Show`) it
  is its box: `box_pair_val` in MIR builds it, and two field loads read it
  back. One pack/unpack pair serves every consumer.
- `==` / `!=` with no instance compares two-word values on their words
  (`Lower::pair_eq`: an enum's tags, then the variant's payload; a
  product's two words; a `float` by IEEE, a heap word structurally) and
  niche words on theirs (`Lower::niche_eq`: the same variant, then the
  payloads). Only a heap word whose aggregate holds a `float` (a shaped
  compare) still boxes.

`coil dissect --boxes` lists every box a compile makes (function, kind,
type, lowering site); `compiler/tests/dissect.rs` asserts on the same
capture (`compiler::start_box_capture`).

`compute_two_word_return_kind`, `callee_param_pairs` and
`callee_has_unboxed_range_params` (`codegen/compiler.rs`) are the callers;
they build `CalleeFacts` and ask `abi`, so there is no second copy of the
policy.

## Host calls

Host natives pack their result into one word at the `HostInvoke` edge
(`machine::host_enum`, operand bits `[17:16]`), with layout bits derived
from `value_layout` of the invoke's result type: `1` for
`NicheOption` / `NicheUnitResult` (`Ok` = `0`), `2` for `NicheResult`,
`3` (pair) for a two-word `Option` / `Result` with a scalar payload
(`Option<int>`, `Option<float>`, `Result<int, IoError>`), `0` (boxed)
otherwise and whenever the type is unsure. `Vec::pop` / `remove` of a heap
element and `gc::get` / `unroot` / `upgrade` pack once. Under the pair
layout a native returns the payload word (`0` for a variant without one)
and the runtime leaves the tag in `NativeCtx::ret_hi`, which native code
and the MIR interpreter read into `HostInvoke`'s `dest_hi`; no enum object
is made (`host_enum::pack_option` / `pack_result`; a native that builds its
object anyway is read off it at the edge). A position that wants the boxed
word (a generic thunk's result) still asks for layout `0`. Cross-module `Result<Path, IoError>` keeps
virtual-error tags for layout (COI-404).

## Representation matrix

`scripts/gen_rep_matrix.py` writes one program per (representation,
position) cell into `tests/representation/`: 12 representations (`int`,
class instance, `string`, `Option<string>`, `Result<string, string>`,
`Option<int>`, `Result<(), E>`, `(int, float)`, pair readonly class,
`#[repr(int)]` enum, `Option<Option<string>>`, payload enum with
`fn drop`) times 20 positions (ground argument and return, unbounded and
bounded generic methods, generic return, bare field, `Vec` element,
`HashMap` value, tuple and record, closure capture and lambda parameter,
`thread::spawn` round trip, host returns, `==` / `show` / `Hash` through a
dictionary, the consumer `match` / `??`, clone vs shared body). Expected
output comes from the ground path. Refused cells go to
`tests/representation/compile_fail/` with their issue; crashing cells are
listed in `scripts/rep_matrix_known_wrong/`. CI's fmt job runs
`gen_rep_matrix.py --check`; `--probe` runs every cell under default and
`-O0`.

State when the niche-vs-boxed series landed: 223 of 234 cells green (199
before). The red cells are `#[repr(int)]` enums in containers and across
threads (#1903, #1904) and `Result<(), E>` through a `HashMap` entry
(#1912, a typechecker bug).

## Performance

Steps of the series and the hit benches (release build, 4-core container,
15 interleaved runs, median with min in brackets):

| Bench | Two-slot convention via `abi` (#1914) |
|-------|------------------------------|
| `pair_int_churn` | 1.298 s (1.185 s) |
| `option_int_churn` | 1.019 s (0.939 s) |
| `readonly_class_pair` | 0.225 s (0.200 s) |

#1914 compiles `tests/`, `examples/` and `examples/perf/` to the same
archives as #1913, so these benches are unchanged by it. #1913 (niche `V`
in shared bodies) changed archives only for bodies that stopped boxing a
niche word; `examples/perf/` compiled byte-identical. The readonly pair
class layout's own numbers are in
[limitations.md](limitations.md) ("Readonly classes as pairs").
