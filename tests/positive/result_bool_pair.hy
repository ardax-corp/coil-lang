// A `Result<bool, string>` keeps its `bool` payload in the pair's word
// beside the string's: built, matched, passed and returned.
fn parse_flag(string s) -> Result<bool, string> {
    if s == "yes" {
        return Result::Ok(true);
    }
    if s == "no" {
        return Result::Ok(false);
    }
    return Result::Err("bad: " + s);
}

fn flip(Result<bool, string> r) -> Result<bool, string> {
    return match r {
        Result::Ok(b) => Result::Ok(!b),
        Result::Err(e) => Result::Err(e),
    };
}

fn show(Result<bool, string> r) -> string {
    return match r {
        Result::Ok(b) => {
            if b {
                return "true";
            }
            "false"
        },
        Result::Err(e) => "err " + e,
    };
}

fn count_true(int n) -> int {
    let c = 0;
    let i = 0;
    while i < n {
        let word = "no";
        if i % 3 == 0 {
            word = "yes";
        }
        let r = parse_flag(word);
        match r {
            Result::Ok(b) => {
                if b {
                    c = c + 1;
                }
            },
            Result::Err(_) => {},
        }
        i = i + 1;
    }
    return c;
}

test("bool payloads round trip") {
    assert(show(parse_flag("yes")) == "true")?;
    assert(show(parse_flag("no")) == "false")?;
    assert(show(parse_flag("x")) == "err bad: x")?;
    assert(show(flip(parse_flag("yes"))) == "false")?;
    assert(show(flip(parse_flag("no"))) == "true")?;
    assert(show(flip(parse_flag("q"))) == "err bad: q")?;
    assert(count_true(10) == 4)?;
}
