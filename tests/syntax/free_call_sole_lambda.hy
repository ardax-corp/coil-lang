// A free-function call whose only argument is a block lambda that spans
// several lines keeps `with_total(fn (int x) {` on one line; `coil fmt` used
// to put `with_total(` and `fn (int x) {` on two lines (#1796).

use tests::assert;

fn with_total(int -> int f) -> int {
    return f(1);
}

test("a free call keeps its sole lambda argument on its line") {
    let r = with_total(fn (int x) {
        let first_value_with_a_long_name = 41 + x;
        let second_value_with_a_long_name = first_value_with_a_long_name * 2;
        second_value_with_a_long_name - first_value_with_a_long_name
    });
    assert!(r == 42);
}

test("a nested free call hugs its lambda too") {
    let r = with_total(fn (int x) {
        let inner = with_total(fn (int y) {
            let first_value_with_a_long_name = 40 + y;
            let second_value_with_a_long_name = first_value_with_a_long_name;
            second_value_with_a_long_name
        });
        inner + x
    });
    assert!(r == 42);
}
