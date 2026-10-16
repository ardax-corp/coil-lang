// Reassigning the only strong reference to a heap value drops it: after a
// `collect`, a weak to the old value no longer upgrades, at every
// optimization level and whether or not the function is inlined (#1402).
use tests::assert;
use gc::{collect, upgrade, weak, Weak};

fn gone<T>(Weak<T> w) -> bool {
    return match upgrade(w) {
        Option::Some(_) => false,
        Option::None => true,
    };
}

// Small enough to inline: the caller's frame must not keep the old `v`.
fn make() -> Weak<Vec<int>> {
    let v = Vec::from([1, 2, 3]);
    let w = weak(v);
    v = Vec::new(); // drop the last strong reference
    return w;
}

test("a weak to a reassigned-away value is cleared by collect") {
    let w = make();
    collect();
    assert!(gone(w));
}

test("reassigning a local in the same frame drops its old value") {
    let v = Vec::from([1, 2, 3]);
    let w = weak(v);
    v = Vec::new();
    collect();
    assert!(gone(w));
    assert!(v.len() == 0);
}

test("a reassigned local that another local still holds stays alive") {
    let v = Vec::from([4, 5]);
    let keep = v;
    let w = weak(v);
    v = Vec::new();
    collect();
    assert!(!gone(w));
    assert!(keep.len() == 2 && v.len() == 0);
}

test("the new value of a reassigned local stays alive") {
    let v = Vec::from([1]);
    v = Vec::from([7, 8, 9]);
    let w = weak(v);
    collect();
    assert!(!gone(w));
    assert!(v.len() == 3);
}
