// COI-400: two-slot `Result<int, string>` + repeated `?` in one test
// (coil-stdlib tests/conv.hy: `assert(parse_int("123")? == 123)?`).

fn parse_int(string s) -> Result<int, string> {
    if s == "123" {
        return 123;
    }
    if s == "+8" {
        return 8;
    }
    if s == "-45" {
        return 0 - 45;
    }
    raise "invalid integer";
}

test("two-slot question inside assert once") {
    assert(parse_int("123")? == 123)?;
}

test("two-slot question inside assert twice") {
    assert(parse_int("123")? == 123)?;
    assert(parse_int("+8")? == 8)?;
}

test("two-slot question inside assert three times") {
    assert(parse_int("123")? == 123)?;
    assert(parse_int("+8")? == 8)?;
    assert(parse_int("-45")? == 0 - 45)?;
}

test("two-slot question bound then assert still works") {
    let a = parse_int("123")?;
    let b = parse_int("+8")?;
    assert(a == 123)?;
    assert(b == 8)?;
}
