// `task::blocking` / `blocking_with` run work on a CPU thread and return its
// result as `Result<T, ThreadError>`; only the calling task waits, and
// outside a scope they just wait.
use tests::assert;
use task::{scope, Scope, TaskError};
use thread::ThreadError;
use clock::sleep_ms;

fn crunch() -> int {
    let total = 0;
    let i = 0;
    while i < 1000 {
        total += i;
        i += 1;
    }
    return total;
}

fn slow_crunch() -> int {
    sleep_ms(30);
    return crunch();
}

fn times(int n) -> int {
    return n * 3;
}

fn greeting() -> string {
    return "hi ada";
}

fn got(Result<int, ThreadError> r) -> int {
    return match r {
        Result::Ok(v) => v,
        Result::Err(_) => -1,
    };
}

test("blocking outside a scope returns the work's value") {
    assert!(got(task::blocking(crunch)) == 499500);
}

test("blocking_with passes its argument") {
    assert!(got(task::blocking_with(times, 14)) == 42);
    assert!(got(task::blocking_with(times, -5)) == -15);
}

test("blocking returns a string result") {
    let s = match task::blocking(greeting) {
        Result::Ok(v) => v,
        Result::Err(_) => "failed",
    };
    assert!(s == "hi ada");
}

test("other tasks run while one waits on blocking") {
    let log: Vec<string> = Vec::new();
    let r = scope(
        fn (Scope s) use (log) {
            let worker = s
                .spawn(
                    fn () use (log) {
                        let v = got(task::blocking(slow_crunch));
                        log.push("blocking done");
                        v
                    },
                );
            s
                .spawn(
                    fn () use (log) {
                        log.push("other ran");
                        0
                    },
                );
            match worker.join() {
                Result::Ok(v) => v,
                Result::Err(_) => -1,
            }
        },
    );
    assert!(r == Result::Ok(499500));
    assert!(log.len() == 2);
    assert!(log[0] == "other ran" && log[1] == "blocking done");
}

test("several tasks wait on blocking work at once") {
    let r = scope(
        fn (Scope s) {
            let a = s.spawn(fn () => got(task::blocking_with(times, 1)));
            let b = s.spawn(fn () => got(task::blocking_with(times, 2)));
            let c = s.spawn(fn () => got(task::blocking_with(times, 3)));
            let total = 0;
            for t in [a, b, c] {
                total += match t.join() {
                    Result::Ok(v) => v,
                    Result::Err(_) => -100,
                };
            }
            total
        },
    );
    assert!(r == Result::Ok(18));
}
