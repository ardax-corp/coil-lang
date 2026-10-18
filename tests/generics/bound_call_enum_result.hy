// A bound trait call in a shared generic body whose result is a ground
// enum (`Result<int, string>`, `Option<int>`): the dictionary adapter's
// shared layout converts to the enum's own layout after the call (#1433).
use tests::assert;
use io::{stdout, Write};
use string::to_bytes;

trait Sink<T> {
    fn put(T s, int b) -> Result<int, string> {}
    fn peek(T s, int b) -> Option<int> {}
    fn both(T s, Result<int, int> r) -> Result<int, int> {}
}

class Cell {
    pub n: int,
}

impl Sink for int {
    fn put(int s, int b) -> Result<int, string> {
        return Result::Ok(s + b);
    }

    fn peek(int s, int b) -> Option<int> {
        if b > 0 {
            return Option::Some(s * b);
        }
        return Option::None;
    }

    fn both(int s, Result<int, int> r) -> Result<int, int> {
        return match r {
            Result::Ok(n) => Result::Ok(n + s),
            Result::Err(e) => Result::Err(e - s),
        };
    }
}

impl Sink for Cell {
    fn put(Cell s, int b) -> Result<int, string> {
        if b < 0 {
            return Result::Err("neg");
        }
        return Result::Ok(s.n * b);
    }

    fn peek(Cell s, int b) -> Option<int> {
        return Option::Some(s.n - b);
    }

    fn both(Cell s, Result<int, int> r) -> Result<int, int> {
        return match r {
            Result::Ok(n) => Result::Err(n * s.n),
            Result::Err(e) => Result::Ok(e),
        };
    }
}

fn go<T: Sink>(T s, int b) -> int {
    return match s.put(b) {
        Result::Ok(n) => n,
        Result::Err(_) => -1,
    };
}

fn err_of<T: Sink>(T s, int b) -> string {
    let r = put(s, b);
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
}

fn look<T: Sink>(T s, int b) -> int {
    return match s.peek(b) {
        Option::Some(n) => n,
        Option::None => -7,
    };
}

fn round<T: Sink>(T s, Result<int, int> r) -> int {
    return match s.both(r) {
        Result::Ok(n) => n,
        Result::Err(e) => 1000 + e,
    };
}

fn write_all<T: Write>(T s, Vec<byte> b) -> int {
    return match s.write(b) {
        Result::Ok(n) => n,
        Result::Err(_) => -1,
    };
}

test("bound calls returning a ground enum") {
    assert!(go(1, 2) == 3);
    assert!(go(new Cell(4), 5) == 20);
    assert!(go(new Cell(4), -1) == -1);
    assert!(err_of(new Cell(4), -1) == "neg");
    assert!(err_of(3, 1) == "ok");
    assert!(look(3, 4) == 12);
    assert!(look(3, 0) == -7);
    assert!(look(new Cell(9), 2) == 7);
    assert!(round(2, Result::Ok(5)) == 7);
    assert!(round(2, Result::Err(5)) == 1003);
    assert!(round(new Cell(3), Result::Ok(5)) == 1015);
    assert!(round(new Cell(3), Result::Err(8)) == 8);
}

test("builtin Write through a bound") {
    assert!(write_all(stdout(), to_bytes("")) == 0);
}
