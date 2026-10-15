// `?` returns from the function at the first `Err` / `None`: the code after
// it does not run, the `defer`s of the function still do, and the error
// reaches the caller unchanged.
use tests::assert;

class Log {
    pub steps: int,
    pub deferred: int,
}

fn step(Log log, int n) -> Result<int, string> {
    if n < 0 {
        raise "bad " + n.show();
    }
    log.steps = log.steps + 1;
    return n;
}

fn run(Log log, int a, int b, int c) -> Result<int, string> {
    defer use (log) {
        log.deferred = log.deferred + 1;
    }
    let x = step(log, a)?;
    let y = step(log, b)?;
    let z = step(log, c)?;
    return x + y + z;
}

fn find_first(Log log, Vec<Option<int>> xs) -> Option<int> {
    let total = 0;
    for x in xs {
        total = total + x?;
        log.steps = log.steps + 1;
    }
    return Option::Some(total);
}

test("all steps succeed") {
    let log = new Log(0, 0);
    assert!(run(log, 1, 2, 3) == Result::Ok(6));
    assert!(log.steps == 3);
    assert!(log.deferred == 1);
}

test("zero is a valid step") {
    let log = new Log(0, 0);
    assert!(run(log, 0, 0, 0) == Result::Ok(0));
    assert!(log.steps == 3);
}

test("the first failure stops the rest") {
    let log = new Log(0, 0);
    assert!(run(log, 1, -2, 3) == Result::Err("bad -2"));
    assert!(log.steps == 1);
    assert!(log.deferred == 1);
}

test("a failure in the first step") {
    let log = new Log(0, 0);
    assert!(run(log, -1, -2, -3) == Result::Err("bad -1"));
    assert!(log.steps == 0);
    assert!(log.deferred == 1);
}

test("Option ? inside a loop") {
    let log = new Log(0, 0);
    let xs: Vec<Option<int>> = [Option::Some(1), Option::Some(2), Option::None, Option::Some(4)];
    assert!(find_first(log, xs) == Option::None);
    assert!(log.steps == 2);
    let ys: Vec<Option<int>> = [Option::Some(5), Option::Some(6)];
    assert!(find_first(log, ys) == Option::Some(11));
}
