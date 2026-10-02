// `?` in a test body accepts any error type with `Show` and `Option`
// operands; a failure fails the case with the shown error (#628).
use io::open;
use io::close;
use io::write;
use io::fs::remove_file;
use io::file::read_text;
use string::to_bytes;

#[derive(Show)]
enum ParseError {
    Empty,
    Bad(string),
}

fn parse_digit(string s) -> Result<int, ParseError> {
    if s.len() == 0 {
        raise Empty;
    }
    if s == "7" {
        return 7;
    }
    raise Bad(s);
}

fn first(Vec<int> xs) -> Option<int> {
    if xs.len() == 0 {
        return None;
    }
    return Some(xs[0]);
}

test("io errors propagate with ?") {
    let path = "/tmp/coil_test_try_any_error.txt";
    let f = open(path, "w")?;
    write(f, to_bytes("hi"))?;
    close(f)?;
    assert(read_text(path)? == "hi")?;
    remove_file(path)?;
}

test("user error types with Show propagate with ?") {
    let d = parse_digit("7")?;
    assert(d == 7)?;
}

test("Option ? unwraps Some") {
    let v = first(Vec::from([3, 4]))?;
    assert(v == 3, "first element")?;
}

test("string errors still work") {
    let r: Result<int, string> = Ok(2);
    assert(r? == 2)?;
}
