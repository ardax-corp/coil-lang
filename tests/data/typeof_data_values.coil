// `typeof` names the static type of a value as a string at compile time:
// classes, enums, tuples, arrays, dicts, Vec and Option.
use tests::assert;

class Point {
    pub x: int,
}

enum Color {
    Red,
    Blue,
}

fn calls() -> int {
    return 1;
}

test("nominal types use their bare names in the entry file") {
    assert!((typeof new Point(1)) == "Point");
    assert!((typeof Color::Red) == "Color", typeof Color::Red);
}

test("structural types") {
    assert!((typeof (1, "a")) == "(int, string)");
    assert!((typeof [1, 2, 3]) == "[int; 3]");
    let d = { b: 1, a: "x" };
    assert!((typeof d) == "{ a: string, b: int }", typeof d);
}

test("prelude types are qualified") {
    assert!((typeof Option::Some(1)) == "prelude::Option<int>");
    let v: Vec<string> = Vec::new();
    assert!((typeof v) == "prelude::Vec<string>", typeof v);
}

test("the operand is not evaluated") {
    let n = 0;
    let t = typeof n++;
    assert!(n == 0);
    assert!(t == "int");
    assert!((typeof calls()) == "int");
}
