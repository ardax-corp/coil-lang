// Bodies that satisfy a non-unit return type without falling off the end:
// `while true` (also on a const), `raise`, `panic`, if/else returns and
// match arms that all exit; yield-only async bodies; code after a return or
// an endless loop, and defers that never run, are warnings only.
use tests::assert;
use io::{stdout, write};
use string::{format, to_bytes};

fn forever() -> int {
    while true {}
}

fn forever_const() -> int {
    const loop = true;
    while loop {}
}

fn boom_raise() -> Result<int, string> {
    raise "x";
}

fn boom_panic() -> int {
    panic "x";
}

fn f(bool b) -> int {
    if b {
        return 1;
    } else {
        return 0;
    }
}

fn both_panic(Option<int> o) -> int {
    match o {
        Option::Some(x) => panic "some",
        Option::None => panic "none",
    }
}

fn after_return() -> int {
    return 1;
    write(stdout(), to_bytes("dead"));
}

fn after_endless() -> int {
    while true {}
    write(stdout(), to_bytes("dead"));
}

fn defer_before_endless() -> int {
    defer {
        write(stdout(), to_bytes("d"));
    }
    while true {}
}

fn defer_inside_endless() -> int {
    while true {
        defer {
            write(stdout(), to_bytes("d"));
        }
    }
}

fn unit_after_return() {
    return;
    let _ = 1;
}

gen fn gen_three() {
    yield 0;
    yield 1;
    yield 2;
}

gen fn outer() {
    yield from gen_three();
}

gen fn parameterized(int base) {
    yield base;
    yield base + 1;
    yield base + 2;
}

test("diverging bodies compile") {
    let _ = forever;
    let _ = forever_const;
    let _ = boom_panic;
    let _ = both_panic;
    let _ = after_endless;
    let _ = defer_before_endless;
    let _ = defer_inside_endless;
    let _ = outer;
    let _ = parameterized;
    let r = boom_raise();
    let msg = match r {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
    assert!(msg == "x");
}

test("if/else returns satisfy the return type") {
    assert!(format("%i,", f(true)) + format("%i", f(false)) == "1,0");
}

test("code after a return is skipped") {
    assert!(after_return() == 1);
    unit_after_return();
}

test("a yield-only async body resumes") {
    let _ = resume gen_three();
}
