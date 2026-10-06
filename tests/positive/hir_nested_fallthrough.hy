// A nested test that misses falls through to the trailing catch-all, which
// HIR shares as the inner match's last arm.
enum Color {
    Red,
    Green,
}

fn status(Option<int> o) -> int {
    return match o {
        Option::Some(200) => 1,
        Option::Some(404) => 2,
        default => 0,
    };
}

fn red(Option<Color> o) -> string {
    return match o {
        Option::Some(Color::Red) => "red",
        whole => "rest",
    };
}

fn deep(Result<Option<int>, int> r) -> int {
    return match r {
        Result::Ok(Option::Some(7)) => 7,
        Result::Err(3) => 3,
        default => -1,
    };
}

test("literal miss reaches the catch-all") {
    assert(status(Option::Some(200)) == 1)?;
    assert(status(Option::Some(404)) == 2)?;
    assert(status(Option::Some(5)) == 0)?;
    assert(status(Option::None) == 0)?;
}

test("variant miss reaches a binding catch-all") {
    assert(red(Option::Some(Color::Red)) == "red")?;
    assert(red(Option::Some(Color::Green)) == "rest")?;
    assert(red(Option::None) == "rest")?;
}

test("two levels and two groups share one catch-all") {
    assert(deep(Result::Ok(Option::Some(7))) == 7)?;
    assert(deep(Result::Ok(Option::Some(8))) == -1)?;
    assert(deep(Result::Ok(Option::None)) == -1)?;
    assert(deep(Result::Err(3)) == 3)?;
    assert(deep(Result::Err(4)) == -1)?;
}
