// `unit` has a builtin `Eq` instance, as the other primitives do: `()`
// meets an `Eq` bound, called directly (a monomorphic clone) and through
// a shared generic body's dictionary (a `PolyFn` value) (#1450). `Show`
// and `Hash` bounds take a `()` through a `PolyFn` value too.
use tests::assert;

fn same<T: Eq>(T a, T b) -> bool {
    return a == b;
}

fn differ<T: Eq>(T a, T b) -> bool {
    return a != b;
}

fn counted<T: Eq>(T a, T b) -> int {
    let n = 0;
    if a == b {
        n += 1;
    }
    if a.eq(b) {
        n += 10;
    }
    if a != b {
        n += 100;
    }
    if a.ne(b) {
        n += 1000;
    }
    return n;
}

fn shown<T: Show>(T a) -> string {
    return show(a);
}

fn hashed<T: Hash>(T a) -> int {
    return a.hash();
}

fn unit_fn() {}

test("unit meets an Eq bound in a direct call") {
    assert!(same((), ()));
    assert!(!differ((), ()));
    assert!(same(unit_fn(), ()));
    assert!(counted((), ()) == 11);
}

test("unit meets an Eq bound through a shared body") {
    let s = same;
    let d = differ;
    let c = counted;
    assert!(s((), ()));
    assert!(!d((), ()));
    assert!(c((), unit_fn()) == 11);
    assert!(c(1, 2) == 1100);
}

test("ground eq and ne on unit") {
    assert!(().eq(()));
    assert!(!().ne(()));
    assert!(() == ());
}

test("unit meets Show and Hash bounds through a shared body") {
    let s = shown;
    let h = hashed;
    assert!(s(()) == "()", s(()));
    assert!(shown(()) == "()", shown(()));
    assert!(h(()) == hashed(()));
}
