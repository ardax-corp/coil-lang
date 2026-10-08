// A `defer` that read-modify-writes a field, in a function whose `?` can
// miss, compiled to a "label was never bound" panic once the body took the
// dense tier: the thunk is reached only by `CALL` and was dropped (#760).
class Log {
    pub n: int,
}

fn step(int n) -> Result<int, int> {
    if n == 0 {
        return Result::Err(-1);
    }
    return Result::Ok(n);
}

fn guarded(Log log, int n) -> Result<int, int> {
    defer use (log) {
        log.n = log.n + 1;
    }
    let a = step(n)?;
    return Result::Ok(a);
}

test("defer with a field increment runs on both exits of a ? body") {
    let log = new Log(0);
    let miss = guarded(log, 0);
    assert(log.n == 1)?;
    let hit = guarded(log, 4);
    assert(log.n == 2)?;
    let tags = match miss {
        Result::Ok(_) => 0,
        Result::Err(e) => e,
    } * 10 + match hit {
        Result::Ok(v) => v,
        Result::Err(_) => 0,
    };
    assert(tags == -6)?;
}
