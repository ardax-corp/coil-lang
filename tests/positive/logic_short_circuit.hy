// `&&` / `||` evaluate the right operand only when the left does not decide
// the result (#718). Pure, trap-free operands may still lower eagerly.
// HIR only: coil-lang#785 (the AST codegen does not carry this fix).
class Log {
    pub n: int,
}

fn probe(Log log, bool r) -> bool {
    log.n = log.n + 1;
    return r;
}

fn and_in_if(Log log, int i) -> bool {
    if i < 0 && probe(log, true) {
        return true;
    }
    return false;
}

fn and_in_let(Log log, int i) -> bool {
    let r = i < 0 && probe(log, true);
    return r;
}

fn or_in_if(Log log, int i) -> bool {
    if i >= 0 || probe(log, true) {
        return true;
    }
    return false;
}

fn or_in_let(Log log, int i) -> bool {
    let r = i >= 0 || probe(log, false);
    return r;
}

fn guarded_index(Vec<int> v) -> bool {
    return len(v) > 0 && v[0] == 5;
}

fn guarded_div(int d) -> bool {
    return d != 0 && 10 / d == 5;
}

fn count_in_loop(Log log, int n) -> int {
    let c = 0;
    let i = 0;
    while i < n {
        if i % 2 == 0 && probe(log, true) {
            c = c + 1;
        }
        if i % 2 == 0 || probe(log, false) {
            c = c + 10;
        }
        i = i + 1;
    }
    return c;
}

test("&& skips the right operand when the left is false") {
    let log = new Log(0);
    assert(!and_in_if(log, 3))?;
    assert(!and_in_let(log, 3))?;
    assert(log.n == 0)?;
    assert(and_in_if(log, -1))?;
    assert(and_in_let(log, -1))?;
    assert(log.n == 2)?;
}

test("|| skips the right operand when the left is true") {
    let log = new Log(0);
    assert(or_in_if(log, 3))?;
    assert(or_in_let(log, 3))?;
    assert(log.n == 0)?;
    assert(!or_in_let(log, -1))?;
    assert(log.n == 1)?;
}

test("a guard protects an index and a division") {
    let v: Vec<int> = Vec::new();
    assert(!guarded_index(v))?;
    v.push(5);
    assert(guarded_index(v))?;
    assert(!guarded_div(0))?;
    assert(guarded_div(2))?;
}

test("short-circuit inside a loop") {
    let log = new Log(0);
    assert(count_in_loop(log, 6) == 33)?;
    assert(log.n == 6)?;
}
