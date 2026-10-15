// `??` is right-associative: `a ?? b ?? c` is `a ?? (b ?? c)`, so the first
// success wins and the last operand is the fallback. A Result's `Err` is
// discarded. `??` binds looser than `==` and `+`, so `a ?? b + c` is
// `a ?? (b + c)`.
use tests::assert;

fn some(int v) -> Option<int> {
    return Option::Some(v);
}

fn none() -> Option<int> {
    return Option::None;
}

fn ok(int v) -> Result<int, string> {
    return Result::Ok(v);
}

fn err() -> Result<int, string> {
    return Result::Err("e");
}

test("the first success in a chain wins") {
    assert!((some(1) ?? some(2) ?? 3) == 1);
    assert!((none() ?? some(2) ?? 3) == 2);
    assert!((none() ?? none() ?? 3) == 3);
}

test("Option and Result mixed in one chain") {
    assert!((err() ?? some(4) ?? 0) == 4);
    assert!((none() ?? ok(5) ?? 0) == 5);
    assert!((err() ?? none() ?? -1) == -1);
}

test("strings and an expression fallback") {
    let name: Option<string> = Option::None;
    let alias: Option<string> = Option::Some("al");
    assert!((name ?? alias ?? "anon") == "al");
    assert!((name ?? "x" + "y") == "xy");
}

test("assignment takes the whole chain") {
    let v = 0;
    v = none() ?? 9;
    assert!(v == 9);
    v = some(v + 1) ?? 0;
    assert!(v == 10);
}
