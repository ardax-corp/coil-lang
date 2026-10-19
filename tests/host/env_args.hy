use tests::assert;
use env::args;

test("args ok has argv0") {
    let a = args()?;
    assert!(a.len() >= 1);
    assert!(a[0].len() >= 1);
}
