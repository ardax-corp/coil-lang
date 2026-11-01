// `typeof e` is the compile-time name of `e`'s type: primitives by name,
// tuples, fixed arrays, records with fields in name order, and
// prelude types with their module (`prelude::Option<int>`,
// `prelude::Vec<float>`).
use tests::assert;

class Point {
    pub x: int,
}

test("primitives") {
    assert!((typeof 1) == "int");
    assert!((typeof 1.5) == "float");
    assert!((typeof true) == "bool");
    assert!((typeof "s") == "string");
    let b: byte = 1;
    assert!((typeof b) == "byte");
}

test("aggregates") {
    assert!((typeof (1, "a")) == "(int, string)");
    assert!((typeof [1, 2, 3]) == "[int; 3]");
    let v: Vec<float> = Vec::new();
    assert!((typeof v) == "prelude::Vec<float>");
    assert!((typeof { b: 1, a: "x" }) == "{ a: string, b: int }");
}

test("nominal types") {
    assert!((typeof Option::Some(1)) == "prelude::Option<int>");
    let r: Result<int, string> = Result::Ok(1);
    assert!((typeof r) == "prelude::Result<int, string>");
    assert!((typeof new Point(1)) == "Point");
}
