// A call to another module's generic function goes to its shared body,
// boxing nothing when no argument is a bare type parameter: closures,
// vectors and results over `T` pass and come back as plain words, so
// direct MIR makes the same `CALL`.
use tests::assert;
use task::{scope, shield, timeout, Scope, TaskError};
use collections::vec::{filter, map};

fn ok_int(Result<int, TaskError> r) -> int {
    return match r {
        Result::Ok(v) => v,
        Result::Err(_) => -1,
    };
}

test("scope over int, string and Option results") {
    assert!(ok_int(scope(fn (Scope s) => 40 + 2)) == 42);
    let words = scope(fn (Scope s) => "a" + "b");
    match words {
        Result::Ok(w) => {
            assert!(w == "ab");
        },
        Result::Err(_) => {
            assert!(false, "scope failed");
        },
    }
    let some = scope(fn (Scope s) => Option::Some(7));
    match some {
        Result::Ok(Option::Some(v)) => {
            assert!(v == 7);
        },
        default => {
            assert!(false, "scope lost the option");
        },
    }
}

test("a captured value and a spawned child") {
    let base = 10;
    let r = scope(
        fn (Scope s) use (base) {
            let t = s.spawn(fn () use (base) => base * 3);
            return match t.join() {
                Result::Ok(v) => v + 1,
                Result::Err(_) => 0,
            };
        },
    );
    assert!(ok_int(r) == 31);
}

test("shield and timeout") {
    assert!(shield(fn () => 5) == 5);
    assert!(ok_int(timeout(1000, fn () => 9)) == 9);
}

test("map and filter with lambdas") {
    let xs: Vec<int> = Vec::new();
    xs.push(1);
    xs.push(2);
    xs.push(3);
    let ys = map(xs, fn (int x) => x * 10);
    assert!(ys[0] == 10 && ys[2] == 30);
    let names = map(xs, fn (int x) => "n" + "!");
    assert!(names[1] == "n!");
    assert!(len(filter(xs, fn (int x) => x > 1)) == 2);
}
