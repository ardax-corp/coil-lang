// `task::scope` returns the body's value once every child finished, joined
// or not; `join` hands back each child's value whatever order they finish in.
use tests::assert;
use task::{scope, Scope, Task, TaskError};

fn ok_or(Result<int, TaskError> r, int fallback) -> int {
    return match r {
        Result::Ok(v) => v,
        Result::Err(_) => fallback,
    };
}

test("a scope with no children returns its body's value") {
    let r = scope(fn (Scope s) => 7);
    assert!(r == Result::Ok(7));
}

test("a scope returns a non-int body value") {
    let r = scope(fn (Scope s) => "done");
    let text = match r {
        Result::Ok(t) => t,
        Result::Err(_) => "failed",
    };
    assert!(text == "done");
}

test("the scope waits for children nobody joined") {
    let log: Vec<int> = Vec::new();
    let r = scope(
        fn (Scope s) use (log) {
            s
                .spawn(
                    fn () use (log) {
                        task::sleep(20);
                        log.push(1);
                        0
                    },
                );
            s
                .spawn(
                    fn () use (log) {
                        task::yield_now();
                        log.push(2);
                        0
                    },
                );
            0
        },
    );
    assert!(ok_or(r, -1) == 0);
    assert!(log.len() == 2);
    assert!(log[0] == 2 && log[1] == 1);
}

test("join returns each value whatever order the children finish in") {
    let r = scope(
        fn (Scope s) {
            let slow = s
                .spawn(
                    fn () {
                        task::sleep(30);
                        1
                    },
                );
            let fast = s.spawn(fn () => 2);
            let mid = s
                .spawn(
                    fn () {
                        task::sleep(10);
                        3
                    },
                );
            ok_or(slow.join(), -100) * 100 + ok_or(mid.join(), -100) * 10 + ok_or(fast.join(), -100)
        },
    );
    assert!(ok_or(r, -1) == 132);
}

test("joining a task twice returns its value both times") {
    let r = scope(
        fn (Scope s) {
            let t = s.spawn(fn () => 21);
            ok_or(t.join(), -100) + ok_or(t.join(), -100)
        },
    );
    assert!(ok_or(r, -1) == 42);
}

test("a child can spawn a sibling through the captured scope") {
    let log: Vec<int> = Vec::new();
    let r = scope(
        fn (Scope s) use (log) {
            let first = s
                .spawn(
                    fn () use (s, log) {
                        log.push(1);
                        s
                            .spawn(
                                fn () use (log) {
                                    log.push(3);
                                    0
                                },
                            );
                        log.push(2);
                        0
                    },
                );
            ok_or(first.join(), -1)
        },
    );
    assert!(ok_or(r, -1) == 0);
    assert!(log.len() == 3);
    assert!(log[0] == 1 && log[1] == 2 && log[2] == 3);
}

test("a child returns a heap value through join") {
    let r = scope(
        fn (Scope s) {
            let t = s
                .spawn(
                    fn () {
                        let v: Vec<int> = Vec::new();
                        v.push(4);
                        v.push(5);
                        v
                    },
                );
            match t.join() {
                Result::Ok(v) => v.len() * 10 + v[1],
                Result::Err(_) => -1,
            }
        },
    );
    assert!(ok_or(r, -2) == 25);
}

test("tasks kept past the scope still hold their values") {
    let kept: Vec<Task<string>> = Vec::new();
    let r = scope(
        fn (Scope s) use (kept) {
            kept.push(s.spawn(fn () => "a"));
            kept.push(s.spawn(fn () => "b"));
            0
        },
    );
    assert!(ok_or(r, -1) == 0);
    let a = match kept[0].join() {
        Result::Ok(v) => v,
        Result::Err(_) => "?",
    };
    let b = match kept[1].join() {
        Result::Ok(v) => v,
        Result::Err(_) => "?",
    };
    assert!(a + b == "ab");
}
