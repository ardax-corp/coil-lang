// Lowering rewrites that must keep their values: `if !c { A } else { B }`
// returned through a `bool` parameter, field and index assignment
// statements, and `for … in` over an array literal.
use tests::assert;

class Point {
    pub x: int,
    pub y: int,
}

fn pick(bool c) -> int {
    if !c {
        return 10;
    } else {
        return 20;
    }
}

test("a negated condition keeps its arms") {
    assert!(pick(false) == 10);
    assert!(pick(true) == 20);
}

test("field and index assignment statements") {
    let p = new Point(0, 0);
    p.x = 3;
    p.y = 4;
    let a = [0, 0];
    a[0] = 10;
    a[1] = 20;
    assert!(p.x + p.y == 7);
    assert!(a[0] + a[1] == 30);
}

test("for in over an array") {
    let a = [1, 2, 3];
    let out = "";
    for x in a {
        out = out + x.show();
    }
    assert!(out == "123", out);
}
