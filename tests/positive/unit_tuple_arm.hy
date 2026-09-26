// `()` and a unit-returning call are the same type in match arms.
static let hits: int = 0;

fn bump() {
    hits = hits + 1;
}

test("unit call arm next to a () arm") {
    let a: Option<int> = Option::Some(1);
    match a {
        Option::Some(_) => bump(),
        Option::None => (),
    };
    let b: Option<int> = Option::None;
    match b {
        Option::None => (),
        Option::Some(_) => bump(),
    };
    assert(hits == 1)?;
}
