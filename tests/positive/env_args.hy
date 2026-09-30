use env::args;

test("args ok has argv0") {
    let a = match args() {
        Result::Ok(v) => v,
        Result::Err(_) => panic "args err",
    };
    assert(a.len() >= 1)?;
    assert(len(a[0]) >= 1)?;
}
