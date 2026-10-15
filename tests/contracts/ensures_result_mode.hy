// In a function returning `Result<T, E>`, `result` in `ensures` is the
// whole `Result`, and the clause is checked on the early error return of a
// `?` as well as on `raise` and a normal return.
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

class In {
    pub v: Result<int, string>,
}

fn safe_div(int a, int b) -> Result<int, string>
    ensures match result {
        Result::Ok(q) => q * b <= a || a < 0 || b < 0,
        Result::Err(_) => b == 0,
    }
{
    if b == 0 {
        raise "division by zero";
    }
    return Result::Ok(a / b);
}

// Promises never to fail, but forwards an error with `?`.
fn positive(In i) -> Result<int, string>
    ensures match result {
        Result::Ok(v) => v > 0,
        Result::Err(_) => false,
    }, "never fails"
{
    let v = i.v?;
    return Result::Ok(v);
}

fn is_ok(Result<int, string> r) -> bool {
    return match r {
        Result::Ok(_) => true,
        Result::Err(_) => false,
    };
}

test("ensures sees Ok and Err results") {
    assert!(safe_div(7, 2) == Result::Ok(3));
    assert!(!is_ok(safe_div(7, 0)));
}

test("an Ok that keeps the clause returns normally") {
    assert!(positive(new In(Result::Ok(5))) == Result::Ok(5));
}

test("the error return of `?` is checked") {
    let m = outcome(fn () {
        let _ = positive(new In(Result::Err("bad")));
        0
    });
    assert!(has(m, "contract violated: ensures"), m);
    assert!(has(m, "(\"never fails\") in positive"), m);
}

test("an Ok that breaks the clause is checked") {
    let m = outcome(fn () {
        let _ = positive(new In(Result::Ok(0)));
        0
    });
    assert!(has(m, "in positive"), m);
}
