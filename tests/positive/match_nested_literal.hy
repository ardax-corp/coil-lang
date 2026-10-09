// #595: a literal or constructor nested in a constructor pattern is a test,
// not a wildcard. `Some(200)` matches only 200, and a miss falls through to
// the next arm, even one with another outer tag (`default`, a binding).

enum Color {
    Red,
    Green,
}

#[repr(int)]
enum Level {
    Low = 1,
    High = 9,
}

enum Msg {
    Move(int, int),
    Paint { color: Color, times: int },
}

fn status(Option<int> o) -> string {
    return match o {
        Option::Some(200) => "200",
        default => "not 200",
    };
}

fn code(Option<int> o) -> string {
    return match o {
        Option::Some(200) => "two hundred",
        Option::Some(404) => "four oh four",
        Option::Some(n) => "other",
        Option::None => "none",
    };
}

fn red_or_rest(Option<Color> o) -> string {
    return match o {
        Option::Some(Color::Red) => "red",
        whole => "rest",
    };
}

fn level(Option<Level> o) -> int {
    return match o {
        Option::Some(Level::High) => 2,
        Option::Some(Level::Low) => 1,
        Option::None => 0,
    };
}

fn moves(Msg m) -> int {
    return match m {
        Msg::Move(0, 0) => 0,
        Msg::Move(0, y) => y,
        Msg::Move(x, 0) => x * 10,
        Msg::Paint { color: Color::Red, times: 3 } => 300,
        Msg::Paint { color: _, times } => times,
        Msg::Move(x, y) => x * 100 + y,
    };
}

// A wildcard before a binding must not shift the binding's slot.
fn second(Msg m) -> int {
    return match m {
        Msg::Move(_, y) => y,
        Msg::Paint { color: _, times } => times,
    };
}

fn deep(Option<Option<int>> o) -> int {
    return match o {
        Option::Some(Option::Some(1)) => 1,
        Option::Some(Option::Some(n)) => n + 100,
        Option::Some(Option::None) => -1,
        Option::None => -2,
    };
}

// `Option<string>` in a `Result` payload is a pointer-niche word, not an enum.
fn niche(Result<Option<string>, int> r) -> string {
    return match r {
        Result::Ok(Option::Some(s)) => s,
        Result::Ok(Option::None) => "none",
        Result::Err(7) => "seven",
        Result::Err(_) => "err",
    };
}

fn niche_err(Result<string, Option<string>> r) -> string {
    return match r {
        Result::Err(Option::Some(e)) => e,
        Result::Err(Option::None) => "no error text",
        Result::Ok(v) => v,
    };
}

fn count_hits(Vec<Option<int>> items) -> int {
    let hits = 0;
    for item in items {
        match item {
            Option::Some(1) => {
                hits = hits + 1;
            },
            Option::Some(_) => {},
            Option::None => {
                hits = hits + 100;
            },
        }
    }
    return hits;
}

fn first_three(Vec<Option<int>> items) -> int {
    let i = 0;
    let n = 0;
    while let Option::Some(3) = items[i] {
        n = n + 1;
        i = i + 1;
    }
    return n;
}

test("nested integer literal is a test, not a wildcard") {
    assert(status(Option::Some(500)) == "not 200")?;
    assert(status(Option::Some(200)) == "200")?;
    assert(status(Option::None) == "not 200")?;
    assert(code(Option::Some(200)) == "two hundred")?;
    assert(code(Option::Some(404)) == "four oh four")?;
    assert(code(Option::Some(7)) == "other")?;
    assert(code(Option::None) == "none")?;
}

test("nested variant miss falls through to a later arm") {
    assert(red_or_rest(Option::Some(Color::Red)) == "red")?;
    assert(red_or_rest(Option::Some(Color::Green)) == "rest")?;
    assert(red_or_rest(Option::None) == "rest")?;
    assert(level(Option::Some(Level::High)) == 2)?;
    assert(level(Option::Some(Level::Low)) == 1)?;
    assert(level(Option::None) == 0)?;
}

test("literals in tuple and record payloads") {
    assert(moves(Msg::Move(0, 0)) == 0)?;
    assert(moves(Msg::Move(0, 5)) == 5)?;
    assert(moves(Msg::Move(4, 0)) == 40)?;
    assert(moves(Msg::Move(4, 5)) == 405)?;
    assert(moves(Msg::Paint { color: Color::Red, times: 3 }) == 300)?;
    assert(moves(Msg::Paint { color: Color::Red, times: 4 }) == 4)?;
    assert(moves(Msg::Paint { color: Color::Green, times: 3 }) == 3)?;
    assert(second(Msg::Move(3, 4)) == 4)?;
    assert(second(Msg::Paint { color: Color::Red, times: 6 }) == 6)?;
}

test("doubly nested patterns") {
    assert(deep(Option::Some(Option::Some(1))) == 1)?;
    assert(deep(Option::Some(Option::Some(5))) == 105)?;
    assert(deep(Option::Some(Option::None)) == -1)?;
    assert(deep(Option::None) == -2)?;
}

test("niche-encoded payloads") {
    assert(niche(Result::Ok(Option::Some("s"))) == "s")?;
    assert(niche(Result::Ok(Option::None)) == "none")?;
    assert(niche(Result::Err(7)) == "seven")?;
    assert(niche(Result::Err(8)) == "err")?;
    assert(niche_err(Result::Err(Option::Some("bad"))) == "bad")?;
    assert(niche_err(Result::Err(Option::None)) == "no error text")?;
    assert(niche_err(Result::Ok("fine")) == "fine")?;
}

test("statement match in a loop, if let and while let") {
    let items: Vec<Option<int>> = Vec::new();
    items.push(Option::Some(1));
    items.push(Option::Some(2));
    items.push(Option::None);
    items.push(Option::Some(1));
    assert(count_hits(items) == 102)?;
    let threes: Vec<Option<int>> = Vec::new();
    threes.push(Option::Some(3));
    threes.push(Option::Some(3));
    threes.push(Option::Some(4));
    assert(first_three(threes) == 2)?;
    let hit = 0;
    if let Option::Some(42) = Option::Some(42) {
        hit = 1;
    }
    if let Option::Some(42) = Option::Some(41) {
        hit = 10;
    }
    assert(hit == 1)?;
}
