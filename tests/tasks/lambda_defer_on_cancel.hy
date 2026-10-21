// A `defer` written directly in a spawned lambda runs when the task is
// cancelled, as one in a function the task calls does (#1194).
use tests::assert;
use task::{scope, Scope, TaskError};

static let LOG: int = 0;

fn push(int d) {
    LOG = LOG * 10 + d;
}

fn cancelled(Result<int, TaskError> r) -> bool {
    return match r {
        Result::Err(TaskError::Cancelled) => true,
        default => false,
    };
}

test("a lambda's defer runs on cancel") {
    LOG = 0;
    let r = scope(
        fn (Scope s) {
            let victim = s.spawn(fn () {
                defer {
                    push(6);
                }
                push(1);
                task::sleep(10000);
                push(9);
                0
            });
            task::yield_now();
            victim.cancel();
            cancelled(victim.join())
        },
    );
    assert!(r == Result::Ok(true));
    assert!(LOG == 16);
}

test("a lambda's defer reads its captures on cancel") {
    LOG = 0;
    let d = 7;
    let r = scope(
        fn (Scope s) use (d) {
            let victim = s.spawn(fn () use (d) {
                defer use (d) {
                    push(d);
                }
                push(2);
                task::sleep(10000);
                0
            });
            task::yield_now();
            victim.cancel();
            cancelled(victim.join())
        },
    );
    assert!(r == Result::Ok(true));
    assert!(LOG == 27);
}

test("a lambda's defer runs once when the task finishes") {
    LOG = 0;
    let r = scope(
        fn (Scope s) {
            let t = s.spawn(fn () {
                defer {
                    push(6);
                }
                push(1);
                task::sleep(1);
                3
            });
            t.join()
        },
    );
    assert!(r == Result::Ok(Result::Ok(3)));
    assert!(LOG == 16);
}
