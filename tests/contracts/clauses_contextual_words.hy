// Clauses may call `pure fn`s and other effect-free code, a `pure fn` may
// carry contracts, and `requires` / `ensures` / `result` are only special
// in clause position: a variable or function named `result` still works.
use tests::assert;
use task::{scope, Scope, TaskError};
use string::find_from;

fn message(Result<int, TaskError> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(TaskError::Panicked(m)) => m,
        Result::Err(_) => "other",
    };
}

// Run `body` as a child task; its panic message, or "ok".
fn outcome(unit -> int body) -> string {
    return message(scope(fn (Scope s) use (body) {
        let _ = s.spawn(body);
        0
    }));
}

fn has(string m, string part) -> bool {
    return find_from(m, part, 0) >= 0;
}

pure fn is_even(int n) -> bool {
    return n % 2 == 0;
}

pure fn half(int n) -> int
    requires is_even(n), "even input"
    ensures result + result == n
{
    return n / 2;
}

fn result_of(int x) -> int {
    let result = x + 1;
    return result;
}

fn sum_all([int; 3] xs) -> int
    ensures result == xs[0] + xs[1] + xs[2]
{
    let result = 0;
    for x in xs {
        result = result + x;
    }
    return result;
}

fn ensures(int x) -> int {
    return x;
}

test("a pure fn with contracts") {
    assert!(half(10) == 5);
    assert!(half(0 - 4) == 0 - 2);
    let m = outcome(fn () => half(3));
    assert!(has(m, "requires is_even(n) (\"even input\") in half"), m);
}

test("a local named result is an ordinary variable") {
    assert!(result_of(4) == 5);
    assert!(sum_all([1, 2, 3]) == 6);
}

test("a function named ensures is an ordinary function") {
    assert!(ensures(7) == 7);
}
