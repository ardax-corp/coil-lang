// `!` takes a `bool` or an `int`: on an `int`, zero is false and any other
// value (negative included) is true, so `!0` is `true` and `!42` `false`.
use tests::assert;

fn not_int(int n) -> bool {
    return !n;
}

test("literal ints") {
    assert!(!0);
    assert!(!(!1));
    assert!(!-1 == false);
}

test("runtime ints") {
    assert!(not_int(0));
    assert!(!not_int(1));
    assert!(!not_int(-7));
    assert!(!not_int(9223372036854775807));
    let i = 0;
    let zeros = 0;
    while i < 5 {
        if not_int(i % 2) {
            zeros = zeros + 1;
        }
        i = i + 1;
    }
    assert!(zeros == 3);
}

test("result is a bool") {
    let b = !5;
    assert!((typeof b) == "bool");
    assert!(b == false);
}
