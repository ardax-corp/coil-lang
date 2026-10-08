// T1 tasks: `task::scope` / `spawn` / `join` / `sleep` / `yield_now`.
use task::{scope, Scope, Task, TaskError};
use clock::sleep_ms;

fn ok_or(Result<int, TaskError> r, int fallback) -> int {
    return match r {
        Result::Ok(v) => v,
        Result::Err(_) => fallback,
    };
}

test("children run at the parent's first suspension, in spawn order") {
    let log: Vec<int> = Vec::new();
    let r = scope(
        fn (Scope s) use (log) {
            s.spawn(fn () use (log) {
                log.push(1);
                0
            });
            s.spawn(fn () use (log) {
                log.push(2);
                0
            });
            log.push(0);
            0
        },
    );
    assert(ok_or(r, -1) == 0)?;
    assert(log.len() == 3)?;
    assert(log[0] == 0 && log[1] == 1 && log[2] == 2)?;
}

test("join returns the child's value") {
    let r = scope(
        fn (Scope s) {
            let a = s.spawn(fn () => 20);
            let b = s.spawn(fn () => 22);
            ok_or(a.join(), -100) + ok_or(b.join(), -100)
        },
    );
    assert(ok_or(r, -1) == 42)?;
}

test("sleep orders tasks by deadline") {
    let log: Vec<int> = Vec::new();
    let r = scope(
        fn (Scope s) use (log) {
            s
                .spawn(
                    fn () use (log) {
                        task::sleep(40);
                        log.push(40);
                        0
                    },
                );
            s
                .spawn(
                    fn () use (log) {
                        task::sleep(5);
                        log.push(5);
                        0
                    },
                );
            s
                .spawn(
                    fn () use (log) {
                        task::sleep(20);
                        log.push(20);
                        0
                    },
                );
            0
        },
    );
    assert(ok_or(r, -1) == 0)?;
    assert(log.len() == 3)?;
    assert(log[0] == 5 && log[1] == 20 && log[2] == 40)?;
}

test("yield_now interleaves ready tasks") {
    let log: Vec<int> = Vec::new();
    let r = scope(
        fn (Scope s) use (log) {
            s
                .spawn(
                    fn () use (log) {
                        log.push(1);
                        task::yield_now();
                        log.push(3);
                        0
                    },
                );
            s
                .spawn(
                    fn () use (log) {
                        log.push(2);
                        task::yield_now();
                        log.push(4);
                        0
                    },
                );
            0
        },
    );
    assert(ok_or(r, -1) == 0)?;
    assert(log[0] == 1 && log[1] == 2 && log[2] == 3 && log[3] == 4)?;
}

test("nested scopes inside a child") {
    let r = scope(
        fn (Scope s) {
            let outer = s
                .spawn(
                    fn () {
                        let inner = scope(
                            fn (Scope t) {
                                let x = t
                                    .spawn(
                                        fn () {
                                            task::sleep(2);
                                            5
                                        },
                                    );
                                let y = t.spawn(fn () => 6);
                                ok_or(x.join(), -100) * ok_or(y.join(), -100)
                            },
                        );
                        ok_or(inner, -1000)
                    },
                );
            ok_or(outer.join(), -10000)
        },
    );
    assert(ok_or(r, -1) == 30)?;
}

test("join after the scope ended") {
    let kept: Vec<Task<int>> = Vec::new();
    let r = scope(
        fn (Scope s) use (kept) {
            kept
                .push(
                    s
                        .spawn(
                            fn () {
                                task::sleep(1);
                                9
                            },
                        ),
                );
            0
        },
    );
    assert(ok_or(r, -1) == 0)?;
    assert(ok_or(kept[0].join(), -1) == 9)?;
}

test("a child panic fails the scope and drops its siblings") {
    let log: Vec<int> = Vec::new();
    let r = scope(
        fn (Scope s) use (log) {
            s
                .spawn(
                    fn () use (log) {
                        task::sleep(50);
                        log.push(1);
                        0
                    },
                );
            s
                .spawn(
                    fn () {
                        task::sleep(1);
                        panic "boom";
                        0
                    },
                );
            0
        },
    );
    let msg = match r {
        Result::Ok(_) => "no error",
        Result::Err(e) => match e {
            TaskError::Panicked(m) => m,
            default => "other error",
        },
    };
    assert(msg == "boom")?;
    assert(log.len() == 0)?;
}

test("joining a panicked child reports it") {
    let seen: Vec<string> = Vec::new();
    let r = scope(
        fn (Scope s) use (seen) {
            let bad = s
                .spawn(
                    fn () {
                        panic "bad child";
                        0
                    },
                );
            match bad.join() {
                Result::Ok(_) => seen.push("ok"),
                Result::Err(e) => match e {
                    TaskError::Panicked(m) => seen.push(m),
                    default => seen.push("other"),
                },
            }
            0
        },
    );
    assert(seen.len() == 1 && seen[0] == "bad child")?;
    let failed = match r {
        Result::Ok(_) => false,
        Result::Err(_) => true,
    };
    assert(failed)?;
}

test("clock::sleep_ms inside a scope suspends the task") {
    let log: Vec<int> = Vec::new();
    let r = scope(
        fn (Scope s) use (log) {
            s
                .spawn(
                    fn () use (log) {
                        sleep_ms(30);
                        log.push(2);
                        0
                    },
                );
            s.spawn(fn () use (log) {
                log.push(1);
                0
            });
            0
        },
    );
    assert(ok_or(r, -1) == 0)?;
    assert(log[0] == 1 && log[1] == 2)?;
}

test("many tasks") {
    let total: Vec<int> = Vec::new();
    let r = scope(
        fn (Scope s) use (total) {
            let i = 0;
            while i < 200 {
                let k = i;
                s
                    .spawn(
                        fn () use (total, k) {
                            task::yield_now();
                            total.push(k);
                            0
                        },
                    );
                i += 1;
            }
            0
        },
    );
    assert(ok_or(r, -1) == 0)?;
    assert(total.len() == 200)?;
}
