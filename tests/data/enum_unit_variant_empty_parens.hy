// A variant declared `Unit()` is a unit variant, as `Unit` is (#1168).
// `coil fmt` writes that declaration as `Unit`, so the corpus holds the
// canonical form; the calls and patterns keep their `()`.
use tests::assert;

enum Shape {
    Unit,
    Square(int),
}

fn area(Shape s) -> int {
    return match s {
        Shape::Unit => 0,
        Shape::Square(n) => n * n,
    };
}

test("`Unit()` builds with or without parentheses") {
    assert!(area(Shape::Unit()) == 0);
    assert!(area(Shape::Unit) == 0);
    assert!(area(Shape::Square(3)) == 9);
}

test("a `Unit()` value matches the unit pattern") {
    let a = Shape::Unit();
    let hit = match a {
        Shape::Unit => true,
        default => false,
    };
    assert!(hit);
}
