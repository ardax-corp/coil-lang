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
argument, with `inner` set for every argument.

## The callee

A `return` of a parameter, or of a parameter's field or element, does
not share: `Plan::store_result` records the value as stored and leaves
the share to the caller. Coroutines keep the old rule (they share what
they hand back), since a resumed frame outlives the call.

A generic body whose result is a bare type parameter (`T`, or `F<A>` over
a type parameter `F`) leaves more to the caller (#2502). Its shared form
holds a `T` as an opaque word, which may be a scalar. A share of one is a
`Cow` on the `I64` word (`mir/from_hir.rs`, `Builtin::Cow`; #2506): the
runtime's `cow_share` leaves a non-object alone, so a `T` the body stores
(into a field, a `Vec`, an `Option`, tuple or `Result` it returns) is
shared. A `unique` of one must be exact, since it replaces the word: the
body reads it from its `kinds` word (#2506). A shared generic free function
or inherent method takes that word after its dictionaries, bit `i` set when
its scheme's type parameter `i` (`Scheme::bounds`, in order) is a type
`cow_ty` holds copy-on-write at the call; the bits past the mask carry a
sentinel the entry checks in debug builds and under `COIL_MIR_VERIFY`
(`codegen/kinds.rs`). A ground call passes a constant; a shared caller
forwards its own bit for a type parameter and a constant for a type with a
known head (`Vec<T>` is 1, `Option<T>` 0); a lambda captures the word after
the dictionaries; a partial application fills it as its last parameter; a
`PolyFn` value captures it with every bit clear, and a class's `drop`, which
the runtime calls, takes none. `Lower` emits the unique under a test of the
bit (`guarded_cow`). Trait methods (instance, default, own `<W>`) take no
word yet: their unique stays out.
A result it would have
shared itself, one a call handed back from inside a parameter
(`return c.get()`), comes back unshared. So when the result is not fresh
(`shared_ret`), `returns_inner` names every parameter the result may reach
(`cow::holders_of_result`), and the caller, which knows the type, shares
the result by the rule below. A parameter that is a bare type parameter
itself is left out: the body can only hand it back whole (`returns_arg`) or
pass it to code not known here (`opaque_args`), which the caller already
answers. A per-type clone still shares the result too: the second share
sets a flag that is already set.

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
`tests/memory/cow_returned_param_names.coil` and
`tests/memory/cow_returned_field.coil` (soundness). Benches:
`examples/perf/cow_call_reassign.coil`, `cow_call_field.coil`,
`cow_entry_update.coil`.
