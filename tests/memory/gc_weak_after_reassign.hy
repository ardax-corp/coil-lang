// Reassigning the only strong reference to a heap value drops it: after a
// `collect`, a weak to the old value no longer upgrades, at every
// optimization level, also when the function is inlined (#1402).
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
