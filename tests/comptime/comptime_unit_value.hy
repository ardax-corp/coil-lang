// A `comptime` expression or `const` whose value is `()` runs while
// compiling and folds to `()` (#1798).
use tests::assert;

fn noop() {}

fn twice(int x) -> int {
    return x * 2;
}

const NOTHING = noop();

test("a unit comptime call folds to ()") {
    comptime noop();
    let x = comptime noop();
    let y = NOTHING;
    assert!(x == y);
}

test("a unit comptime block folds to ()") {
    let z = comptime {
        let _ = twice(2);
    };
    assert!(z == ());
}
