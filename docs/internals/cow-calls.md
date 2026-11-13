# Copy-on-write across calls

How `hir/cow.rs` treats arguments and results at a call (#2206). The
copy-on-write model itself (a sticky shared flag set by a share op, a
copy at the first write to a shared object) is described in the user
docs under `language/memory`. View classes are covered in
[view-classes.md](view-classes.md).

## Summary bits

`hir/effects.rs` `Summary` carries, per parameter `i`:

| bit | meaning | set by |
|-----|---------|--------|
| `mutates` | the callee may write through the parameter (inout: the caller's place sees the write) | effects walk |
| `keeps` | the parameter's object is stored past the call (field, element, capture, `yield`) | `cow::keeps` |
| `returns_arg` | the result may be the parameter's object itself | `cow::keeps` (`return p`, through `let x = p` and calls that return their argument) |
| `returns_inner` | the result may be an object held inside the parameter (`return p.f`, `return p.xs[i]`) | `cow::keeps` via `inner_params`; for a result of a bare type parameter, also every parameter the result may reach (`returns`, `escapes`) that is not a bare type parameter itself |
| `opaque_args` | the parameter reaches code not known here (a trait call on a type parameter, a function value) | `cow::keeps` |

Code that is not known (`ArgUse` `unknown`: a function value, a trait
call through a dictionary) counts as returning and writing every
argument, with `inner` set for every argument. The rewrite reads a
call's summary as the solver does (`ProgramEffects::call_summary`): a
function by its def, a static method (`H::wrap(s, i)`), whose def names
no function summary, by its body's name (#2222). Before #2222 such a call
was unknown, so its argument was made unique before every call and its
result shared, a copy per call once the argument was shared.

## The callee

A `return` of a parameter, or of a parameter's field or element, does
not share: `Plan::store_result` records the value as stored and leaves
the share to the caller. Coroutines keep the old rule (they share what
they hand back), since a resumed frame outlives the call. For the same
reason a coroutine's parameter is never the caller's place (#2662): its
writes run on `resume`, after the caller has its value back, so the body
makes the parameter unique where it writes it, as a local.

### Shared generic bodies: the `kinds` word

A shared generic body holds a value of a type parameter `T` (or `F<A>` over
a type parameter `F`) as the ground type's word, which may be a scalar or an
object, so it cannot tell on its own whether a share or a unique applies, and
a unique replaces the word. It reads that from its `kinds` word (#2506,
`codegen/kinds.rs`): bit `i` set when type parameter `i` is a type `cow_ty`
holds copy-on-write at the call (a class instance, a `Vec`, an array; not a
scalar, `string`, `Option`, tuple, enum or function). `Lower` emits both ops
on such a word under a test of the bit (`guarded_cow`), so a body pays a
test and a branch, and the op only where the type is an object, as its
per-type clone does. A body with no bit for the variable (a cleanup pad, an
outlined helper) emits no op. Native code sets a share's flag inline, with no
call ([native.md](native.md), "Inline share"), so a body whose type is a scalar
pays the test alone. The bits past the mask carry a sentinel the
entry checks in debug builds and under `COIL_MIR_VERIFY`, so an edge that
does not pass the word fails loudly.

Where the word comes from:

- a shared generic free function or inherent method takes it after its
  dictionaries, its scheme's type parameters (`Scheme::bounds`) in order;
  a class's `drop`, which the runtime calls, takes none;
- a trait method reads it from its dictionary, which carries a `kinds`
  word after its methods (the trait's parameters, then the instance head's
  type variables; [dictionary-passing.md](dictionary-passing.md)): a
  default body and a generic-head instance's method read it at entry. A
  trait method with type parameters of its own (`fn visit<W>`) takes their
  word after its own bounds' dictionaries at every entry of it: the shared
  one, a per-type clone, the `$dict` adapter, a forwarder, a clone's thunk;
- a lambda captures its body's word after the dictionaries;
- a `PolyFn` value captures it after its dictionaries: the word at the type
  it is read at. A type parameter left open there (the value is an argument
  a `forall` parameter takes, or a local bound from the function) gets its
  bit from each call (#2621). A call through a `forall` parameter or such a
  local passes, last, one bit per type variable of the type it sees (the
  `forall`'s, or the function's own), in order of first appearance in that
  type, and their count at bit 56 (`common::KINDS_CALL_COUNT_SHIFT`); bit 63
  is clear, so it is no entry's word. A call made at the calling body's own type parameter forwards that body's bit for it (#2652). Where the value is made, the checker
  has recorded the `forall` type it is passed to (`rank_n_arg_at`), and
  each open type parameter captures a `common::kinds_from_call(n, to, from)`
  word after the `kinds` word: `machine::apply` (both engines; native code
  calls a `PolyFn` through it) sets bit `to` when the call's word counts `n`
  variables and has bit `from`, and passes no word for it. A type and one
  that renames it into another declared order list their variables in the
  same order, so a value passed on to a `forall` of another order keeps
  its bits. A local bound from a generic function and read as a
  value (passed to a `forall` parameter, returned, stored, captured, sent)
  is the function's value made at that read, as its name would be (#2637,
  #2658). A `forall` parameter passed on to a
  `forall` that is not its own type renamed (a less general one) is refused
  (E0801 `forall-relay`): the value cannot read that type's order (#2637). A
  function value read at a ground type captures no such word.

A ground call passes a constant; a shared caller forwards its own bit for a
type parameter and a constant for a type with a known head (`Vec<T>` is 1,
`Option<T>` 0); a partial application fills it as its last parameter.

Since the body shares exactly what a clone shares, a result of a bare type
parameter it hands back from a call (`return c.get()`) is shared there as in
a clone (#2502). Compiler-made glue (`==`, `Show`, `hash`) plans no op on
an open word and takes no word.

## The caller

For each argument `a` of a call `c` (`Plan` `HirKind::Call`):

- `kept`: share `a` (it now has a second name).
- `returned`, `c`'s result is a copy-on-write value, and `a` is not moved
  into the call:
  - opaque code: share the result after the call (lazy; the writes the
    callee made are done),
  - a known callee that writes `a`: an eager copy of the result (one copy,
    decided 2026-11-02),
  - otherwise: share `a`.
- `inner` on an argument that may hold objects (not a scalar, `string` or
  function) and is not moved in: share the result, unless the caller only
  reads it.

`moved_into_call` decides "moved": a temporary argument (a call result, a
new object, a literal), the last read of a local that owns its object, or
a place the result is assigned straight back to (`l = f(l)`,
`self.f = g(self.f)`, `a[i] = h(a[i])`). For a place, the root and every
index must be plain locals (not borrowed or captured), indices must be
literals or the same place on both sides, and nothing else in the call
may read the root or an index.

`read_only_result` is a result dropped as a statement or by `let _`, or
an operator operand.

## Consequences

- `v = f(v)` and `self.items = f(self.items)` copy nothing.
- An entry-style chain, `m.entry(k).and_modify(f).or_insert(init)` with
  the result dropped, copies nothing.
- A returned parameter may be the same object as the argument (values
  only; nothing observes identity).
- A generic trait call whose result the caller then writes copies once
  per call (the result is shared because the dictionary call is opaque).

## `use (mut x)` captures

A `use (mut x)` capture of a copy-on-write value is a borrow of `x`'s
object for as long as the closure lives (#2787), under the E0149 rule of
`let w = mut x` (`Plan::check_borrows`): while it lives, `x` gets no second
name (a `let`, a store, an argument a callee keeps) and is not replaced.
The closure's life (`closure_life`) runs from the lambda to the last read of
any local holding it (`let f = …`, copies of `f`, closures capturing `f`, to
the end of the body for a `defer` capturing it) and through any call that
takes it without keeping it. A closure that is returned, stored (a field,
an element, a value built around it) or passed to a call that may keep it
(`kept` or `opaque`: a spawned task, a partial application) holds the
borrow past the body: the capture is E0149 itself. Before, a share during
the closure's life went unreported, and the next write through `x` copied
its object while the closure kept the old one. State a kept closure shares
belongs in a `Ref<T>` or a `shared class`. Tests:
`tests/memory/capture_borrow_in_step.coil`,
`tests/memory/compile_fail/capture_borrow_*.coil`.

## Call results that may be a borrowed object

While a borrow lives, a call's result that may be the borrowed object, or an
object holding it, is a second name like a `let` of the place (#2818), E0149
where it is bound, stored or passed on (to a call that keeps it, hands it
back or hands it to code not known here). Before, nothing recorded such a
result as a store, so the borrow's in-place writes showed through it. A
result may be the object when the call is given the borrowed place or a
holder of it (a place rooted at the borrow's source whose steps lead to the
borrowed one) and
- the call may hand the argument back (`returned`: opaque code, or a known
  callee whose summary has `returns_arg`), or
- its result may be an object inside a holder (`inner`, `returns_inner`), or
- it writes a holder and hands it back: the eager copy of the result still
  holds the borrowed object.

Reading a field or element of the result counts the steps: `g(d).c` may be
`d.c`. A result that cannot be one is no second name: a scalar or `string`,
a fresh result (the summary's `shared_ret` clear), a result only read
(dropped, an operand, a receiver a call only reads), an object inside the
borrowed one (the borrow makes it unique before writing it), and the eager
copy of the borrowed object itself. Code not known here given the borrow
local itself (`g(w)`), or an `inout` parameter, that is written after the
call (later in the body, or in a loop around it) hands back a copy (an eager
copy of the result, as `let r = w` copies), since a write through either is
never tested; a result only read is not copied. Tests: `tests/memory/borrow_call_results.coil`,
`tests/memory/compile_fail/borrow_{opaque_result,inner_result,inner_result_stored,holder_returned_written}.coil`.

## Writes through a borrowed source

The shared flag is sticky: nothing clears it when the second name dies. A
callee that only walks its parameter (`for p in m` over a map: `iter()`
stores `m` in the iterator, which shares it) keeps nothing past the call,
so its summary's `keeps` bit is clear and E0149 lets a borrowed object be
passed to it, but the object comes back flagged. A write through the
borrow writes in place (the borrow is in `inout`), while a write through
the source (`a` after `let w = mut a`, or the outer `a` of a `use (mut a)`
closure) made it unique, copied it and left the borrow on the old object
(#2816).

`Plan::hold_borrowed` treats the source as the borrow does: a write whose
path runs through a borrowed place while the borrow lives (the span E0149
checks) leaves the borrowed object, and each object holding it on the way
from the root, without a unique; objects below the borrowed one are made
unique as before, and the copy is written into the object both names
share. This is sound for the reason a borrow's own in-place writes are:
the borrow made those objects unique where it began, and while it lives
E0149 refuses every second name for them (a `let`, a store, an argument a
callee keeps or hands to code not known here, a capture), so a flag there
can only come from a holder that died with the call that made it. The
same reasoning already leaves a fresh local that nothing keeps without a
unique (`fresh_locals`). Writes keep their unique where a holder E0149
does not see may exist: inside a `for` over an iterator (not an array,
tuple or range), whose iterator holds its iterable while the body runs;
and for every borrow of a root when, during one of them, the borrowed
object or one holding it is handed to code not known here (`opaque`), a
holder of it is passed to a call whose result may be an object inside it
(`inner`), or a `use (mut a)` closure's body keeps, returns or hands its
capture to such code (`Facts::loose_captures`). An iterator or generator that outlives the
call (`let it = a.iter()`, `let g = values(a)` for a `gen fn`) is kept,
so it stays E0149 while the borrow lives. A flag left by a walk still
costs one copy at the first write after the borrow is done, unless the
source is a fresh local (below, "After a walk"). Tests:
`tests/memory/borrow_iterating_callee.coil`,
`tests/memory/compile_fail/borrow_iterator_kept.coil`,
`tests/memory/compile_fail/borrow_generator_walk.coil`.

## Writing what a `for` walks

A `for` over an iterator (`ForInKind::Custom`: `into_iter`, then `next`)
walks the value its iterable had when the loop began: the iterator holds
the collection (`into_iter` stores it, which shares it), so a write to it in
the body copies it and the loop goes on over the old object (#2819). A
write in such a loop whose root is the iterable's root keeps its unique on
a fresh local too (`Plan::walked`; `fresh_locals` would skip it), or the
write would go into the object the loop walks: a user iterator over a
growing `Vec` never ended.

While a borrow lives, the loop is a second holder of what it walks. A
`for` over an iterator whose iterable is the borrowed place, one holding
it, or the borrow itself (`for p in w`), whose body writes what the loop
walks (a path from the source that leads to the walked object or below it,
any write through the borrow, or a call of a `use (mut a)` closure that
borrows it), is E0149 at the iterable: a write through the source would
copy the object away from the borrow, and one through the borrow would
change what the loop walks (before, `for p in a { w.insert(…) }` never
ended). It is refused rather than given a meaning because the iterator's
old object and the borrow's object cannot both be the one value. A walk of
an object inside the borrowed one (`for p in a.m` while `w = mut a`) is a
second name below the borrow, which writes make unique first, as for any
other second name. A walk whose body writes nothing it walks, or one after
the borrow's last use, is fine. Tests:
`tests/memory/for_walk_writes_iterable.coil`,
`tests/memory/compile_fail/borrow_for_*.coil`,
`tests/memory/compile_fail/capture_borrow_for_written.coil`. Not covered: a
walked `inout` parameter (#2826) and a `for` over a `Vec` (#2827).

## After a walk

A walk shares what it walks (`into_iter` stores it in the iterator) and the
flag stays set when the iterator dies with the loop, so the next write
copied: walking and growing a map was quadratic (#2821). Where the plan
knows every name of the object, the write needs no test. That is
`fresh_locals`: a local only ever given fresh objects (a constructor, a
call whose summary has `shared_ret` clear), never shared, captured or bound
from a place, and passed only to calls that neither keep it, hand it back
nor hand it to code not known here. Its flag after a walk is one a dead
holder left, in this body or in a callee that walked it and returned.

That is sound only if a walk keeps nothing past the loop but its iterator.
A `for` is no `Call` in HIR, so `walk_keeps` reads the loop's `into_iter`
and `next` (the instance bodies its desugar names, `ForInKind::Custom`, by
their key: `module.instance_fns` for this module, the recorded
`ProgramEffects` instance keys for earlier ones): a walk keeps the
iterable when either may keep what it is given other than in its result
(`escapes`), hands it to code not known here, is not known, or when `next`
may hand back an object inside the iterator whose type is the iterable's
(the walked object itself). Such a loop counts as a call that keeps its
iterable: `cow::keeps` sets the parameter's `keeps` bit, and the caller's
`Plan` stores the iterable (shared, a second name under E0149, no fresh
local). Before, a `for` was never a keep, so an `into_iter` that stored its
collection elsewhere saw the caller's next write in place.

`HashMap::new` / `with_capacity` are fresh calls since `key_index::table`
(registry `key_index_table`) is in `escape::FRESH_HOSTS`: a host export is
classified by its surface name (`table`), so a fresh host under another
surface name is looked up by its registry name (`escape::fresh_registry`).

Not covered (the flag is the only record, so a write after the walk still
copies once): a walked object that is not a fresh local (a field such as
`r.m`, a local copied from another name or bound from a call that may
share its result, a captured local), and a collection whose iterator holds
an object inside it (`new It(b.items)`) instead of the collection: the flag
is then on that inner object. A run-time restore for those is designed on
#2821. Tests: `tests/memory/cow_walk_holder_died.coil` (values for every
escape: a second name, a keeping callee, a body that stores, passes or
captures the collection or keeps an element, an iterator that keeps what
it walks elsewhere, `break` / `return`, a generator suspended mid-walk, a
task that panics mid-walk with a `defer`). Benches:
`examples/perf/cow_walk_grow_map.coil`, `cow_walk_grow_vec.coil` (the
control: a `Vec` walk is an index loop and shares nothing).

## Loop views

A write through a nested path, `f.blocks[b].insts[i] = v`, makes each
object on the path unique before it runs. In a loop that is a `unique` per
level on every iteration, and the calls keep MIR from hoisting the path's
loads (#2351: 17x the loop through a `let xs = mut f.blocks[b].insts`
view). `hir/cow_view.rs` binds such a path once before the loop, as that
`let … = mut` would, when nothing in the loop can change which objects the
path names or give one of them a second name: the root and indexes are
locals the loop does not assign and nothing captures; no write in the loop
changes an object above the path's last one (a store into another field
is fine) or goes through another name that may reach it; nothing in the
loop shares an object on the path, and calls only read the objects above
the last one; no `yield`, `resume`, closure or `defer`; a root the body
does not own (a parameter, which may be a `static` in the caller) meets
only calls that touch no `static` and switch no task. Evaluating the path
early must not fail where the loop would not: its last index step is read
first on entry (the `while` test, the `for` iterable), after nothing but
locals and literals.

A view makes no copy the loop would not: a write through the path must run
on every iteration that completes (not under an `if`, a `match` arm, the
right of `&&` / `||`, `?` or a nested loop, nor after a `break`,
`continue` or `return` that may run first), so the first iteration would
have made the path unique there; and the view sits behind a copy of the
loop's first test, `if test { let v = mut path; while test { … } }` (for a
range `for`, `start < end`), so a loop that runs no iteration makes
nothing unique. Only a `while` loop whose test, or a range `for` whose
bounds, read nothing but locals, literals, fields, elements, operators and
`len()` qualifies, since the test runs once more. A write in a nested loop
does not count for the outer loop (the inner one may run no iteration):
the inner loop makes its own view from the original root.

A round views the outermost loops; the next round sees the inner loops
with the views as roots, so the issue's pass, which also pushes to
`blocks[b].insts` on each outer iteration, makes `f.blocks` unique once
per call and `blocks[b].insts` once per block. The rewrite runs again on
the viewed body and keeps a view only if that draws no new diagnostic
(E0149 checks it as a written borrow) and shares no view. Tests:
`tests/memory/cow_loop_path_writes.coil` (values),
`hir/cow_view.tests.rs` (which loops get one), `tests/cow_call_share_cli.rs`
`a_loop_whose_write_does_not_run_copies_what_one_with_no_write_does`.
Bench: `examples/perf/cow_nested_path_loop.coil`.

Tests: `tests/cow_call_share_cli.rs` (copy counts),
`tests/memory/cow_generic_bodies_shared.coil` (a type parameter result of
a shared body), `tests/memory/cow_generic_bodies_unique.coil` (a write
through a copy of a type parameter's value, the `kinds` word),
`tests/memory/cow_generic_trait_methods.coil` (the same in trait methods),
`tests/memory/cow_generic_forall_values.coil` (through a `forall` value),
`tests/memory/cow_generic_forall_order.coil` (the same over several type
parameters in any order),
`tests/memory/cow_generic_fn_value_in_clone.coil` (a value made in a
per-type clone or a shared body at its own type parameter),
`tests/memory/cow_returned_param_names.coil` and
`tests/memory/cow_returned_field.coil` (soundness). Benches:
`examples/perf/cow_call_reassign.coil`, `cow_call_field.coil`,
`cow_entry_update.coil`.
