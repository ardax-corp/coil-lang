// if / else / else-if control flow.
use tests::assert;

test("single branch if true") {
    let x = 0;
    if true {
        x = 1;
    }
    assert!(x == 1);
}

test("single branch if false skips body") {
    let x = 0;
    if false {
        x = 1;
    }
    assert!(x == 0);
}

test("if else both arms") {
    let a = 0;
    if true {
        a = 1;
    } else {
        a = 2;
    }
    assert!(a == 1);

    let b = 0;
    if false {
        b = 1;
    } else {
        b = 2;
    }
    assert!(b == 2);
}

test("else if chain") {
    let n = 2;
    let r = 0;
    if n == 0 {
        r = 10;
    } else if n == 1 {
        r = 20;
    } else if n == 2 {
        r = 30;
    } else {
        r = 40;
    }
    assert!(r == 30);
}

test("nested if") {
    let x = 5;
    let y = 0;
    if x > 0 {
        if x < 10 {
            y = 1;
        } else {
            y = 2;
        }
    }
    assert!(y == 1);
}

test("if expression via assignment") {
    let flag = true;
    let v = 0;
    if flag {
        v = 42;
    } else {
        v = -1;
    }
    assert!(v == 42);
}
