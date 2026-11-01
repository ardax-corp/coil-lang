// `gc::root` and `gc::weak` take a `()` value as the empty tuple object, as
// `format` and plain calls do (#1428).
use tests::assert;
use gc::{root, weak, Root, Weak};

fn unit_root() -> Root<unit> {
    return root(());
}

test("root and weak take a unit value") {
    let r = unit_root();
    let w: Weak<unit> = weak(());
    assert!(true);
}
