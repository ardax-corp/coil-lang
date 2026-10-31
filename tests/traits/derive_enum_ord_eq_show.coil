// #[derive] Ord / Eq / Show on enums.
use tests::assert;

#[derive(Ord, Eq)]
enum Color {
    Red,
    Blue,
    Green,
}

#[derive(Eq)]
enum Flag {
    Off,
    On,
}

test("derived Ord order") {
    assert!(Color::Red < Color::Blue);
    assert!(Color::Blue < Color::Green);
    assert!(Color::Red <= Color::Red);
    assert!(!(Color::Blue < Color::Red));
}

test("derived Eq") {
    assert!(Color::Red == Color::Red);
    assert!(Color::Red != Color::Blue);
    assert!(Flag::On == Flag::On);
    assert!(Flag::Off != Flag::On);
}
