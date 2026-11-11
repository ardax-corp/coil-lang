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
they hand back), since a resumed frame outlives the call.

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
  it is read at, or, for a function of one type parameter read where it
  stays open (an argument a `forall` parameter takes, a local bound from
  it), `common::KINDS_FROM_CALL`, which the runtime replaces by the call's
  last word; a call through a `forall` parameter or such a local passes it
  last. A function of more type parameters read open captures every bit
  clear (the `forall`'s order need not be its own): its ops stay off (#2621).

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

Tests: `tests/cow_call_share_cli.rs` (copy counts),
`tests/memory/cow_generic_bodies_shared.coil` (a type parameter result of
a shared body), `tests/memory/cow_generic_bodies_unique.coil` (a write
through a copy of a type parameter's value, the `kinds` word),
`tests/memory/cow_generic_trait_methods.coil` (the same in trait methods),
`tests/memory/cow_generic_forall_values.coil` (through a `forall` value),
`tests/memory/cow_returned_param_names.coil` and
`tests/memory/cow_returned_field.coil` (soundness). Benches:
`examples/perf/cow_call_reassign.coil`, `cow_call_field.coil`,
`cow_entry_update.coil`.
