// Exercised by `coil test`.
use tests::assert;

#[derive(Eq, Ord)]
enum Color {
    Red,
    Blue,
}

test("derived Ord compares Red before Blue") {
    assert!(Color::Red < Color::Blue);
    assert!(Color::Red <= Color::Red);
    assert!(!(Color::Blue < Color::Red));
}

// Unit variants compare by declaration order, not by where their values
// live in memory; repeat so a pointer-order compare would flake.
test("derived Ord on unit variants is declaration order") {
    let i = 0;
    while i < 8 {
        assert!(Color::Red < Color::Blue);
        assert!(!(Color::Blue < Color::Red));
        assert!(!(Color::Red < Color::Red));
        assert!(Color::Red <= Color::Red);
        assert!(Color::Blue > Color::Red);
        i = i + 1;
    }
}
