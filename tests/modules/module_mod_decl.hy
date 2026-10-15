// `mod m;` loads `m.hy` without binding its names; its items are then
// reachable by qualified path.
use tests::assert;

mod math;

fn add(int a, int b) -> int {
    return a - b;
}

test("the local name is this file's own") {
    assert!(add(5, 3) == 2);
}

test("the loaded module's item by qualified path") {
    assert!(math::add(5, 3) == 8);
    assert!(math::mul(5, 3) == 15);
}
