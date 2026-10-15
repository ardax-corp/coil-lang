// `if let` and `while let` take match patterns: nested constructors,
// literals in a payload, record variants, an `else if let` chain, and a
// loop that stops at the first value that does not match.
use tests::assert;

enum Msg {
    Move { x: int, y: int },
    Say(string),
    Quit,
}

fn kind(Msg m) -> string {
    if let Msg::Say(s) = m {
        return "say " + s;
    } else if let Msg::Move { x, y } = m {
        if x == 0 {
            return "vertical";
        }
        if y == 0 {
            return "horizontal";
        }
    }
    return "other";
}

fn inner(Result<Option<int>, string> r) -> int {
    if let Result::Ok(Option::Some(v)) = r {
        return v;
    } else {
        return -1;
    }
}

test("else if let chain over tuple and record variants") {
    assert!(kind(Msg::Move { x: 0, y: 3 }) == "vertical");
    assert!(kind(Msg::Move { x: 3, y: 0 }) == "horizontal");
    assert!(kind(Msg::Move { x: 1, y: 1 }) == "other");
    assert!(kind(Msg::Say("hi")) == "say hi");
    assert!(kind(Msg::Quit) == "other");
}

test("if let with a nested pattern and else") {
    assert!(inner(Result::Ok(Option::Some(4))) == 4);
    assert!(inner(Result::Ok(Option::None)) == -1);
    assert!(inner(Result::Err("e")) == -1);
}

test("while let drains a Vec") {
    let v: Vec<int> = [1, 2, 3, 4];
    let total = 0;
    while let Option::Some(x) = v.pop() {
        total = total * 10 + x;
    }
    assert!(total == 4321);
    assert!(len(v) == 0);
}

test("while let stops at the first non-matching value") {
    let v: Vec<int> = [5, 0, 7, 8];
    let seen = 0;
    while let Option::Some(8) = v.pop() {
        seen = seen + 1;
    }
    assert!(seen == 1);
    assert!(len(v) == 2);
}
