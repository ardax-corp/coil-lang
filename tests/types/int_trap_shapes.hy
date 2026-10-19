// More `int` trap shapes: `**`, `/ -1`, compound assignment, loop
// accumulators and vectorizable `Vec` loops panic with `integer overflow`;
// `/` and `%` by a zero from a call panic with `division by zero`; values
// that land on a limit do not trap.
use tests::assert;
use task::{scope, Scope, TaskError};

fn message(Result<int, TaskError> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(TaskError::Panicked(m)) => m,
        Result::Err(_) => "other",
    };
}

// Run `body` as a child task: its panic message, or "ok".
fn outcome(unit -> int body) -> string {
    return message(scope(fn (Scope s) use (body) {
        let _ = s.spawn(body);
        0
    }));
}

// int::MAX from a call, so nothing folds it.
fn big() -> int {
    return 9223372036854775807;
}

fn small() -> int {
    return 0 - big() - 1;
}

fn id(int x) -> int {
    return x;
}

fn add_assign_big() -> int {
    let x = big();
    x += 1;
    return x;
}

fn sub_assign_small() -> int {
    let x = small();
    x -= 1;
    return x;
}

fn for_accumulate() -> int {
    let s = big() - 10;
    for i in 0..20 {
        s = s + 1;
    }
    return s;
}

fn while_triple() -> int {
    let s = 1;
    let i = 0;
    while i < 100 {
        s = s * 3;
        i = i + 1;
    }
    return s;
}

fn scan(Vec<int> v) -> int {
    let acc = 0;
    let i = 0;
    while i < v.len() {
        acc = acc + v[i];
        i = i + 1;
    }
    return acc;
}

fn scale(int k, Vec<int> x, Vec<int> y) -> int {
    let i = 0;
    while i < x.len() {
        y[i] = k * x[i];
        i = i + 1;
    }
    return y[0];
}

test("operators and loops trap on overflow") {
    assert!(outcome(fn () => big() + 1) == "integer overflow");
    assert!(outcome(fn () => small() - 1) == "integer overflow");
    assert!(outcome(fn () => big() * 2) == "integer overflow");
    assert!(outcome(fn () => -small()) == "integer overflow");
    assert!(outcome(fn () => small() / id(-1)) == "integer overflow");
    assert!(outcome(fn () => id(2) ** 63) == "integer overflow");
    assert!(outcome(fn () => add_assign_big()) == "integer overflow");
    assert!(outcome(fn () => sub_assign_small()) == "integer overflow");
    assert!(outcome(fn () => for_accumulate()) == "integer overflow");
    assert!(outcome(fn () => while_triple()) == "integer overflow");
}

test("division by a zero from a call traps") {
    assert!(outcome(fn () => big() / id(0)) == "division by zero");
    assert!(outcome(fn () => big() % id(0)) == "division by zero");
}

test("values at the edges do not trap") {
    let min = 0 - big() - 1;
    assert!(big() - 1 + 1 == 9223372036854775807);
    assert!(min % id(-1) == 0);
    assert!(id(-2) ** 63 == min);
}

test("vectorizable loops trap like scalar ones") {
    let big = 4611686018427387904;
    let v: Vec<int> = Vec::from([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
    let w: Vec<int> = Vec::from([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, big]);
    let y: Vec<int> = Vec::from([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert!(scan(v) == 136);
    assert!(scale(2, v, y) == 2);
    assert!(outcome(fn () use (w, y) => scale(2, w, y)) == "integer overflow");
    // A sum folds in element order: big + big overflows first.
    let u: Vec<int> = Vec::from([big, big, 0 - big, 0 - big, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
    assert!(outcome(fn () use (u) => scan(u)) == "integer overflow");
}
