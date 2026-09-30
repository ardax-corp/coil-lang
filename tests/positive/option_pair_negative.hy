fn parse(int value) {
    if value < 0 {
        raise "bad";
    }
    return value;
}

fn inc(int value) {
    let parsed = parse(value)?;
    return parsed + 1;
}

test("negative pair propagation") {
    assert(match inc(-1) {
        Result::Ok(_) => false,
        Result::Err(_) => true,
    })?;
}
