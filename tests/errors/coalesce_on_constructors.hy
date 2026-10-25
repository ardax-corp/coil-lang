// `??` applies directly to an `Option` or `Result` constructor: the default
// fixes the payload type of a bare `Option::None` or `Result::Err(..)`.
use tests::assert;

test("defaults for a bare None and an Err") {
    let a = Option::None ?? "bar";
    assert!((typeof a) == "string");
    assert!(a == "bar");
    let b = Result::Err("boom").ok() ?? 7;
    assert!((typeof b) == "int");
    assert!(b == 7);
}
