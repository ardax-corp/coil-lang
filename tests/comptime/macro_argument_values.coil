// A macro reads the value of a constant argument: `n.int()`, `n.float()`,
// `n.bool()`, `n.string()` (escapes decoded); anything else is `None`.
use comptime_values::{repeat, describe_value, half};
use tests::assert;

test("ints and strings") {
    assert!(repeat!("ab", 3) == "ababab");
    assert!(repeat!("x", 2 * 2) == "xxxx");
    assert!(repeat!("q\"", 2) == "q\"q\"");
}

test("each kind") {
    assert!(describe_value!(-7) == "int -7");
    assert!(describe_value!(true) == "bool true");
    assert!(describe_value!("a\nb") == "string a\nb");
    assert!(describe_value!(2.5) == "float 2.5");
    let x = 1;
    assert!(describe_value!(x) == "not a constant");
}

test("floats") {
    assert!(half!(0.3) == 0.15);
    assert!(half!(-1000.0) == -500.0);
    assert!(half!(3) == 1.5);
}
