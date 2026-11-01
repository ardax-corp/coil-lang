// `io::capture_begin()` / `capture_end()` keep what a test prints so it can
// assert on it: stdout and stderr apart, nested captures innermost first,
// and an empty pair when none is open.
use io::{capture_begin, capture_end, stderr, stdout, write};
use string::to_bytes;
use tests::assert;

fn greet(string name) -> Result<(), IoError> {
    write(stdout(), to_bytes("hello " + name + "\n"))?;
    write(stderr(), to_bytes("greeted\n"))?;
    return;
}

test("stdout and stderr apart") {
    capture_begin();
    greet("coil")?;
    let (out, err) = capture_end();
    assert!(out == "hello coil\n");
    assert!(err == "greeted\n");
}

test("captures nest") {
    capture_begin();
    write(stdout(), to_bytes("outer "))?;
    capture_begin();
    write(stdout(), to_bytes("inner"))?;
    let (inner, _) = capture_end();
    write(stdout(), to_bytes("again"))?;
    let (outer, _) = capture_end();
    assert!(inner == "inner");
    assert!(outer == "outer again");
}

test("nothing open") {
    let (out, err) = capture_end();
    assert!(out == "" && err == "");
}
