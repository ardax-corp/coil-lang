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
| `()` | pointer to the empty tuple object, where a word is stored or passed | no word in a local or a plain result: see below |
| `Option<T>`, `T` a ground heap type | niche: `0` = `None`, else the pointer | `Layout::NicheOption` |
| `Result<(), E>`, `E` a ground heap type | niche: `0` = `Ok(())`, else the error pointer | `Layout::NicheUnitResult` |
| `Result<T, E>`, `T` and `E` ground heap types | niche: pointer = `Ok`, `pointer \| 1` = `Err` | `Layout::NicheResult`; never steals int bits |
| `Option<int>` (any immediate inner), `Result<immediate, _>`, `Result<(), immediate>`, closed payload enums with every arity `<= 1`, arity-2 immediate tuples, two-field immediate `readonly class` (#1562) | boxed object | `Layout::Pair`: two slots only under the call convention below |
| `Option<Option<_>>`, `Result<Result<_>>`, mixed heap/immediate, enums with `fn drop` | boxed `ObjEnum` | `Layout::Word` |
| `Option<T>` / `Result<T, E>` with `T` open | boxed `ObjEnum` | see [Open enums](#open-enums) |
| bare `V` in a shared body | `rep(ground T)` | |

The niche and two-slot families are disjoint: a niche needs heap payloads,
a pair an immediate one. Values are not tagged; the static type picks the
decode (a compare with `0` for Option, bit operations for Result). No
pair- or niche-specific operation exists.

**Unit.** A body that knows a value is `()` keeps no word for it: a `()`
local has no slot, and a function declared `-> ()` returns nothing a
caller reads. Where a word is stored or passed, `()` is the empty tuple
object: an element of a `Vec<()>` or array, a tuple item, a class or record
field, an enum payload, an argument (so a shared body's bare `T` at
`T = ()` is that pointer), and the result of a shared body's bare `T`.
The value runs for its effect first; a read of such a place runs the
base and yields nothing. The collector reads such a word as a pointer
(`layout::word_kind` of `()` is `WORD_POINTER`, #2507): it is the unit
object or `0`, the word a dictionary adapter moves it as. So `()` is a type argument like any one-word
type, in a clone and in a shared body (#2489); a clone-only generic class
method (one whose shared body holds a nested open enum or a function over
its parameters in its own layout, `fn_sig_split`) takes its clone at
`()` as at any other type.

A ground `-> ()` function value a shared body calls where its result is
a bare type parameter (`fn() -> T` at `T = ()`) gets the adapter that
returns the empty tuple object (`HirFnRet::Unit`), as a niche result gets
the one that boxes it: the body stores or passes the word it gets. A
function value, a lambda and a closure passed as an argument all take
it. A function below the top (an element of `Vec<unit -> T>`, a tuple
item, a type argument) has no adapter, so such a signature takes a clone
for every ground call past the clone budget, as a nested open enum does
below, and a call still reaching the shared body at `T = ()` is refused
(`layout::nested_fn_var_result`, #2498).

A generic class's or enum's own methods are the same case: they call a
function in a field or payload declared `fn(..) -> V`, `V` the type's
parameter, and keep its result as their `V`, and a class's methods are
shared at any clone budget. So the function takes the unit adapter where
it is stored: `new C(f)`, a variant `E::V(f)` and a field store `c.f = f`
of a ground `-> ()` function wrap it (`HirPacks::unit_fns`,
`Compiler::hir_unit_fn_slot`). Ground readers ignore the object it
returns. A function below the top of such a slot (`fs: Vec<fn() -> T>`,
a tuple item, another type's argument, `Option<fn(T) -> T>`) has no
adapter, so the type is clone-only ("Functions in fields and payloads"
below): its methods and every generic over it at `()` are per-type
clones, which call the function as the ground `-> ()` call it is, and a
call that would still take a shared body there is refused
(`nested_fn_unit_slot` in `hir_shared_split`, #2560). Storing such a
value is refused with E0801 `unit-fn-nested` only for another module's
class at types that module does not know, whose methods are all its
shared bodies (`Compiler::hir_unit_fn_nested`). A dictionary method whose declared
result is the trait's type parameter, at an instance for `()`, goes in
through its `$dict` adapter, which returns the empty tuple object
(`Compiler::trait_method_unit_ret`, #2541).

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

**Heap products.** A boxed tuple and a boxed variant are one object kind,
`ObjEnum` (`tag`, `type_id`, then the payload). A tuple is the tag-less
product: its tag is `common::PRODUCT_TAG` (`u32::MAX`, which no enum
reaches), its `type_id` `0`, and `()` is the shared unit object of that
tag. Its words are the payload, in declaration order, with the packed
word kinds of the first four (`PACKED_KIND_WORDS`); later words are
unknown, which the collector resolves. Up to four words are inline, wider
payloads spill. The object is 48 bytes (56 with the header): a variant
lost 8 since `finalized` moved into the GC header, a tuple gained 8 for
the tag and type id. MIR spells a tuple allocation `MirAllocKind::TUPLE`
(`alloc.tuple`), an `Enum` of that tag; snapshots, thread copies and the
inline allocation images have no tuple form of their own. A class
instance keeps its fields in the same store (`EnumPayload`), at the same
offset in the object (8, after `tag` and `type_id` or `type_id` alone).

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
- A bare `T` argument is passed as the ground type's word (a `float` as
  its bits, a two-word value boxed), which is also the word an element of
  `[T; N]` or `Vec<T>` is. So the body's values of `T` are the same words
  wherever they come from, and an array, `Vec` or tuple of them it builds
  is returned as it is, at any depth (`hir_word_holder`, #2415). So is a
  user enum or class whose payloads or fields, at its arguments, are
  such holders (`List<T>` with `Cons(T, List<T>)`, #2512; `Box<T>` with
  `v: T`, #2511): a payload item is one word whatever its type, a field
  typed by a class parameter is one word in every instance, a field whose
  top is an open `Option` / `Result` (`r: Result<T, string>`) is the enum
  object in its declared layout on both sides, holding the words of its
  arguments (`hir_field_rep`), and an item
  the body wrote has the unknown word kind, which the collector resolves
  (a class's row keeps its generic fields unknown). A tuple or enum
  result whose own items are bare parameters' values is rebuilt at the
  call, to give it the ground type's word kinds (`hir_ret_items`,
  #1549); a class result is the body's object. A function parameter
  whose parameters and result are such words, or an `Option` / `Result`
  of them at its top (`T -> Option<T>`, `fn(T) -> (T, T)`, also in a
  `Vec` or tuple), gives the body the same words: its adapter passes
  and returns the ground words (`hir_param_words`, #2579). A call is
  still refused where `T` is a bare parameter and the result or another
  parameter holds it any other way (a function of functions, an enum
  with `drop`). An open `Option<T>`
  below the top is never shared: see "Open enums".
- A rest parameter `T... xs` is one `Vec<T>` argument: the call packs the
  rest arguments into it (`hir_call_shapes`), a `...vec` spread is the
  pack itself, and the shared body takes it as the clone does, a `Vec` of
  `T` words. A shared body's own call packs its values of `T` the same way
  (#2510). The pack's type is the one the checker unified its items to,
  the callee's `T` as the call bound it: `None` next to `Some(1)` is an
  `Option<int>`, and an empty pack takes it from the fixed arguments or
  the expected type (`Checker::rest_pack_ty_at`, #2537).
- An array literal of such values (`let v: Vec<T> = [x];`, `[(x, 1)]`) is
  the array object, never a stack array: a slot holds one word of a
  known kind, and a `T` word's kind is not known in the body
  (`Compiler::stack_slot_ty`, #2581).
- `hir_method_clone` clones niche types too: the per-type clone is a cost
  decision (speed), never a correctness one.
- A shared entry takes one more word after its dictionaries, its `kinds`
  mask: which of its type parameters are copy-on-write objects at the call,
  which its share and unique ops on a bare `V` test (#2506,
  [cow-calls.md](cow-calls.md)). A trait dictionary carries one after its
  methods ([dictionary-passing.md](dictionary-passing.md)).

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

A dictionary is not such an edge: a shared body calls its methods on a
bare `W`, the niche word. A bound's dictionary for an instance with a
generic head over `Option` / `Result` therefore holds the instance's
per-type clones at a niche type, never the shared entries (#2477,
[generic-instances.md](generic-instances.md)).

**Below the top nothing converts** (#2441). An open enum nested in a
parameter or result (an element of `Vec<Option<T>>` or `[Option<T>; N]`,
a tuple item, a type argument, the payload of another enum) lives in an
object both sides hold: ground code reads and writes its elements as
niche words where `T` is a heap type, the shared body as enum objects. A
copy at the call would lose the body's writes to the caller's object,
and the body cannot read the ground layout without being told each
type's. So such a call does not take the shared body. A function below the top
is the same case (#2488): an element of `Vec<unit -> Option<T>>` returns
the niche word from a ground closure, and only a function that is itself
the argument gets an adapter. That adapter converts the argument's own
parameters and result, not a function among its parameters: in `fn(fn(T)
-> Option<T>) -> T` the inner function's result is below the top (#2591). The rule reads the signature (and the
declarations of the types it names, below), `layout::nested_open_enum`,
for every ground type:

- a free function whose signature has that shape gets a clone for every
  ground call, past the clone budget, as a keyed or hot function does
  (`GenericFnSig::split`, `monomorphize.rs`);
- an inherent, static or instance method of that shape gets its clone
  past the budget too (`Compiler::fn_sig_split` in `instance_clone` and
  `method_clone_room`); a clone that would be a thunk around the shared
  entry does not count;
- a call that would still reach the shared body, with the nested enum a
  niche word at the call's types, is refused (`hir_shared_split`, E0801
  `callee-generic`). A method with type parameters of its own on a plain
  class takes its clone where its signature has the shape (#2447), and a
  call through a `let g = wrap;` value takes `wrap`'s clone, its type
  arguments bound from `wrap`'s signature.

**Nominal types** (#2446, #2493, #2509). A user class or enum whose
declaration holds such an enum is one itself, wherever it is named:
`nested_open_enum` reads a class's fields and an enum's payloads with
the type's parameters bound to its arguments, transitively and through
recursive types. A class's field is a top (it is stored in its declared
layout, `hir_field_rep`) unless it is a function; an enum's payload is
below the enum's top. So `W<T>` with `xs: Vec<Option<T>>`, `E<T>` with a
payload `Option<T>` or `Vec<Option<T>>`, `Deep<T>` with a field `find:
fn(T) -> Option<T>`, `P<T>` with a payload of that type, and a class
holding any of them are all nested open enums. Their methods (`self` is
one), every generic over them and every dictionary for them at a niche
type are clone-only, past the budget too (`nested_head` in
`plan_instance_dict_steps`), and a call that still reaches a shared body
is refused as above. Since a shared body then sees only instances made
where the parameters are open, a function such a type holds is a closure
word there that returns the open enum's object: a field's
(`query::field_fun`, `binds_from_nested`) and a payload's
(`classify::generic_enum_field`).

The walk reads every user class and enum, another module's too
(`nested_open_look`, #2547). Another module's clone of a generic whose
signature has the shape is compiled from that module's body, where its
body would otherwise make it a thunk to the shared one (`layout_safe`),
so ground code here that constructs such a class or fills a `pub`
field of it holds the layout its methods read. A type that module does
not know (declared after it, a tuple: `knows_tys`) has no clone there:
all the class's methods at it are its shared bodies, which agree on the
enum objects. The walk skips such an instance where it decides whether a
call or a dictionary may take a shared body (`shared_only_class`), and
ground code that constructs one, or reads or writes a field holding the
nested enum, is refused (E0801 `field-shared-layout`,
`hir_check_shared_field`). In coil-stdlib only
`collections::deque::VecDeque` (`buf: Vec<Option<T>>`, private) has the
shape; in the prelude, `task::Channel`.

**Functions in fields and payloads** (#2511). A function over a type
parameter in a user class's field or enum payload, at any depth of the
slot (`Slot<T>` with `f: fn(T) -> T`, `G<T>` with `F(fn(T) -> T)`,
`fs: Vec<fn() -> T>`, `(fn(T) -> (T, T), int)`, #2560) crosses a shared body inside
the object too: a ground function takes a `float` or `bool` as its own
MIR type and a two-word value as two words, while one the shared body
made (`fn (T x) => x`) or adapted (a function argument) takes their
words. So a signature that holds such a type (`layout::nested_fn_field`,
the same walk over the declarations) takes a clone for every ground
call, past the budget, through the same two rules (`GenericFnSig::split`,
`fn_sig_split`).

**Plain words** (#2493, #2571). Which function types over a type
parameter are one closure word that a shared body and ground code call
alike is one predicate, `classify::fun_words_in`: each parameter and
the result is `()` or a plain word (`plain_word`), which is

- a closed one-word type or closed enum, a bare `T`, a generic class's
  instance over type parameters (`Cell<T>`, `Vec<T>`) or a fixed array
  over them (`[T; 3]`, the same heap array, #2615), or a function;
- a tuple of plain words (`fn(T) -> (T, T)`): the tuple object, whose
  items are each one word, never a pair at a word position;
- an `Option` / `Result` of such functions and words (`Option<fn(T) ->
  T>`): a function is no niche payload and never closed, so the value
  is the boxed object in every instance.

A local, a binding (`let`, a pattern, a `match` or `if` of closure
words, #2565), a field, a payload and an item of a `Vec`, array, tuple,
`Option` or `Result` (`classify::fun_holder`) of such a function is
then one word everywhere, and matching or binding it needs no refusal.
What the predicate leaves out is what differs between the two sides:
an open `Option<T>` in the function's signature (a niche word at a
ground type) is a plain word only where every closure was made with the
enum open (`open_enum`: a closure literal's type, a function argument,
a value read from a nested open enum).

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
same object, so every alias sees the change, and it never turns back. A
lane array a ground function returns to a shared body that calls it as a
function value (`fn() -> Vec<T>`) is converted the same way by the
argument's adapter (`HirFnRet::Lanes`); a function below a signature's top
(`Vec<fn() -> Vec<T>>`) gets no adapter, so such a signature is
clone-only, as for a `()` result (`layout::nested_fn_var_result`, #2592).
A ground instance's method that a dictionary calls is converted by its
`$dict` adapter where the trait's result is a `Vec` or array of its
parameters or associated types (`fn many(S s) -> Vec<S>` at `Option<int>`,
`Compiler::trait_method_lanes_ret`, #2616): the adapter returns the trait's
open type, so its return converts the array.
The lane ops in turn accept an array of boxed elements (one a generic body
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
