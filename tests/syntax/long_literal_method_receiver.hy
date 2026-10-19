// A method called on a string or number literal whose line runs past the
// width limit. `coil fmt` may break the line before the `.method()` after a
// literal or a field: the parser takes a line break before `.` after any
// receiver (#1505, #1540).

use tests::assert;

class Holder {
    pub label_with_a_rather_long_name: string,
}

test("a long string literal as a method receiver") {
    let t = "a";
    let b = t.len() == "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
        .len();
    assert!(!b);
    assert!(
        t.len() + 89 == "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
            .len(),
    );
}

test("a number literal starts a chain that does not fit") {
    let s = 1234567
        .show()
        .len()
        .show()
        .len()
        .show()
        .len()
        .show()
        .len()
        .show()
        .len()
        .show()
        .len()
        .show();
    assert!(s == "1");
    let f = 12345.5
        .show()
        .len()
        .show()
        .len()
        .show()
        .len()
        .show()
        .len()
        .show()
        .len()
        .show()
        .len()
        .show();
    assert!(f == "1");
}

test("a field before a method in a chain that does not fit") {
    let holder_with_a_long_name = new Holder("abc");
    let n = holder_with_a_long_name.label_with_a_rather_long_name.len().show().len().show().len();
    assert!(n == 1);
    let m = holder_with_a_long_name.label_with_a_rather_long_name.len() +
            holder_with_a_long_name.label_with_a_rather_long_name.len();
    assert!(m == 6);
}
