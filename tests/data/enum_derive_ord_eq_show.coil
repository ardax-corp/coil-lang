// `#[derive(Show, Eq, Ord)]` on a payload enum: Eq compares tag and payload,
// Ord is lexicographic in declaration order, Show prints qualified cases.
use tests::assert;
use string::format;

#[derive(Show, Eq, Ord)]
enum Rank {
    Low,
    Mid(int),
    High(int, int),
}

#[derive(Show, Eq)]
enum Msg {
    Text(string),
    Pair { a: int, b: string },
}

test("order follows declaration order of cases") {
    assert!(Rank::Low < Rank::Mid(0));
    assert!(Rank::Mid(99) < Rank::High(0, 0));
    assert!(Rank::High(0, 0) > Rank::Low);
    assert!(!(Rank::High(0, 0) < Rank::Low));
}

test("same case orders by payload lexicographically") {
    assert!(Rank::Mid(1) < Rank::Mid(2));
    assert!(Rank::High(1, 9) < Rank::High(2, 0));
    assert!(Rank::High(1, 2) < Rank::High(1, 3));
    assert!(Rank::High(1, 2) <= Rank::High(1, 2));
    assert!(Rank::High(1, 2) >= Rank::High(1, 2));
    assert!(!(Rank::High(1, 2) < Rank::High(1, 2)));
}

test("equality compares tag and payload") {
    assert!(Rank::Mid(3) == Rank::Mid(3));
    assert!(Rank::Mid(3) != Rank::Mid(4));
    assert!(Rank::Low == Rank::Low);
    assert!(Rank::Low != Rank::Mid(0));
    assert!(Msg::Text("a") == Msg::Text("a"));
    assert!(Msg::Text("a") != Msg::Text("b"));
    assert!(Msg::Pair { a: 1, b: "x" } == Msg::Pair { b: "x", a: 1 });
    assert!(Msg::Pair { a: 1, b: "x" } != Msg::Pair { a: 1, b: "y" });
}

test("show prints the qualified case and its payload") {
    assert!(format("%v", Rank::Low) == "Rank::Low", format("%v", Rank::Low));
    assert!(format("%v", Rank::Mid(-4)) == "Rank::Mid(-4)", format("%v", Rank::Mid(-4)));
    assert!(format("%v", Rank::High(1, 2)) == "Rank::High(1, 2)");
    assert!(Rank::High(1, 2).show() == "Rank::High(1, 2)");
}
