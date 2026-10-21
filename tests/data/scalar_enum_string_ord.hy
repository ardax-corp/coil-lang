// A `#[repr(string)]` enum orders by its backing string (byte-wise), as an
// `int`-backed one orders by its number: with or without `#[derive(Ord)]`,
// and not by declaration order (#1722).
use tests::assert;

#[repr(string)]
enum Fruit {
    Pear = "pear",
    Fig = "fig",
    Apple = "apple",
}

#[repr(string)]
#[derive(Eq, Ord)]
enum Level {
    Warn = "warn",
    Debug = "debug",
    Info = "info",
}

#[repr(int)]
#[derive(Eq, Ord)]
enum Rank {
    High = 10,
    Low = 1,
}

fn smaller<T: Ord>(T a, T b) -> T {
    if a < b {
        return a;
    }
    return b;
}

fn pick(int i) -> Fruit {
    if i == 0 {
        return Fruit::Pear;
    }
    if i == 1 {
        return Fruit::Fig;
    }
    return Fruit::Apple;
}

test("operators without a derive order by the backing string") {
    assert!(Fruit::Fig < Fruit::Pear);
    assert!(Fruit::Apple <= Fruit::Fig);
    assert!(Fruit::Pear > Fruit::Apple);
    assert!(Fruit::Pear >= Fruit::Pear);
    assert!(!(Fruit::Pear < Fruit::Fig));
    assert!(pick(2) < pick(1));
    assert!(pick(0) > pick(1));
}

test("against a plain string") {
    assert!(Fruit::Fig < "g");
    assert!("a" < Fruit::Apple);
}

test("derived Ord orders by the backing string") {
    assert!(Level::Debug < Level::Info);
    assert!(Level::Info < Level::Warn);
    assert!(Level::Warn >= Level::Info);
    assert!(Level::Debug <= Level::Debug);
    assert!(!(Level::Warn < Level::Debug));
    assert!(smaller(Level::Warn, Level::Debug) == Level::Debug);
}

test("as an int-backed enum orders by its number") {
    assert!(Rank::Low < Rank::High);
    assert!(smaller(Rank::High, Rank::Low) == Rank::Low);
}
