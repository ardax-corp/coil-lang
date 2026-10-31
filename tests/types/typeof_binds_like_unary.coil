// `typeof` binds like a unary prefix (`-`, `!`, `readonly`): it takes the
// operand right after it, so `typeof(x) == "int"` compares the type name
// (#1773). Postfix forms (calls, fields, indexing) stay part of the operand.
use tests::assert;

class Point {
    pub x: int,
}

fn twice(int n) -> int {
    return n * 2;
}

test("comparison takes the type name") {
    let x = 1;
    let s = "a";
    assert!(typeof x == "int");
    assert!(typeof s == "string");
    assert!(typeof s != "bool");
    assert!(!(typeof x == "float"));
    assert!(typeof x == "int" && typeof s == "string");
}

test("binary operators apply to the name") {
    let x = 1;
    assert!(typeof x + "!" == "int!");
    assert!(typeof (x == 1) == "bool");
    assert!(typeof (1.5 + 2.5) == "float");
}

test("postfix forms stay in the operand") {
    let p = new Point(1);
    let v = [1.5, 2.5];
    assert!(typeof p.x == "int");
    assert!(typeof twice(2) == "int");
    assert!(typeof v[0] == "float");
    assert!(typeof "hi".len() == "int");
}

test("prefix operands nest") {
    let x = 1;
    assert!(typeof -x == "int");
    assert!(typeof !true == "bool");
    assert!(typeof typeof x == "string");
}
