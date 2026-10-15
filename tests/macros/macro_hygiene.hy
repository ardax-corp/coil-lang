// Names a macro binds with `let` or `for` inside its `quote` are its own: they
// neither clobber nor capture the caller's variables of the same name, even
// when the caller passes one of those names as an argument.
use tests::assert;
use macro_probe::{sum_into, bump};

test("a macro's loop variables leave the caller's alone") {
    let acc = 100;
    let x = 7;
    let total = 0;
    sum_into!(total, [1, 2, 3]);
    assert!(total == 6);
    assert!(acc == 100 && x == 7);
}

test("the caller's variable passed in is the one assigned") {
    let acc = 0;
    sum_into!(acc, [4, 5]);
    assert!(acc == 9);
}

test("an argument naming the macro's own local refers to the caller's") {
    let tmp = 10;
    bump!(tmp);
    assert!(tmp == 11);
    bump!(tmp);
    assert!(tmp == 12);
}

test("expansions in a loop each get fresh locals") {
    let n = 0;
    for _ in 0..5 {
        bump!(n);
    }
    assert!(n == 5);
}

test("the argument may use the caller's loop variable") {
    let total = 0;
    let x = 2;
    sum_into!(total, [x, x * 10]);
    assert!(total == 22);
}
