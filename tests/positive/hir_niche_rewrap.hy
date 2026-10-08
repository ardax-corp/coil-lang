// `Err(e) => return Err(e)` out of a one-word (niche) match into a
// two-word return pushes only the tag back: the matched word is the payload.
fn check(int x) -> Result<int, string> {
    assert(x > 0)?;
    return Result::Ok(x * 2);
}

fn name_len(Result<string, string> r) -> Result<int, string> {
    match r {
        Result::Ok(s) => {
            return Result::Ok(len(s));
        },
        Result::Err(e) => {
            return Result::Err(e);
        },
    }
}

fn first_word(Result<string, string> r) -> Result<string, string> {
    match r {
        Result::Ok(s) => {
            return Result::Ok(s + "!");
        },
        Result::Err(e) => {
            return Result::Err(e);
        },
    }
}

fn score(Result<int, string> r) -> int {
    return match r {
        Result::Ok(v) => v,
        Result::Err(e) => 0 - len(e),
    };
}

test("assert ? forwards its error") {
    assert(score(check(3)) == 6)?;
    assert(score(check(0)) < 0)?;
}

test("a niche Err rewraps into a pair") {
    assert(score(name_len(Result::Ok("abc"))) == 3)?;
    assert(score(name_len(Result::Err("no"))) == -2)?;
}

test("a niche Err returns its own word") {
    match first_word(Result::Err("bad")) {
        Result::Ok(_) => assert(false)?,
        Result::Err(e) => assert(e == "bad")?,
    }
    match first_word(Result::Ok("hi")) {
        Result::Ok(s) => assert(s == "hi!")?,
        Result::Err(_) => assert(false)?,
    }
}
