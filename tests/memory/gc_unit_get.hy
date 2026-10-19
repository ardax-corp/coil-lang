// `gc::get` of a `Root<unit>` and `gc::upgrade` of a `Weak<unit>` return
// an `Option<unit>` (#1449).
use tests::assert;
use gc::{root, weak, get, upgrade, unroot};

test("get of a unit root is Some") {
    let r = root(());
    assert!(match get(r) {
        Option::Some(u) => u == (),
        Option::None => false,
    });
    unroot(r);
}

// `()` is one shared object that is never collected.
test("upgrade of a unit weak is Some") {
    let w = weak(());
    let u = upgrade(w);
    assert!(match u {
        Option::Some(_) => true,
        Option::None => false,
    });
}
