// Exercised by `coil test`.
use tests::assert;

#[derive(Ord)]
enum Color {
    Red,
    Blue,
}

test("derived Ord compares Red before Blue") {
    assert!(Color::Red < Color::Blue);
    assert!(Color::Red <= Color::Red);
    assert!(!(Color::Blue < Color::Red));
}
