// `env::args()` matched directly: the `Ok` arm binds a usable string vector
// that holds at least argv[0].
use tests::assert;
use env::args;

test("match args() binds the argument vector") {
    let n = match args() {
        Result::Ok(v) => v.len(),
        Result::Err(_) => -1,
    };
    assert!(n >= 1);
}
