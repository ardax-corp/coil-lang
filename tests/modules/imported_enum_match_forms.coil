// Matching an imported enum: a `default` arm in a test body, a payload binding
// compared in the arm, and an exhaustive match inside a function body.
use tests::assert;
use qpath_provider::Shape;
use qpath_provider::inner::Level;

fn rank(Level l) -> int {
    let n = match l {
        Level::Low => 0,
        Level::High(v) => v,
    };
    return n;
}

test("a default arm on an imported enum") {
    let s = Shape::Empty;
    let ok = match s {
        Shape::Empty => true,
        default => false,
    };
    assert!(ok);
}

test("a payload binding of an imported variant") {
    let s = Shape::Square(4);
    let side = match s {
        Shape::Square(n) => n == 4,
        default => false,
    };
    assert!(side);
}

test("an exhaustive match in a function body") {
    assert!(rank(Level::Low) == 0);
    assert!(rank(Level::High(5)) == 5);
}
