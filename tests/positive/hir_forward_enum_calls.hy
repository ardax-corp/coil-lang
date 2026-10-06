// Calls to functions declared later in the file that return an enum or a
// pair: the call reads the two-word return kind from the signature.
// `checked` also guards the MIR->LIR rebuild: its block labels must skip
// the entry id of `halve`, which is the next one after its own.
fn bounce_a(Option<int> o) -> Option<int> {
    return bounce_b(o);
}

fn bounce_b(Option<int> o) -> Option<int> {
    return match o {
        Option::Some(n) => Option::Some(n + 1),
        Option::None => Option::None,
    };
}

fn checked(int n) -> Result<int, string> {
    let m = halve(n)?;
    return Result::Ok(m + 1);
}

fn halve(int n) -> Result<int, string> {
    if n % 2 == 1 {
        return Result::Err("odd");
    }
    return Result::Ok(n / 2);
}

test("forward call returning Option") {
    assert(bounce_a(Option::Some(41)) == Option::Some(42))?;
    assert(bounce_a(Option::None) == Option::None)?;
}

test("forward call returning Result") {
    assert(checked(10) == Result::Ok(6))?;
    assert(checked(3) == Result::Err("odd"))?;
}
