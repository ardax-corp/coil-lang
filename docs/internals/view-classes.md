# View classes

A `view class` holds borrows: each `mut` field borrows the place it was
given, so writes through the view land in that place's object instead of a
copy-on-write copy. This is how a class can hold a mutable reference under
copy-on-write without a runtime borrow count: the view is second-class, and
the compiler proves the borrow is still good wherever the view is used.

```coil
view class Slot {
    mut c: Counts,
    i: int,
}

impl Counts {
    pub fn slot(int i) -> Slot {
        return new Slot(mut self, i);
    }
}

impl Slot {
    pub fn add(int d) -> Slot {
        self.c.vals[self.i] += d;
        return self;
    }
}

let _ = c.slot(3).add(1).add(2); // writes c.vals[3] twice
```

## Rules

| Code | Rule | Where |
|------|------|-------|
| E0150 `MisplacedMut` | `mut` marks only a `let` init, a `for` iterable or a view constructor's `mut` field argument; a `mut` field only in a `view class`; a `mut` field takes `mut place` and a plain field never does; no borrow of a scalar (the runtime has no interior pointers) | `hir/build.rs` (`stray_muts`), `typechecking` (`check_borrow_arg`) |
| E0151 `ViewEscape` | A view is never stored (field, element, static, new object or variant), held inside another type (`Option<View>`, `Vec<View>`), captured by a lambda, or returned except by a method of the object it borrows: a view built over `mut self` (or below it), `self` in the view's own method, or another such call on `self`; a view argument a callee keeps | `hir/view_lint.rs`, `hir/cow.rs` (`check_kept_views`) |
| E0149 `BorrowConflict` | While a borrow lives (a `mut` local, a view local, `for mut`, or a view method's `mut` fields for the whole body), the borrowed place is not shared, moved, captured or replaced; a place below it may be shared | `hir/cow.rs` (`check_borrows`) |

The return rule is what makes method chains safe: a call that returns a
view borrows its receiver, so `c.slot(i)` is treated as a write of `c`
(made unique before the call, never kept by it).

## Copy-on-write

- `cow_ty` excludes view classes: a view is never shared or copied.
- A view constructor's `mut` argument is a write at the `new`
  (`Facts::note` pushes `Write { at: make, path }`), so the source is made
  unique once, when the view is built. It is not a store, so it does not
  share the source.
- Effects (`hir/effects.rs`) count the constructor as mutating its `mut`
  arguments' roots.
- No unique check on a view's `mut` field (`cow.rs`, `borrow_field`): the
  object was made unique at the `new`, and E0149 keeps it unshared while the
  view lives.

## Cost

A spliced view costs nothing. After `slot` and `add` inline, splicing
leaves copies of the view behind: a discarded result (`let _ = ..add(1)`)
and the next chained call's receiver. `hir_spliced_copies`
(`codegen/emit_hir.rs`) drops unread copies, forwards a view copy to its
source, and dissolves a view whose fields are only read into its
constructor arguments, so `v.c.vals[i] = ..` becomes `c.vals[i] = ..`.

`examples/perf/view_slot_hot.coil` (3M iterations of a two-call chain,
`--release --fold-calls off`): 1056 ms before the splice tidy, 292 ms
after, the same as writing `c.vals[i] += ..` directly. Dissect of `main`
shows no `NEW` and no call in the loop.

A view that does not inline (a method too large to splice, or a call
across a module the inliner refuses) is an ordinary two-field object
passed by reference: one allocation per view.

## Generic callees

A generic function sees a view as a bare `T`, so E0151 cannot check its
body. Instead the caller refuses a view argument for a parameter the
callee keeps (its copy-on-write `keeps` summary: stored, captured by a
closure it returns, returned, or handed to a callee that keeps it), except
the receiver of a call that returns a view (#2198).
