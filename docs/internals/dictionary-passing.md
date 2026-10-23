# Dictionary passing

The calling convention for trait-bounded generic code that is not monomorphized. Moved here from the website's types reference (#1880), which keeps a user-level summary.

Constrained calls that are not monomorphized append one dictionary per trait constraint after the value arguments. Each dictionary is a `MakeTuple` of method code offsets in declaration order. The callee reserves trailing locals `__dict0`, `__dict1`, …, loads the matching method slot with `Index`, and invokes it with `CallIndirect`. A generic calling another generic with the same open bound forwards its existing dictionary. Builtin classes use compiler-generated primitive method thunks through this same ABI; ground monomorphization remains an optimization.
