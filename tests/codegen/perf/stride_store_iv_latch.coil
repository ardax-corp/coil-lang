// The rotated store loop's latch, `k = k + p; while k < n` after a
// `DenseStoreIndex`, adds and compares inline (#2012): it still stops at
// `n`, and an IV bump past int::MAX still panics with `integer overflow`.
use tests::assert;
use task::{scope, Scope, TaskError};
use string::format;

fn message(Result<int, TaskError> r) -> string {
    return match r {
        Result::Ok(v) => format("ok %i", v),
        Result::Err(TaskError::Panicked(m)) => m,
        Result::Err(_) => "other",
    };
}

fn big() -> int {
    return 9223372036854775807;
}

// The loop `fill` runs: store, then the IV bump and its compare.
fn clear(Vec<int> flags, int k, int p, int n) {
    while k < n {
        flags[k] = 0;
        k = k + p;
    }
}

fn fill(int n, int p) -> int {
    let flags: Vec<int> = [1, 1, 1, 1, 1, 1, 1, 1, 1, 1];
    clear(flags, p, p, n);
    let left = 0;
    for f in flags {
        left = left + f;
    }
    return left;
}

test("the latch stops at n") {
    assert!(fill(10, 3) == 7);
    assert!(fill(10, 1) == 1);
    assert!(fill(10, 20) == 10);
}

test("an IV bump past int::MAX panics") {
    let r = scope(fn (Scope s) {
        let _ = s.spawn(fn () {
            // 1 + int::MAX: flags[1] is stored, then the bump overflows.
            let flags: Vec<int> = [1, 1];
            clear(flags, 1, big(), big());
            flags[1]
        });
        0
    });
    assert!(message(r) == "integer overflow");
}
