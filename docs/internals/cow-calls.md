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
| `returns_inner` | the result may be an object held inside the parameter (`return p.f`, `return p.xs[i]`) | `cow::keeps` via `inner_params` |
| `opaque_args` | the parameter reaches code not known here (a trait call on a type parameter, a function value) | `cow::keeps` |

Code that is not known (`ArgUse` `unknown`: a function value, a trait
call through a dictionary) counts as returning and writing every
argument, with `inner` set for every argument.

## The callee

A `return` of a parameter, or of a parameter's field or element, does
not share: `Plan::store_result` records the value as stored and leaves
the share to the caller. Coroutines keep the old rule (they share what
they hand back), since a resumed frame outlives the call.

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
`tests/memory/cow_returned_param_names.coil` and
`tests/memory/cow_returned_field.coil` (soundness). Benches:
`examples/perf/cow_call_reassign.coil`, `cow_call_field.coil`,
`cow_entry_update.coil`.
