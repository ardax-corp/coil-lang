// Primitive type names in annotations match case-insensitively: `Int`,
// `INT` and `int` are one type, as are `String` / `string`, `Bool`,
// `Float` and `Byte`.
use tests::assert;

fn twice(Int n) -> INT {
    return n * 2;
}

fn greet(String s) -> string {
    return "hi " + s;
}

test("annotations in any case") {
    let a: Int = 4;
    let b: int = twice(a);
    assert!(b == 8);
    let f: Float = 1.5;
    assert!(f * 2.0 == 3.0);
    let ok: BOOL = true;
    assert!(ok);
    let by: Byte = 200;
    assert!(by as int == 200);
}

test("string names") {
    let s: String = "you";
    assert!(greet(s) == "hi you");
    assert!((typeof s) == "string");
    assert!((typeof twice(1)) == "int");
}
