// A two-slot `[payload, tag]` return only carries the *returned* enum's tag. An
// `Ok`/`Some` payload that is itself an enum has to stay boxed, or its tag is
// mistaken for the function's.
fn parse(int n, int fail) {
    if fail == 1 {
        raise "bad";
    }
    return n;
}

fn maybe(int fail, int empty) -> Result<Option<int>, string> {
    let value = parse(7, fail)?;
    if empty == 1 {
        return Option::None;
    }
    return Option::Some(value);
}

// Payload of the same kind, one level down.
fn nested_result(int fail) -> Result<Result<int, string>, string> {
    let value = parse(7, fail)?;
    return Result::Ok(value);
}

test("Option payload keeps its own tag") {
    assert(
        match maybe(0, 0) {
            Result::Ok(inner) => match inner {
                Option::Some(value) => value == 7,
                Option::None => false,
            },
            Result::Err(_) => false,
        },
    )?;
    assert(
        match maybe(0, 1) {
            Result::Ok(inner) => match inner {
                Option::Some(_) => false,
                Option::None => true,
            },
            Result::Err(_) => false,
        },
    )?;
    assert(
        match maybe(1, 0) {
            Result::Ok(_) => false,
            Result::Err(_) => true,
        },
    )?;
}

test("nested Result payload keeps its own tag") {
    assert(
        match nested_result(0) {
            Result::Ok(inner) => match inner {
                Result::Ok(value) => value == 7,
                Result::Err(_) => false,
            },
            Result::Err(_) => false,
        },
    )?;
    assert(
        match nested_result(1) {
            Result::Ok(_) => false,
            Result::Err(_) => true,
        },
    )?;
}
