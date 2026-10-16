// A derived `Default` class whose field is a derived-`Default` enum with a
// multi-field first case (#1167), and `new` of such a field by hand.
use tests::assert;

#[derive(Default)]
enum Mode {
    First(int, int),
    Second,
}

#[derive(Default)]
class Outer {
    pub mode: Mode,
    pub n: int,
}

class Holder {
    pub mode: Mode,
}

fn first_sum(Mode m) -> int {
    return match m {
        Mode::First(a, b) => a + b + 1,
        Mode::Second => -1,
    };
}

test("nested derived defaults") {
    let o = Outer::default();
    assert!(first_sum(o.mode) == 1);
    assert!(o.n == 0);
}

test("a multi-field enum value stored in a class field") {
    let h = new Holder(Mode::First(2, 3));
    assert!(first_sum(h.mode) == 6);
    let s = new Holder(Mode::Second);
    assert!(first_sum(s.mode) == -1);
}
