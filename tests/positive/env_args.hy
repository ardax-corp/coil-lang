use env::args;

test("args ok has argv0") {
    let a = args()?;
    assert(a.len() >= 1)?;
    assert(len(a[0]) >= 1)?;
}
