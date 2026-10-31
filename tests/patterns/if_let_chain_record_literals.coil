// `if let` / `else if let` over record-variant patterns with literal
// fields, and plain `if let`s that bind the other field (#1162).
use tests::assert;

enum Msg {
    Move { x: int, y: int },
    Quit,
}

fn kind(Msg m) -> string {
    if let Msg::Move { x: 0, y: _ } = m {
        return "vertical";
    } else if let Msg::Move { x: _, y: 0 } = m {
        return "horizontal";
    }
    return "other";
}

fn off_axis(Msg m) -> int {
    if let Msg::Move { x: 0, y } = m {
        return y;
    }
    if let Msg::Move { x, y: 0 } = m {
        return x;
    }
    return -1;
}

test("an else-if-let chain picks the first matching literal") {
    assert!(kind(Msg::Move { x: 0, y: 5 }) == "vertical");
    assert!(kind(Msg::Move { x: 3, y: 0 }) == "horizontal");
    assert!(kind(Msg::Move { x: 1, y: 1 }) == "other");
    assert!(kind(Msg::Quit) == "other");
}

test("sequential if-lets bind the other field") {
    assert!(off_axis(Msg::Move { x: 0, y: 7 }) == 7);
    assert!(off_axis(Msg::Move { x: 4, y: 0 }) == 4);
    assert!(off_axis(Msg::Move { x: 2, y: 2 }) == -1);
    assert!(off_axis(Msg::Quit) == -1);
}
