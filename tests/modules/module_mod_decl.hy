// `mod m;` loads `m.hy` without binding its names; its items are then
// reachable by qualified path.
use tests::assert;

mod calc;

fn add(int a, int b) -> int {
    return a - b;
}

test("the local name is this file's own") {
    assert!(add(5, 3) == 2);
}

test("the loaded module's item by qualified path") {
    assert!(calc::add(5, 3) == 8);
    assert!(calc::mul(5, 3) == 15);
}
